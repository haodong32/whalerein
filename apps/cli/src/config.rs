//! Client-environment configuration adapter.
//!
//! The CLI only maps arguments to Core's typed patches and renders Core's typed
//! view/import results. Path selection, TOML parsing, catalog authority, merge
//! rules and durable writes stay in `whalerein-core`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use whalerein_core::api::{
    commit_import, default_config_path, legacy_state_present, preview_import,
    BuiltinSpecialization, ConfigOpenOptions, ConfigPatch, ConfigSnapshot, ConfigSourceKind,
    ConfigView, Configuration, ConfigurationError, CustomEnvironmentInput, DefinitionSource,
    EnvironmentMode, ImportCommit, ImportCommitOptions, ImportOutcome, ImportPreview,
    ImportPreviewOptions, ImportPreviewStatus, LegacySourceKind, SecretStore, SelectionSource,
    TlsConfig, TlsConfiguration, TlsProvider, TrustResolver,
};

use crate::cli::{
    Cli, Command, ConfigAction, ConfigCaProvider, ConfigEnvironmentMode, ConfigRemoveAction,
    ConfigSetAction,
};
use crate::output::{CliError, CliOutcome};

/// Open the one selected configuration file.
///
/// `config show`/`check` and every import preview are inspection-only; ordinary
/// startup may initialize a missing default file when Core's legacy probe finds
/// no legacy state.
pub(crate) fn open(
    cli: &Cli,
    command: &Command,
) -> Result<ConfigSnapshot, whalerein_core::CoreError> {
    let inspection = matches!(
        command,
        Command::Config {
            action: ConfigAction::Show | ConfigAction::Check
        }
    );
    let mut options = if inspection {
        ConfigOpenOptions::inspection()
    } else {
        ConfigOpenOptions::startup()
    };
    options.explicit_path = cli.config.as_deref();
    let probe = legacy_state_present;
    options.legacy_state_probe = Some(&probe);
    Configuration::open(&options)
}

/// Execute `config show`, `check`, `set` or `remove`.
pub(crate) fn run(
    cli: &Cli,
    command: &Command,
    action: &ConfigAction,
    snapshot: &ConfigSnapshot,
) -> Result<CliOutcome, whalerein_core::CoreError> {
    match action {
        ConfigAction::Show => Ok(present_view(
            command,
            snapshot,
            cli.environment.as_deref(),
            false,
        )),
        ConfigAction::Check => Ok(present_view(
            command,
            snapshot,
            cli.environment.as_deref(),
            true,
        )),
        ConfigAction::Set { action } => {
            let patch = patch_for(action)?;
            let updated = snapshot.update(&patch, snapshot.revision())?;
            let view = updated.view(None);
            let human = format!(
                "Configuration updated: {}\n{}",
                patch_label(action),
                render_view(&view)
            );
            Ok(CliOutcome::success(command, view_data(&view), human))
        }
        ConfigAction::Remove { action } => {
            let ConfigRemoveAction::Environment { name } = action;
            let patch = ConfigPatch::RemoveCustomEnvironment {
                environment: name.clone(),
            };
            let updated = snapshot.update(&patch, snapshot.revision())?;
            let view = updated.view(None);
            let human = format!("Removed custom environment {name}.\n{}", render_view(&view));
            Ok(CliOutcome::success(command, view_data(&view), human))
        }
        ConfigAction::Import { .. } => Err(whalerein_core::CoreError::Internal(
            "config import is dispatched separately".into(),
        )),
    }
}

/// Preview or confirm legacy adoption. `--confirm` requires `--yes`; there is no
/// interactive prompt in this path.
pub(crate) fn run_import(
    cli: &Cli,
    command: &Command,
    action: &ConfigAction,
    data_dir: &Path,
    _secrets: &Arc<dyn SecretStore>,
) -> Result<CliOutcome, whalerein_core::CoreError> {
    let ConfigAction::Import { source, confirm } = action else {
        return Err(whalerein_core::CoreError::Internal(
            "run_import received a non-import action".into(),
        ));
    };
    let (target_path, target_revision) = import_target(cli)?;
    let required_kind = source.as_deref().map(parse_source_kind).transpose()?;
    let preview = build_preview(
        data_dir,
        &target_path,
        target_revision.as_deref(),
        required_kind,
    )?;

    let Some(expected) = confirm else {
        return Ok(present_preview(command, &preview));
    };
    if !cli.yes {
        return Err(ConfigurationError::InvalidPatch {
            message: "--confirm requires --yes; previews are never confirmed interactively".into(),
        }
        .into());
    }
    if preview.preview_id.as_str() != expected {
        return Err(ConfigurationError::ImportConflict {
            message: "the legacy source or configuration target changed after the preview; run preview again"
                .into(),
        }
        .into());
    }
    let outcome = commit_import(
        &ImportCommit::from_preview(&preview),
        &ImportCommitOptions {
            secure_store_available: true,
        },
    )?;
    Ok(present_outcome(command, &outcome))
}

fn import_target(cli: &Cli) -> Result<(PathBuf, Option<String>), whalerein_core::CoreError> {
    let mut options = ConfigOpenOptions::inspection();
    options.explicit_path = cli.config.as_deref();
    match Configuration::open(&options) {
        Ok(snapshot) => Ok((
            snapshot.source().path.clone(),
            snapshot.revision().map(str::to_string),
        )),
        Err(whalerein_core::CoreError::Configuration(error))
            if error.code() == "missing_config"
                && cli.config.is_none()
                && std::env::var_os("WHALEREIN_CONFIG").is_none_or(|value| value.is_empty()) =>
        {
            // Explicit and environment-selected paths must fail. Only the
            // documented default target may be previewed before it exists.
            Ok((
                default_config_path().map_err(whalerein_core::CoreError::from)?,
                None,
            ))
        }
        Err(error) => Err(error),
    }
}

fn build_preview(
    data_dir: &Path,
    target_path: &Path,
    target_revision: Option<&str>,
    required_kind: Option<LegacySourceKind>,
) -> Result<ImportPreview, whalerein_core::CoreError> {
    let options = ImportPreviewOptions {
        state_root: data_dir,
        target_path,
        target_revision,
        secure_store_available: true,
    };
    let preview = preview_import(&options)?;
    if required_kind.is_some_and(|kind| kind != preview.source.kind) {
        return Err(ConfigurationError::ImportUnsupported {
            message: format!(
                "the detected legacy source is {}, not the requested kind",
                source_kind_name(preview.source.kind)
            ),
        }
        .into());
    }
    Ok(preview)
}

fn parse_source_kind(value: &str) -> Result<LegacySourceKind, whalerein_core::CoreError> {
    match value {
        "core-flat" => Ok(LegacySourceKind::CoreFlat),
        "core-profiles" => Ok(LegacySourceKind::CoreProfiles),
        "cordis-managed" => Ok(LegacySourceKind::CordisManaged),
        "cordis-standalone" => Ok(LegacySourceKind::CordisStandalone),
        "prototype" => Ok(LegacySourceKind::Prototype),
        other => Err(whalerein_core::CoreError::InvalidInput(format!(
            "unknown import source {other}; expected core-flat, core-profiles, cordis-managed, cordis-standalone or prototype"
        ))),
    }
}

fn source_kind_name(kind: LegacySourceKind) -> &'static str {
    match kind {
        LegacySourceKind::CoreFlat => "core-flat",
        LegacySourceKind::CoreProfiles => "core-profiles",
        LegacySourceKind::CordisManaged => "cordis-managed",
        LegacySourceKind::CordisStandalone => "cordis-standalone",
        LegacySourceKind::Prototype => "prototype",
    }
}

fn patch_for(action: &ConfigSetAction) -> Result<ConfigPatch, whalerein_core::CoreError> {
    match action {
        ConfigSetAction::Default { name, clear } => match (name, clear) {
            (Some(name), false) => Ok(ConfigPatch::SetDefaultEnvironment {
                environment: Some(name.clone()),
            }),
            (None, true) => Ok(ConfigPatch::SetDefaultEnvironment { environment: None }),
            _ => Err(whalerein_core::CoreError::InvalidInput(
                "set default requires exactly one of <NAME> or --clear".into(),
            )),
        },
        ConfigSetAction::Environment {
            name,
            mode,
            api_origin,
            issuer,
            native_client_id,
            label,
            ca_provider,
            ca_file,
        } => Ok(ConfigPatch::UpsertCustomEnvironment(
            CustomEnvironmentInput {
                environment: name.clone(),
                label: label.clone(),
                mode: mode_for(*mode),
                api_origin: api_origin.clone(),
                issuer: issuer.clone(),
                native_client_id: native_client_id.clone(),
                tls: tls_for(*ca_provider, ca_file.as_deref())?,
            },
        )),
        ConfigSetAction::Builtin {
            name,
            label,
            clear_label,
            ca_provider,
            ca_file,
            clear_tls,
        } => {
            if *clear_tls && (ca_provider.is_some() || ca_file.is_some()) {
                return Err(whalerein_core::CoreError::InvalidInput(
                    "--clear-tls cannot be combined with --ca-provider or --ca-file".into(),
                ));
            }
            Ok(ConfigPatch::SpecializeBuiltin(BuiltinSpecialization {
                environment: name.clone(),
                label: label.clone(),
                clear_label: *clear_label,
                tls: tls_for(*ca_provider, ca_file.as_deref())?,
                clear_tls: *clear_tls,
            }))
        }
    }
}

fn mode_for(mode: ConfigEnvironmentMode) -> EnvironmentMode {
    match mode {
        ConfigEnvironmentMode::Account => EnvironmentMode::Account,
        ConfigEnvironmentMode::Standalone => EnvironmentMode::Standalone,
    }
}

fn tls_for(
    provider: Option<ConfigCaProvider>,
    ca_file: Option<&str>,
) -> Result<Option<TlsConfiguration>, whalerein_core::CoreError> {
    match (provider, ca_file) {
        (None, None) => Ok(None),
        (None, Some(path)) => Ok(Some(TlsConfiguration::File {
            ca_file: path.to_owned(),
        })),
        (Some(ConfigCaProvider::File), Some(path)) => Ok(Some(TlsConfiguration::File {
            ca_file: path.to_owned(),
        })),
        (Some(ConfigCaProvider::File), None) => Err(whalerein_core::CoreError::InvalidInput(
            "--ca-provider file requires --ca-file PATH".into(),
        )),
        (Some(ConfigCaProvider::System), None) => Ok(Some(TlsConfiguration::System)),
        (Some(ConfigCaProvider::Mkcert), None) => Ok(Some(TlsConfiguration::Mkcert)),
        (Some(_), Some(_)) => Err(whalerein_core::CoreError::InvalidInput(
            "--ca-file requires --ca-provider file".into(),
        )),
    }
}

fn patch_label(action: &ConfigSetAction) -> &'static str {
    match action {
        ConfigSetAction::Default { .. } => "default environment",
        ConfigSetAction::Environment { .. } => "environment",
        ConfigSetAction::Builtin { .. } => "built-in environment",
    }
}

fn view_data(view: &ConfigView) -> Value {
    serde_json::to_value(view).unwrap_or_else(|_| json!({"path": view.path}))
}

fn present_view(
    command: &Command,
    snapshot: &ConfigSnapshot,
    environment: Option<&str>,
    check: bool,
) -> CliOutcome {
    let mut view = snapshot.view(environment);
    if check && view.errors.is_empty() {
        if let Some(definition) = view.definition.as_ref() {
            // Offline trust validation: read the selected public CA bundle.
            // This never installs trust or claims live connectivity.
            if let Err(error) = TrustResolver::new(&view.path).resolve_definition(definition) {
                view.errors
                    .push(whalerein_core::api::ConfigErrorView::from(&error));
            }
        }
    }
    let human = render_view(&view);
    let command_name = match &command {
        Command::Config { action } => action.name(),
        _ => command.name(),
    };
    if check && !view.errors.is_empty() {
        let first = &view.errors[0];
        return CliOutcome {
            command: command_name.to_owned(),
            data: Some(view_data(&view)),
            error: Some(CliError::new(first.code.clone(), first.message.clone())),
            human,
            exit: 2,
        };
    }
    CliOutcome {
        command: command_name.to_owned(),
        data: Some(view_data(&view)),
        error: None,
        human,
        exit: 0,
    }
}

fn render_view(view: &ConfigView) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Configuration file: {}", view.path.display());
    let _ = writeln!(out, "Source: {}", source_kind_label(view.source_kind));
    let _ = writeln!(
        out,
        "Revision: {}",
        view.revision.as_deref().unwrap_or("none")
    );
    let _ = writeln!(
        out,
        "Desired default: {}",
        view.desired_default.as_deref().unwrap_or("none")
    );
    let _ = writeln!(
        out,
        "Invocation override: {}",
        view.effective_override.as_deref().unwrap_or("none")
    );
    match &view.selected_environment {
        Some(environment) => {
            let _ = writeln!(out, "Selected environment: {environment}");
            let _ = writeln!(
                out,
                "Selection source: {}",
                selection_source_label(view.selection_source)
            );
            let _ = writeln!(
                out,
                "Definition source: {}",
                definition_source_label(view.definition_source)
            );
        }
        None => {
            let _ = writeln!(out, "Selected environment: unresolved");
        }
    }
    if let Some(definition) = &view.definition {
        let _ = writeln!(out, "Definition id: {}", definition.id);
        let _ = writeln!(
            out,
            "Label: {}",
            definition.label.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            out,
            "Mode: {}",
            definition
                .mode
                .map(EnvironmentMode::as_str)
                .unwrap_or("none")
        );
        let _ = writeln!(
            out,
            "Environment token: {}",
            definition.environment.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            out,
            "API origin: {}",
            definition.api_origin.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            out,
            "Issuer: {}",
            definition.issuer.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            out,
            "Native client id: {}",
            definition.native_client_id.as_deref().unwrap_or("none")
        );
        if let Some(tls) = &definition.tls {
            let _ = writeln!(out, "TLS: {}", tls_label(tls));
        }
    }
    if !view.field_provenance.is_empty() {
        out.push_str("Field provenance:\n");
        for (field, source) in &view.field_provenance {
            let _ = writeln!(out, "  {field}: {}", field_source_label(*source));
        }
    }
    let _ = writeln!(out, "Validation: offline only");
    let _ = writeln!(out, "Live readiness: not checked");
    for error in &view.errors {
        let _ = writeln!(out, "Error: {} ({})", error.message, error.code);
    }
    out
}

fn source_kind_label(kind: ConfigSourceKind) -> &'static str {
    match kind {
        ConfigSourceKind::Explicit => "explicit --config",
        ConfigSourceKind::Environment => "WHALEREIN_CONFIG",
        ConfigSourceKind::Default => "default",
    }
}

fn selection_source_label(source: Option<SelectionSource>) -> &'static str {
    match source {
        Some(SelectionSource::Invocation) => "invocation override (not persisted)",
        Some(SelectionSource::File) => "configuration file",
        Some(SelectionSource::Default) => "built-in default",
        None => "unresolved",
    }
}

fn definition_source_label(source: Option<DefinitionSource>) -> &'static str {
    match source {
        Some(DefinitionSource::Builtin) => "built-in catalog",
        Some(DefinitionSource::File) => "configuration file",
        None => "unresolved",
    }
}

fn field_source_label(source: whalerein_core::api::FieldSource) -> &'static str {
    use whalerein_core::api::FieldSource;
    match source {
        FieldSource::Catalog => "catalog",
        FieldSource::File => "file",
        FieldSource::Default => "default",
    }
}

fn tls_label(tls: &TlsConfig) -> String {
    let provider = match tls.ca_provider {
        TlsProvider::System => "system",
        TlsProvider::Mkcert => "mkcert",
        TlsProvider::File => "file",
    };
    match &tls.ca_file {
        Some(path) => format!("{provider} ({path})"),
        None => provider.to_owned(),
    }
}

fn present_preview(command: &Command, preview: &ImportPreview) -> CliOutcome {
    let human = render_preview(preview);
    let data = serde_json::to_value(preview).unwrap_or_else(|_| json!({}));
    CliOutcome::success(command, data, human)
}

fn present_outcome(command: &Command, outcome: &ImportOutcome) -> CliOutcome {
    let human = format!(
        "Import status: {}\nTarget: {}\nRevision: {}\nJournal: {}\n",
        outcome_status_label(outcome),
        outcome.target_path.display(),
        outcome.target_revision.as_deref().unwrap_or("none"),
        journal_state_label(outcome.journal_state),
    );
    let data = serde_json::to_value(outcome).unwrap_or_else(|_| json!({}));
    CliOutcome::success(command, data, human)
}

fn outcome_status_label(outcome: &ImportOutcome) -> &'static str {
    use whalerein_core::api::ImportOutcomeStatus;
    match outcome.status {
        ImportOutcomeStatus::Imported => "imported",
        ImportOutcomeStatus::AlreadyApplied => "already applied",
    }
}

fn render_preview(preview: &ImportPreview) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Preview id: {}", preview.preview_id);
    let _ = writeln!(out, "Status: {}", preview_status_label(preview.status));
    let _ = writeln!(
        out,
        "Source: {} ({})",
        source_kind_name(preview.source.kind),
        preview.source.root.display()
    );
    let _ = writeln!(out, "Source revision: {}", preview.source_revision);
    let _ = writeln!(
        out,
        "Target: {} (revision {})",
        preview.target_path.display(),
        preview.target_revision.as_deref().unwrap_or("none")
    );
    if let Some(proposed) = &preview.proposed {
        let _ = writeln!(
            out,
            "Proposed default: {}",
            proposed.default_environment.as_deref().unwrap_or("none")
        );
        for environment in &proposed.environments {
            let _ = writeln!(
                out,
                "Environment: {} ({}) label={} mode={}",
                environment.name,
                definition_source_label(Some(environment.definition_source)),
                environment.label.as_deref().unwrap_or("none"),
                environment
                    .mode
                    .map(EnvironmentMode::as_str)
                    .unwrap_or("none")
            );
            if let Some(tls) = &environment.tls {
                let _ = writeln!(out, "  TLS: {}", tls_label(tls));
            }
        }
    }
    if !preview.retained_references.is_empty() {
        out.push_str("Retained references:\n");
        for reference in &preview.retained_references {
            let _ = writeln!(out, "  {}", reference_label(reference));
        }
    }
    if let Some(handoff) = &preview.secure_store_handoff {
        let _ = writeln!(
            out,
            "Secure-store handoff: required={} satisfied={} namespaces={}",
            handoff.required,
            handoff.satisfied,
            handoff.namespaces.join(",")
        );
    }
    for conflict in &preview.conflicts {
        let _ = writeln!(out, "Conflict: {} ({})", conflict.message, conflict.code);
    }
    out
}

fn preview_status_label(status: ImportPreviewStatus) -> &'static str {
    match status {
        ImportPreviewStatus::Ready => "ready",
        ImportPreviewStatus::RetainOnly => "retain-only",
        ImportPreviewStatus::Blocked => "blocked",
        ImportPreviewStatus::Conflict => "conflict",
    }
}

fn reference_label(reference: &whalerein_core::api::RetainedReference) -> String {
    use whalerein_core::api::RetainedReference;
    match reference {
        RetainedReference::StatePath { path } => format!("state path {}", path.display()),
        RetainedReference::ProfileId { profile_id } => format!("profile id {profile_id}"),
        RetainedReference::CredentialNamespace { namespace } => {
            format!("credential namespace {namespace}")
        }
        RetainedReference::SecretSlot { slot } => format!("secret slot {slot}"),
        RetainedReference::PendingJournal { path } => format!("pending journal {}", path.display()),
        RetainedReference::TrustFile { path } => format!("trust file {}", path.display()),
    }
}

fn journal_state_label(state: whalerein_core::api::ImportJournalState) -> &'static str {
    use whalerein_core::api::ImportJournalState;
    match state {
        ImportJournalState::Previewed => "previewed",
        ImportJournalState::Prepared => "prepared",
        ImportJournalState::Published => "published",
        ImportJournalState::Activated => "activated",
        ImportJournalState::Complete => "complete",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    fn action(args: &[&str]) -> ConfigAction {
        cli(args).command_action()
    }

    trait CommandAction {
        fn command_action(&self) -> ConfigAction;
    }

    impl CommandAction for Cli {
        fn command_action(&self) -> ConfigAction {
            match &self.command {
                Command::Config { action } => action.clone(),
                _ => panic!("expected config command"),
            }
        }
    }

    #[test]
    fn set_default_maps_to_the_typed_patch() {
        assert_eq!(
            patch_for(&ConfigSetAction::Default {
                name: Some("company".into()),
                clear: false
            })
            .unwrap(),
            ConfigPatch::SetDefaultEnvironment {
                environment: Some("company".into())
            }
        );
        assert_eq!(
            patch_for(&ConfigSetAction::Default {
                name: None,
                clear: true
            })
            .unwrap(),
            ConfigPatch::SetDefaultEnvironment { environment: None }
        );
        assert!(patch_for(&ConfigSetAction::Default {
            name: None,
            clear: false
        })
        .is_err());
    }

    #[test]
    fn set_environment_maps_modes_and_tls_without_cli_policy() {
        let patch = patch_for(&ConfigSetAction::Environment {
            name: "company".into(),
            mode: ConfigEnvironmentMode::Account,
            api_origin: Some("https://cloud.example.com".into()),
            issuer: Some("https://auth.example.com/oidc".into()),
            native_client_id: Some("whalerein-cli".into()),
            label: Some("Company Cloud".into()),
            ca_provider: Some(ConfigCaProvider::File),
            ca_file: Some("~/ca.pem".into()),
        })
        .unwrap();
        let ConfigPatch::UpsertCustomEnvironment(input) = patch else {
            panic!("wrong patch");
        };
        assert_eq!(input.environment, "company");
        assert_eq!(input.mode, EnvironmentMode::Account);
        assert_eq!(
            input.tls,
            Some(TlsConfiguration::File {
                ca_file: "~/ca.pem".into()
            })
        );
        assert_eq!(
            patch_for(&ConfigSetAction::Environment {
                name: "lab".into(),
                mode: ConfigEnvironmentMode::Standalone,
                api_origin: None,
                issuer: None,
                native_client_id: None,
                label: None,
                ca_provider: None,
                ca_file: None,
            })
            .unwrap(),
            ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
                environment: "lab".into(),
                label: None,
                mode: EnvironmentMode::Standalone,
                api_origin: None,
                issuer: None,
                native_client_id: None,
                tls: None,
            })
        );
    }

    #[test]
    fn tls_arguments_are_explicit_about_the_file_provider() {
        assert_eq!(tls_for(None, None).unwrap(), None);
        assert_eq!(
            tls_for(None, Some("ca.pem")).unwrap(),
            Some(TlsConfiguration::File {
                ca_file: "ca.pem".into()
            })
        );
        assert_eq!(
            tls_for(Some(ConfigCaProvider::System), None).unwrap(),
            Some(TlsConfiguration::System)
        );
        assert_eq!(
            tls_for(Some(ConfigCaProvider::Mkcert), None).unwrap(),
            Some(TlsConfiguration::Mkcert)
        );
        assert!(tls_for(Some(ConfigCaProvider::File), None).is_err());
        assert!(tls_for(Some(ConfigCaProvider::System), Some("ca.pem")).is_err());
    }

    #[test]
    fn builtin_specialization_rejects_clear_with_ca_arguments() {
        let err = patch_for(&ConfigSetAction::Builtin {
            name: "production".into(),
            label: Some("Prod".into()),
            clear_label: false,
            ca_provider: Some(ConfigCaProvider::System),
            ca_file: None,
            clear_tls: true,
        })
        .unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
    }

    #[test]
    fn import_source_names_are_checked_without_toml_policy() {
        assert_eq!(
            parse_source_kind("core-flat").unwrap(),
            LegacySourceKind::CoreFlat
        );
        assert_eq!(
            parse_source_kind("core-profiles").unwrap(),
            LegacySourceKind::CoreProfiles
        );
        assert_eq!(
            parse_source_kind("cordis-managed").unwrap(),
            LegacySourceKind::CordisManaged
        );
        assert_eq!(
            parse_source_kind("cordis-standalone").unwrap(),
            LegacySourceKind::CordisStandalone
        );
        assert_eq!(
            parse_source_kind("prototype").unwrap(),
            LegacySourceKind::Prototype
        );
        assert!(parse_source_kind("prototype-local").is_err());
        assert_eq!(source_kind_name(LegacySourceKind::CoreFlat), "core-flat");
    }

    #[test]
    fn human_view_explains_sources_offline_scope_and_errors() {
        let view = ConfigView {
            path: "/tmp/config.toml".into(),
            source_kind: ConfigSourceKind::Explicit,
            revision: Some("sha256:aa".into()),
            selected_environment: Some("company".into()),
            selection_source: Some(SelectionSource::File),
            definition_source: Some(DefinitionSource::File),
            desired_default: Some("company".into()),
            effective_override: Some("lab".into()),
            definition: Some(whalerein_core::api::EnvironmentDefinition {
                id: "company".into(),
                mode: Some(EnvironmentMode::Account),
                label: Some("Company".into()),
                api_origin: Some("https://cloud.example.com".into()),
                issuer: Some("https://auth.example.com/oidc".into()),
                native_client_id: Some("whalerein-cli".into()),
                environment: None,
                tls: Some(TlsConfig {
                    ca_provider: TlsProvider::System,
                    ca_file: None,
                }),
            }),
            field_provenance: [("label".to_owned(), whalerein_core::api::FieldSource::File)].into(),
            validation: whalerein_core::api::ConfigValidation {
                scope: whalerein_core::api::ValidationScope::Offline,
                live_readiness: whalerein_core::api::LiveReadiness::NotChecked,
            },
            errors: vec![whalerein_core::api::ConfigErrorView {
                code: "unknown_environment".into(),
                message: "unknown environment".into(),
                environment: Some("missing".into()),
            }],
        };
        let human = render_view(&view);
        assert!(human.contains("explicit --config"));
        assert!(human.contains("Selected environment: company"));
        assert!(human.contains("Validation: offline only"));
        assert!(human.contains("Live readiness: not checked"));
        assert!(human.contains("Error: unknown environment (unknown_environment)"));
    }

    #[test]
    fn check_turns_view_errors_into_a_stable_nonzero_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "version = 1\nenvironment = \"missing\"\n").unwrap();
        let parsed = cli(&[
            "whalerein",
            "--config",
            path.to_str().unwrap(),
            "config",
            "check",
        ]);
        let snapshot = open(&parsed, &parsed.command).unwrap();
        let check_action = action(&[
            "whalerein",
            "--config",
            path.to_str().unwrap(),
            "config",
            "check",
        ]);
        let outcome = run(&parsed, &parsed.command, &check_action, &snapshot).unwrap();
        assert_eq!(outcome.exit, 2);
        assert_eq!(outcome.to_json()["command"], "config.check");
        assert_eq!(outcome.to_json()["error"]["code"], "unknown_environment");
        assert_eq!(
            outcome.to_json()["data"]["errors"][0]["code"],
            "unknown_environment"
        );
    }
}
