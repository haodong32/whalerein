use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;

use crate::api::CoreError;

use super::document::{self, CONFIG_CORPUS_JSON};
use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvScope {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_config: Option<OsString>,
    previous_xdg_data: Option<OsString>,
    previous_localappdata: Option<OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl EnvScope {
    fn new(home: Option<&Path>, config: Option<&Path>) -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_config = std::env::var_os("WHALEREIN_CONFIG");
        let previous_xdg_data = std::env::var_os("XDG_DATA_HOME");
        let previous_localappdata = std::env::var_os("LOCALAPPDATA");
        if let Some(home) = home {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        if let Some(config) = config {
            std::env::set_var("WHALEREIN_CONFIG", config);
        } else {
            std::env::remove_var("WHALEREIN_CONFIG");
        }
        Self {
            previous_home,
            previous_userprofile,
            previous_config,
            previous_xdg_data,
            previous_localappdata,
            _guard: guard,
        }
    }

    fn with_data_home(self, data: &Path) -> Self {
        std::env::set_var("XDG_DATA_HOME", data);
        std::env::set_var("LOCALAPPDATA", data);
        self
    }

    fn without_home(self) -> Self {
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("LOCALAPPDATA");
        self
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, previous) in [
            ("HOME", &self.previous_home),
            ("USERPROFILE", &self.previous_userprofile),
            ("WHALEREIN_CONFIG", &self.previous_config),
            ("XDG_DATA_HOME", &self.previous_xdg_data),
            ("LOCALAPPDATA", &self.previous_localappdata),
        ] {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn contract_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("contracts")
        .join("client-environment")
}

fn open_path(path: &Path) -> CoreResult<Configuration> {
    Configuration::open(&ConfigOpenOptions {
        explicit_path: Some(path),
        ..ConfigOpenOptions::default()
    })
}

fn write_config(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

fn open_text(text: &str) -> (tempfile::TempDir, Configuration) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), text);
    let configuration = open_path(&path).unwrap();
    (dir, configuration)
}

fn code(error: &CoreError) -> &'static str {
    error.code()
}

fn custom_account(name: &str) -> String {
    format!(
        "version = 1\nenvironment = \"{name}\"\n\n[environments.{name}]\n\
         label = \"Custom\"\nmode = \"account\"\n\
         api_origin = \"https://cloud.example.com\"\n\
         issuer = \"https://auth.example.com/oidc\"\n\
         native_client_id = \"whalerein-cli\"\n"
    )
}

// ---------------------------------------------------------------------------
// Corpus and packaging parity
// ---------------------------------------------------------------------------

#[test]
fn config_env_ac_05_corpus_parity_matches_every_case_and_resolution() {
    let corpus: serde_json::Value = serde_json::from_str(CONFIG_CORPUS_JSON).unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let file = case["file"].as_str().unwrap();
        let text = std::fs::read_to_string(contract_root().join(file)).unwrap();
        let id = case["id"].as_str().unwrap();
        match case["kind"].as_str().unwrap() {
            "valid" => {
                let (parsed, _) = document::parse_configuration(&text)
                    .unwrap_or_else(|error| panic!("{id}: {}", error.code()));
                assert_eq!(
                    serde_json::to_value(&parsed).unwrap(),
                    case["document"],
                    "{id}"
                );
            }
            "invalid" => {
                let error = document::parse_configuration(&text).unwrap_err();
                assert_eq!(error.code(), case["error"].as_str().unwrap(), "{id}");
            }
            "resolve" => {
                let path = contract_root().join(file);
                let configuration = open_path(&path).unwrap();
                let error = resolve_environment(&configuration, None).unwrap_err();
                assert_eq!(error.code(), case["error"].as_str().unwrap(), "{id}");
            }
            other => panic!("unknown corpus kind {other}"),
        }
    }
    for entry in corpus["resolution"].as_array().unwrap() {
        let path = contract_root().join(entry["file"].as_str().unwrap());
        let configuration = open_path(&path).unwrap();
        let override_name = entry["override"].as_str();
        match resolve_environment(&configuration, override_name) {
            Ok(resolution) => {
                assert_eq!(
                    serde_json::to_value(&resolution).unwrap(),
                    entry["expect"],
                    "{}",
                    entry["id"]
                );
            }
            Err(error) => {
                assert_eq!(
                    error.code(),
                    entry["error"].as_str().unwrap(),
                    "{}",
                    entry["id"]
                );
            }
        }
    }
}

#[test]
fn config_env_ac_02_embedded_catalog_and_corpus_match_canonical_assets() {
    let catalog = std::fs::read_to_string(contract_root().join("catalog.json")).unwrap();
    let corpus = std::fs::read_to_string(contract_root().join("cases.json")).unwrap();
    assert_eq!(CONFIG_CATALOG_JSON, catalog);
    assert_eq!(CONFIG_CORPUS_JSON, corpus);
    let parsed = document::catalog().expect("embedded catalog validates");
    for name in document::BUILTIN_NAMES {
        let entry = parsed.get(name).expect("built-in");
        assert_eq!(entry.environment, name);
        assert_eq!(entry.mode, "account");
    }
    assert_eq!(parsed.get("local").unwrap().tls.ca_provider, "mkcert");
    assert_eq!(parsed.get("production").unwrap().tls.ca_provider, "system");
}

// ---------------------------------------------------------------------------
// Path selection and initialization
// ---------------------------------------------------------------------------

#[test]
fn config_env_ac_03_fresh_default_is_initialized_atomically_with_minimal_content() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None);
    let configuration = Configuration::open(&ConfigOpenOptions::startup()).unwrap();
    assert_eq!(configuration.source().kind, ConfigSourceKind::Default);
    assert!(configuration.source().initialized);
    assert_eq!(
        configuration.source().path,
        std::fs::canonicalize(home.path().join(".whalerein/config.toml")).unwrap()
    );
    let bytes = std::fs::read(home.path().join(".whalerein/config.toml")).unwrap();
    assert_eq!(bytes, b"version = 1\nenvironment = \"production\"\n");
    assert_eq!(
        resolve_environment(&configuration, None)
            .unwrap()
            .environment,
        "production"
    );
    assert!(std::fs::read_dir(home.path().join(".whalerein"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp-")));
}

#[test]
fn config_env_ac_03_help_and_inspection_never_write_or_initialize() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None);
    let error = Configuration::open(&ConfigOpenOptions::inspection()).unwrap_err();
    assert_eq!(code(&error), "missing_config");
    assert!(!home.path().join(".whalerein/config.toml").exists());
    assert!(!home.path().join(".whalerein").exists());

    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let before = std::fs::read(&path).unwrap();
    let configuration = open_path(&path).unwrap();
    assert_eq!(configuration.source().kind, ConfigSourceKind::Explicit);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn config_env_ac_03_legacy_state_suppresses_default_initialization() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None);
    let probe = |_path: &Path| true;
    let options = ConfigOpenOptions {
        allow_default_init: true,
        legacy_state_probe: Some(&probe),
        ..ConfigOpenOptions::default()
    };
    let error = Configuration::open(&options).unwrap_err();
    assert_eq!(code(&error), "missing_config");
    assert!(!home.path().join(".whalerein/config.toml").exists());
}

#[test]
fn config_env_ac_03_explicit_path_beats_environment_which_beats_default() {
    let home = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let env_path = write_config(first.path(), "version = 1\n");
    let explicit_path = write_config(second.path(), "version = 1\nenvironment = \"staging\"\n");
    let _env = EnvScope::new(Some(home.path()), Some(&env_path));

    let configuration = open_path(&explicit_path).unwrap();
    assert_eq!(configuration.source().kind, ConfigSourceKind::Explicit);
    assert_eq!(
        resolve_environment(&configuration, None)
            .unwrap()
            .environment,
        "staging"
    );

    let configuration = Configuration::open(&ConfigOpenOptions::inspection()).unwrap();
    assert_eq!(configuration.source().kind, ConfigSourceKind::Environment);
    assert_eq!(
        configuration.source().path,
        std::fs::canonicalize(env_path).unwrap()
    );
}

#[test]
fn config_env_ac_03_missing_and_unreadable_files_fail_with_exact_codes() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml");
    let error = open_path(&missing).unwrap_err();
    assert_eq!(code(&error), "missing_config");

    let unreadable = dir.path().join("directory");
    std::fs::create_dir(&unreadable).unwrap();
    let error = open_path(&unreadable).unwrap_err();
    assert_eq!(code(&error), "unreadable_config");
}

#[test]
fn config_env_ac_05_unknown_and_future_versions_leave_bytes_unchanged() {
    for text in [
        "version = 2\n",
        "version = \"1\"\n",
        "environment = \"production\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), text);
        let before = std::fs::read(&path).unwrap();
        let error = open_path(&path).unwrap_err();
        assert_eq!(code(&error), "unsupported_version");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn config_env_ac_05_duplicate_and_unknown_fields_are_distinct() {
    let duplicate = document::parse_configuration("version = 1\nversion = 1\n").unwrap_err();
    assert_eq!(duplicate.code(), "duplicate_key");
    let unknown =
        document::parse_configuration("version = 1\ndefault_environment = \"production\"\n")
            .unwrap_err();
    assert_eq!(unknown.code(), "unknown_field");
}

#[test]
fn config_env_ac_20_managed_routes_and_secret_fields_are_not_schema_fields() {
    for text in [
        "version = 1\n[environments.company]\nmode = \"account\"\napi_origin = \"https://cloud.example.com\"\nissuer = \"https://auth.example.com/oidc\"\nnative_client_id = \"whalerein-cli\"\nmqtt_url = \"mqtts://broker.example.com:443\"\n",
        "version = 1\n[environments.company]\nmode = \"standalone\"\ntoken = \"secret\"\n",
        "version = 1\n[environments.company]\nmode = \"account\"\napi_origin = \"https://cloud.example.com\"\nissuer = \"https://auth.example.com/oidc\"\nnative_client_id = \"whalerein-cli\"\nregion = \"eu-west-1\"\n",
    ] {
        let error = document::parse_configuration(text).unwrap_err();
        assert_eq!(error.code(), "unknown_field");
    }
}

// ---------------------------------------------------------------------------
// Resolution and views
// ---------------------------------------------------------------------------

#[test]
fn config_env_ac_06_selection_precedence_and_no_fallback_are_explicit() {
    let (_dir, configuration) = open_text(&custom_account("company"));
    let saved = resolve_environment(&configuration, None).unwrap();
    assert_eq!(saved.environment, "company");
    assert_eq!(saved.selection_source, SelectionSource::File);
    assert_eq!(saved.definition_source, DefinitionSource::File);

    let overridden = resolve_environment(&configuration, Some("local")).unwrap();
    assert_eq!(overridden.environment, "local");
    assert_eq!(overridden.selection_source, SelectionSource::Invocation);
    assert_eq!(overridden.definition_source, DefinitionSource::Builtin);
    assert_eq!(overridden.definition.environment.as_deref(), Some("local"));

    let (_dir, minimal) = open_text("version = 1\n");
    let default = resolve_environment(&minimal, None).unwrap();
    assert_eq!(default.environment, "production");
    assert_eq!(default.selection_source, SelectionSource::Default);
    assert_eq!(default.definition_source, DefinitionSource::Builtin);

    for unknown in ["ghost", "Production", "-company", ""] {
        let error = resolve_environment(&minimal, Some(unknown)).unwrap_err();
        let expected = if unknown == "ghost" {
            "unknown_environment"
        } else {
            "invalid_identifier"
        };
        assert_eq!(code(&error), expected);
    }
}

#[test]
fn config_env_ac_06_override_is_ephemeral_and_never_rewrites_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &custom_account("company"));
    let before = std::fs::read(&path).unwrap();
    let configuration = open_path(&path).unwrap();
    resolve_environment(&configuration, Some("production")).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        resolve_environment(&configuration, None)
            .unwrap()
            .environment,
        "company"
    );
}

#[test]
fn config_env_ac_02_builtin_authority_and_trust_specialization() {
    let (_dir, configuration) = open_text(
        "version = 1\nenvironment = \"local\"\n\n[environments.local]\n\
         label = \"Development machine\"\n\n[environments.local.tls]\n\
         ca_provider = \"file\"\nca_file = \"/opt/whalerein/development-rootCA.pem\"\n",
    );
    let resolution = resolve_environment(&configuration, None).unwrap();
    assert_eq!(resolution.definition.id, "official-local");
    assert_eq!(resolution.definition.mode, Some(EnvironmentMode::Account));
    assert_eq!(
        resolution.definition.api_origin.as_deref(),
        Some("https://app.whalerein.test")
    );
    assert_eq!(
        resolution.definition.label.as_deref(),
        Some("Development machine")
    );
    assert_eq!(
        resolution.definition.tls.unwrap().ca_provider,
        TlsProvider::File
    );

    for text in [
        "version = 1\n[environments.production]\napi_origin = \"https://cloud.example.com\"\n",
        "version = 1\n[environments.local]\nmode = \"standalone\"\n",
        "version = 1\n[environments.staging]\nnative_client_id = \"custom-cli\"\n",
        "version = 1\n[environments.production]\nenvironment = \"production\"\n",
    ] {
        let error = document::parse_configuration(text).unwrap_err();
        assert_eq!(error.code(), "authority_override");
    }
}

#[test]
fn config_env_ac_04_custom_modes_and_no_hostname_inference() {
    let (_dir, standalone) = open_text(
        "version = 1\nenvironment = \"lab\"\n\n[environments.lab]\n\
         label = \"Lab pairing\"\nmode = \"standalone\"\n",
    );
    let resolution = resolve_environment(&standalone, None).unwrap();
    assert_eq!(resolution.definition.id, "lab");
    assert_eq!(
        resolution.definition.mode,
        Some(EnvironmentMode::Standalone)
    );
    assert_eq!(resolution.definition.api_origin, None);
    assert_eq!(
        resolution.definition.tls.unwrap().ca_provider,
        TlsProvider::System
    );

    let arbitrary = custom_account("team-1-eu");
    let (_dir, configuration) = open_text(&arbitrary);
    assert_eq!(
        resolve_environment(&configuration, None)
            .unwrap()
            .environment,
        "team-1-eu"
    );

    for text in [
        "version = 1\n[environments.company]\napi_origin = \"https://cloud.example.com\"\nissuer = \"https://auth.example.com/oidc\"\nnative_client_id = \"whalerein-cli\"\n",
        "version = 1\n[environments.company]\nmode = \"oauth\"\n",
        "version = 1\n[environments.company]\nmode = \"account\"\napi_origin = \"https://cloud.example.com\"\nnative_client_id = \"whalerein-cli\"\n",
        "version = 1\n[environments.lab]\nmode = \"standalone\"\nissuer = \"https://auth.example.com/oidc\"\n",
    ] {
        let error = document::parse_configuration(text).unwrap_err();
        assert!(
            matches!(error.code(), "invalid_mode" | "invalid_mode_fields"),
            "{}",
            error.code()
        );
    }
    let error = document::parse_configuration(
        "version = 1\n[environments.company]\nmode = \"account\"\napi_origin = \"http://cloud.example.com\"\nissuer = \"https://auth.example.com/oidc\"\nnative_client_id = \"whalerein-cli\"\n",
    )
    .unwrap_err();
    assert_eq!(error.code(), "invalid_url");
}

#[test]
fn config_env_ac_16_view_reports_provenance_and_offline_scope() {
    let (_dir, configuration) = open_text(
        "version = 1\nenvironment = \"local\"\n\n[environments.local]\n\
         label = \"Development machine\"\n\n[environments.local.tls]\n\
         ca_provider = \"file\"\nca_file = \"/opt/rootCA.pem\"\n",
    );
    let view = configuration.view(Some("staging"));
    assert_eq!(view.selected_environment.as_deref(), Some("staging"));
    assert_eq!(view.selection_source, Some(SelectionSource::Invocation));
    assert_eq!(view.definition_source, Some(DefinitionSource::Builtin));
    assert_eq!(view.desired_default.as_deref(), Some("local"));
    assert_eq!(view.effective_override.as_deref(), Some("staging"));
    assert_eq!(view.field_provenance["label"], FieldSource::Catalog);
    assert_eq!(view.field_provenance["tls"], FieldSource::Catalog);
    assert_eq!(view.field_provenance["api_origin"], FieldSource::Catalog);
    assert_eq!(view.validation.scope, ValidationScope::Offline);
    assert_eq!(view.validation.live_readiness, LiveReadiness::NotChecked);
    assert!(view.errors.is_empty());

    let view = configuration.view(None);
    assert_eq!(view.field_provenance["label"], FieldSource::File);
    assert_eq!(view.field_provenance["tls"], FieldSource::File);

    let view = configuration.view(Some("ghost"));
    assert_eq!(view.errors.len(), 1);
    assert_eq!(view.errors[0].code, "unknown_environment");
    assert_eq!(view.selected_environment, None);
}

#[test]
fn config_env_ac_04_view_reports_custom_file_provenance_and_defaults() {
    let (_dir, configuration) = open_text(&custom_account("company"));
    let view = configuration.view(None);
    assert_eq!(view.field_provenance["id"], FieldSource::File);
    assert_eq!(view.field_provenance["mode"], FieldSource::File);
    assert_eq!(view.field_provenance["label"], FieldSource::File);
    assert_eq!(view.field_provenance["api_origin"], FieldSource::File);
    assert_eq!(view.field_provenance["environment"], FieldSource::Default);
    assert_eq!(view.field_provenance["tls"], FieldSource::Default);
    assert_eq!(
        view.definition.unwrap().tls.unwrap().ca_provider,
        TlsProvider::System
    );
}

// ---------------------------------------------------------------------------
// Typed updates, comments and revisions
// ---------------------------------------------------------------------------

#[test]
fn config_env_ac_12_update_preserves_comments_and_unrelated_keys() {
    let dir = tempfile::tempdir().unwrap();
    let source =
        std::fs::read_to_string(contract_root().join("fixtures/valid/production-selection.toml"))
            .unwrap();
    let path = write_config(dir.path(), &source);
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("staging".into()),
    };
    let updated = configuration
        .update(&patch, configuration.revision())
        .unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("# Operator notes must survive settings updates."));
    assert!(after.contains("label = \"WhaleRein Cloud (work laptop)\""));
    assert_eq!(
        resolve_environment(&updated, None).unwrap().environment,
        "staging"
    );
    assert_eq!(
        updated.revision(),
        Some(&*writer::content_revision(after.as_bytes()))
    );
}

#[test]
fn config_env_ac_19_set_default_affects_the_next_open_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &custom_account("company"));
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("production".into()),
    };
    configuration
        .update(&patch, configuration.revision())
        .unwrap();
    let reopened = open_path(&path).unwrap();
    assert_eq!(
        resolve_environment(&reopened, None).unwrap().environment,
        "production"
    );
    assert_eq!(
        resolve_environment(&reopened, Some("company"))
            .unwrap()
            .environment,
        "company"
    );
}

#[test]
fn config_env_ac_04_upsert_custom_environment_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
        environment: "lab".into(),
        label: Some("Lab pairing".into()),
        mode: EnvironmentMode::Standalone,
        api_origin: None,
        issuer: None,
        native_client_id: None,
        tls: Some(TlsConfiguration::File {
            ca_file: "/opt/lab/rootCA.pem".into(),
        }),
    });
    let updated = configuration
        .update(&patch, configuration.revision())
        .unwrap();
    let resolution = resolve_environment(&updated, Some("lab")).unwrap();
    assert_eq!(
        resolution.definition.mode,
        Some(EnvironmentMode::Standalone)
    );
    assert_eq!(
        resolution.definition.tls.unwrap().ca_provider,
        TlsProvider::File
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("[environments.lab]"));
    assert!(after.contains("ca_provider = \"file\""));
}

#[test]
fn config_env_ac_02_specialize_and_remove_only_the_permitted_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &custom_account("company"));
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::SpecializeBuiltin(BuiltinSpecialization {
        environment: "local".into(),
        label: Some("Development machine".into()),
        clear_label: false,
        tls: Some(TlsConfiguration::Mkcert),
        clear_tls: false,
    });
    let updated = configuration
        .update(&patch, configuration.revision())
        .unwrap();
    let resolution = resolve_environment(&updated, Some("local")).unwrap();
    assert_eq!(
        resolution.definition.label.as_deref(),
        Some("Development machine")
    );
    assert_eq!(
        resolution.definition.tls.unwrap().ca_provider,
        TlsProvider::Mkcert
    );

    let patch = ConfigPatch::RemoveCustomEnvironment {
        environment: "company".into(),
    };
    let updated = updated.update(&patch, updated.revision()).unwrap();
    assert!(!updated.document().environments.contains_key("company"));
    assert!(updated.document().environments.contains_key("local"));

    for patch in [
        ConfigPatch::RemoveCustomEnvironment {
            environment: "production".into(),
        },
        ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
            environment: "production".into(),
            label: None,
            mode: EnvironmentMode::Standalone,
            api_origin: None,
            issuer: None,
            native_client_id: None,
            tls: None,
        }),
    ] {
        let error = updated.update(&patch, updated.revision()).unwrap_err();
        assert_eq!(code(&error), "authority_override");
    }
}

#[test]
fn config_env_ac_13_invalid_patch_does_not_rewrite_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let patch = ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
        environment: "company".into(),
        label: None,
        mode: EnvironmentMode::Account,
        api_origin: Some("https://cloud.example.com".into()),
        issuer: None,
        native_client_id: Some("whalerein-cli".into()),
        tls: None,
    });
    let error = configuration
        .update(&patch, configuration.revision())
        .unwrap_err();
    assert_eq!(code(&error), "invalid_mode_fields");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn config_env_ac_12_stale_revision_returns_conflict_with_current_revision_and_draft() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let revision = configuration.revision().unwrap().to_string();
    std::fs::write(&path, "version = 1\nenvironment = \"staging\"\n").unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("production".into()),
    };
    let error = configuration.update(&patch, Some(&revision)).unwrap_err();
    assert_eq!(code(&error), "revision_conflict");
    let CoreError::Configuration(error) = error else {
        panic!("typed configuration error")
    };
    assert_eq!(
        error.current_revision(),
        Some(&*writer::content_revision(
            b"version = 1\nenvironment = \"staging\"\n"
        ))
    );
    assert_eq!(error.draft(), Some(&patch));
}

#[test]
fn config_env_ac_13_future_file_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    std::fs::write(&path, "version = 2\n").unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("production".into()),
    };
    let error = configuration
        .update(&patch, configuration.revision())
        .unwrap_err();
    assert_eq!(code(&error), "revision_conflict");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "version = 2\n");
}

#[test]
fn config_env_ac_12_concurrent_updates_serialize_without_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = std::sync::Arc::new(write_config(dir.path(), "version = 1\n"));
    let _serial = EnvScope::new(None, None);
    let mut threads = Vec::new();
    for index in 0..6 {
        let path = path.clone();
        threads.push(std::thread::spawn(move || {
            let target = if index % 2 == 0 {
                "staging"
            } else {
                "production"
            };
            for _ in 0..200 {
                let configuration = open_path(&path).unwrap();
                let patch = ConfigPatch::SetDefaultEnvironment {
                    environment: Some(target.into()),
                };
                match configuration.update(&patch, configuration.revision()) {
                    Ok(_) => return,
                    Err(error) if error.code() == "revision_conflict" => continue,
                    Err(error) => panic!("{}", error.code()),
                }
            }
            panic!("update never made progress");
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let configuration = open_path(&path).unwrap();
    assert!(matches!(
        resolve_environment(&configuration, None)
            .unwrap()
            .environment
            .as_str(),
        "staging" | "production"
    ));
}

#[test]
#[ignore = "helper process that holds the sidecar lock until killed"]
fn config_env_ac_12_crash_lock_release_holder() {
    let lock = PathBuf::from(std::env::var_os("WHALEREIN_TEST_LOCK_PATH").unwrap());
    let ready = PathBuf::from(std::env::var_os("WHALEREIN_TEST_LOCK_READY").unwrap());
    let _guard = writer::ConfigLock::acquire(&lock).unwrap();
    std::fs::write(ready, b"ready").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn config_env_ac_12_crash_lock_release_leaves_the_file_usable() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let lock = writer::lock_path(&std::fs::canonicalize(&path).unwrap());
    let ready = dir.path().join("ready");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "configuration::tests::config_env_ac_12_crash_lock_release_holder",
        ])
        .env("WHALEREIN_TEST_LOCK_PATH", &lock)
        .env("WHALEREIN_TEST_LOCK_READY", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(ready.exists(), "helper process did not take the lock");
    child.kill().unwrap();
    child.wait().unwrap();
    let guard = writer::ConfigLock::acquire(&lock).expect("OS released the crashed lock");
    drop(guard);
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("staging".into()),
    };
    configuration
        .update(&patch, configuration.revision())
        .unwrap();
}

struct FaultReset;

impl Drop for FaultReset {
    fn drop(&mut self) {
        writer::FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn config_env_ac_13_write_and_replace_faults_leave_recoverable_state() {
    let _serial = EnvScope::new(None, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("staging".into()),
    };
    let before = std::fs::read(&path).unwrap();

    for point in [1, 2] {
        let _reset = FaultReset;
        writer::FAULT.store(point, std::sync::atomic::Ordering::SeqCst);
        let error = configuration
            .update(&patch, configuration.revision())
            .unwrap_err();
        assert_eq!(code(&error), "unreadable_config");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    let _reset = FaultReset;
    writer::FAULT.store(3, std::sync::atomic::Ordering::SeqCst);
    let error = configuration
        .update(&patch, configuration.revision())
        .unwrap_err();
    assert_eq!(code(&error), "unreadable_config");
    let after = std::fs::read(&path).unwrap();
    let (parsed, _) = document::parse_configuration(std::str::from_utf8(&after).unwrap()).unwrap();
    assert_eq!(parsed.environment.as_deref(), Some("staging"));
}

#[test]
fn config_env_ac_08_no_cli_import_and_sandbox_paths_keep_state_untouched() {
    let state = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("legacy-identity"), b"retained").unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let path = config_dir.path().join("device.toml");
    std::fs::write(&path, custom_account("company")).unwrap();
    let before: Vec<_> = std::fs::read_dir(state.path()).unwrap().collect();
    let configuration = open_path(&path).unwrap();
    let view = configuration.view(None);
    assert_eq!(view.selected_environment.as_deref(), Some("company"));
    let moved_dir = tempfile::tempdir().unwrap();
    let moved = moved_dir.path().join("moved.toml");
    std::fs::rename(&path, &moved).unwrap();
    let reopened = open_path(&moved).unwrap();
    assert_eq!(
        resolve_environment(&reopened, None).unwrap().environment,
        "company"
    );
    let after: Vec<_> = std::fs::read_dir(state.path()).unwrap().collect();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.len(), 1);
    assert!(state.path().join("legacy-identity").exists());
}

// ---------------------------------------------------------------------------
// Scoped TLS trust
// ---------------------------------------------------------------------------

fn generated_cert(host: &str) -> (Vec<u8>, rustls::pki_types::CertificateDer<'static>) {
    let key = rcgen::generate_simple_self_signed(vec![host.to_string()]).unwrap();
    (key.cert.pem().into_bytes(), key.cert.der().clone())
}

fn verify(
    roots: &[rustls::pki_types::CertificateDer<'static>],
    leaf: &rustls::pki_types::CertificateDer<'static>,
    host: &str,
) -> Result<(), rustls::Error> {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store.add(root.clone()).unwrap();
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder(std::sync::Arc::new(store))
        .build()
        .unwrap();
    use rustls::client::danger::ServerCertVerifier;
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).unwrap();
    verifier
        .verify_server_cert(leaf, &[], &name, &[], rustls::pki_types::UnixTime::now())
        .map(|_| ())
}

#[test]
fn config_env_ac_10_correct_chain_and_hostname_verify_but_wrong_host_does_not() {
    let (pem, leaf) = generated_cert("app.whalerein.test");
    let root = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    verify(&root, &leaf, "app.whalerein.test").expect("valid chain and host");
    assert!(verify(&root, &leaf, "other.whalerein.test").is_err());
    assert!(verify(&root, &leaf, "localhost").is_err());
}

#[test]
fn config_env_ac_10_wrong_ca_is_rejected() {
    let (wrong_pem, wrong_leaf) = generated_cert("app.whalerein.test");
    let (trusted_pem, _) = generated_cert("app.whalerein.test");
    let root = rustls_pemfile::certs(&mut &trusted_pem[..])
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(verify(&root, &wrong_leaf, "app.whalerein.test").is_err());
    assert!(!wrong_pem.is_empty());
}

#[test]
fn config_env_ac_10_system_scope_returns_no_private_ca_material() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let trust = TrustResolver::new(&path).resolve(&resolution).unwrap();
    assert_eq!(trust.scope, TlsTrustScope::System);
    assert!(trust.ca_pem.is_none());
    assert!(trust.ca_path.is_none());
}

#[test]
fn config_env_ac_10_mkcert_scope_uses_fixed_discovery_and_missing_is_actionable() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None).with_data_home(data.path());
    let (pem, _) = generated_cert("app.whalerein.test");
    let ca_path = mkcert_ca_path().unwrap();
    std::fs::create_dir_all(ca_path.parent().unwrap()).unwrap();
    std::fs::write(&ca_path, &pem).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\nenvironment = \"local\"\n");
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let trust = TrustResolver::new(&path).resolve(&resolution).unwrap();
    assert_eq!(trust.scope, TlsTrustScope::Mkcert);
    assert_eq!(trust.ca_pem.as_deref(), Some(pem.as_slice()));
    assert_eq!(trust.ca_path.as_deref(), Some(ca_path.as_path()));
    assert!(!trust
        .ca_pem
        .as_deref()
        .unwrap()
        .windows(b"PRIVATE KEY".len())
        .any(|window| window == b"PRIVATE KEY"));
}

#[test]
fn config_env_ac_10_missing_mkcert_ca_is_invalid_tls_without_changing_selection() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\nenvironment = \"local\"\n");
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let error = TrustResolver::new(&path).resolve(&resolution).unwrap_err();
    assert_eq!(error.code(), "invalid_tls");
    assert_eq!(
        resolve_environment(&configuration, None)
            .unwrap()
            .environment,
        "local"
    );
}

#[test]
fn config_env_ac_10_explicit_file_scope_expands_tilde_and_relative_once() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None);
    let (pem, _) = generated_cert("app.whalerein.test");
    std::fs::write(home.path().join("rootCA.pem"), &pem).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        "version = 1\nenvironment = \"local\"\n\n[environments.local.tls]\n\
         ca_provider = \"file\"\nca_file = \"~/rootCA.pem\"\n",
    );
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let trust = TrustResolver::new(&path).resolve(&resolution).unwrap();
    assert_eq!(trust.scope, TlsTrustScope::File);
    assert_eq!(
        trust.ca_path.as_deref(),
        Some(home.path().join("rootCA.pem").as_path())
    );
    assert_eq!(trust.ca_pem.as_deref(), Some(pem.as_slice()));

    let relative = resolve_ca_path(&path, "ca.pem").unwrap();
    assert_eq!(relative, dir.path().join("ca.pem"));
    let unchanged = resolve_ca_path(&path, "~foo/ca.pem").unwrap();
    assert_eq!(unchanged, dir.path().join("~foo/ca.pem"));
}

#[test]
fn config_env_ac_10_missing_ca_file_and_bad_pem_are_invalid_tls() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        "version = 1\nenvironment = \"local\"\n\n[environments.local.tls]\n\
         ca_provider = \"file\"\nca_file = \"missing.pem\"\n",
    );
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let error = TrustResolver::new(&path).resolve(&resolution).unwrap_err();
    assert_eq!(error.code(), "invalid_tls");

    std::fs::write(dir.path().join("bad.pem"), b"not a certificate").unwrap();
    let path = write_config(
        dir.path(),
        "version = 1\nenvironment = \"local\"\n\n[environments.local.tls]\n\
         ca_provider = \"file\"\nca_file = \"bad.pem\"\n",
    );
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let error = TrustResolver::new(&path).resolve(&resolution).unwrap_err();
    assert_eq!(error.code(), "invalid_tls");
}

#[test]
fn config_env_ac_10_local_mkcert_is_not_inherited_by_other_environments() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None).with_data_home(data.path());
    let (pem, _) = generated_cert("app.whalerein.test");
    let ca_path = mkcert_ca_path().unwrap();
    std::fs::create_dir_all(ca_path.parent().unwrap()).unwrap();
    std::fs::write(&ca_path, &pem).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let production = resolve_environment(&configuration, None).unwrap();
    let trust = TrustResolver::new(&path).resolve(&production).unwrap();
    assert_eq!(trust.scope, TlsTrustScope::System);
    assert!(trust.ca_pem.is_none());
    let local = resolve_environment(&configuration, Some("local")).unwrap();
    let trust = TrustResolver::new(&path).resolve(&local).unwrap();
    assert_eq!(trust.scope, TlsTrustScope::Mkcert);
}

#[test]
fn config_env_ac_10_ca_rotation_is_read_again_per_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        "version = 1\nenvironment = \"local\"\n\n[environments.local.tls]\n\
         ca_provider = \"file\"\nca_file = \"rootCA.pem\"\n",
    );
    let (first, _) = generated_cert("app.whalerein.test");
    let (second, _) = generated_cert("app.whalerein.test");
    std::fs::write(dir.path().join("rootCA.pem"), &first).unwrap();
    let configuration = open_path(&path).unwrap();
    let resolution = resolve_environment(&configuration, None).unwrap();
    let trust = TrustResolver::new(&path).resolve(&resolution).unwrap();
    assert_eq!(trust.ca_pem.as_deref(), Some(first.as_slice()));
    std::fs::write(dir.path().join("rootCA.pem"), &second).unwrap();
    let trust = TrustResolver::new(&path).resolve(&resolution).unwrap();
    assert_eq!(trust.ca_pem.as_deref(), Some(second.as_slice()));
    assert_ne!(first, second);
}

#[test]
fn config_env_ac_02_builtin_trust_specialization_accepts_explicit_file() {
    let (parsed, _) = document::parse_configuration(
        "version = 1\n[environments.production]\n[environments.production.tls]\n\
         ca_provider = \"file\"\nca_file = \"/tmp/rootCA.pem\"\n",
    )
    .unwrap();
    assert_eq!(
        parsed.environments["production"]
            .tls
            .as_ref()
            .unwrap()
            .ca_provider,
        TlsProvider::File
    );
}

#[test]
fn config_env_ac_02_catalog_identity_is_stable_for_the_corpus_and_json_view() {
    let value = json!({
        "catalog_local": document::catalog().unwrap().get("local").unwrap().id,
        "catalog_production": document::catalog().unwrap().get("production").unwrap().id,
    });
    assert_eq!(value["catalog_local"], "official-local");
    assert_eq!(value["catalog_production"], "official-production");
}

#[cfg(unix)]
#[test]
fn config_env_ac_12_changed_canonical_target_is_a_conflict() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.toml");
    let second = dir.path().join("second.toml");
    std::fs::write(&first, "version = 1\n").unwrap();
    std::fs::write(&second, "version = 1\n").unwrap();
    let link = dir.path().join("config.toml");
    symlink(&first, &link).unwrap();
    let configuration = open_path(&link).unwrap();
    std::fs::remove_file(&link).unwrap();
    symlink(&second, &link).unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("staging".into()),
    };
    let error = configuration
        .update(&patch, configuration.revision())
        .unwrap_err();
    assert_eq!(code(&error), "revision_conflict");
    let CoreError::Configuration(error) = error else {
        panic!("typed configuration error")
    };
    assert_eq!(error.draft(), Some(&patch));
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "version = 1\n");
}

#[test]
fn config_env_ac_04_account_upsert_covers_every_typed_field_and_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let patch = ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
        environment: "company".into(),
        label: Some("Company".into()),
        mode: EnvironmentMode::Account,
        api_origin: Some("https://cloud.example.com".into()),
        issuer: Some("https://auth.example.com/oidc".into()),
        native_client_id: Some("whalerein-cli".into()),
        tls: Some(TlsConfiguration::System),
    });
    let updated = configuration
        .update(&patch, configuration.revision())
        .unwrap();
    let definition = &updated.document().environments["company"];
    assert_eq!(definition.label.as_deref(), Some("Company"));
    assert_eq!(
        definition.api_origin.as_deref(),
        Some("https://cloud.example.com")
    );
    assert_eq!(
        definition.native_client_id.as_deref(),
        Some("whalerein-cli")
    );
    assert!(definition.tls.is_some());

    let replace = ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
        environment: "company".into(),
        label: None,
        mode: EnvironmentMode::Standalone,
        api_origin: None,
        issuer: None,
        native_client_id: None,
        tls: None,
    });
    let updated = updated.update(&replace, updated.revision()).unwrap();
    let definition = &updated.document().environments["company"];
    assert_eq!(definition.mode, Some(EnvironmentMode::Standalone));
    assert_eq!(definition.label, None);
    assert_eq!(definition.api_origin, None);
    assert_eq!(definition.issuer, None);
    assert_eq!(definition.native_client_id, None);
    assert!(definition.tls.is_none());

    let invalid = ConfigPatch::UpsertCustomEnvironment(CustomEnvironmentInput {
        environment: "lab".into(),
        label: None,
        mode: EnvironmentMode::Standalone,
        api_origin: Some("https://cloud.example.com".into()),
        issuer: None,
        native_client_id: None,
        tls: None,
    });
    let error = updated.update(&invalid, updated.revision()).unwrap_err();
    assert_eq!(code(&error), "invalid_mode_fields");
}

#[test]
fn config_env_ac_12_clear_default_and_reject_unknown_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\nenvironment = \"production\"\n");
    let configuration = open_path(&path).unwrap();
    let cleared = configuration
        .update(
            &ConfigPatch::SetDefaultEnvironment { environment: None },
            configuration.revision(),
        )
        .unwrap();
    assert_eq!(cleared.document().environment, None);
    let error = cleared
        .update(
            &ConfigPatch::SetDefaultEnvironment {
                environment: Some("ghost".into()),
            },
            cleared.revision(),
        )
        .unwrap_err();
    assert_eq!(code(&error), "unknown_environment");
}

#[test]
fn config_env_ac_02_specialize_noop_clear_and_reject_custom() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        "version = 1\n\n[environments.local]\nlabel = \"Local\"\n\n[environments.local.tls]\nca_provider = \"mkcert\"\n",
    );
    let configuration = open_path(&path).unwrap();
    let noop = ConfigPatch::SpecializeBuiltin(BuiltinSpecialization {
        environment: "local".into(),
        label: None,
        clear_label: false,
        tls: None,
        clear_tls: false,
    });
    configuration
        .update(&noop, configuration.revision())
        .unwrap();
    let clear = ConfigPatch::SpecializeBuiltin(BuiltinSpecialization {
        environment: "local".into(),
        label: None,
        clear_label: true,
        tls: None,
        clear_tls: true,
    });
    let updated = configuration
        .update(&clear, configuration.revision())
        .unwrap();
    assert!(updated.document().environments["local"].label.is_none());
    assert!(updated.document().environments["local"].tls.is_none());

    let (dir, custom) = open_text(&custom_account("company"));
    let error = custom
        .update(
            &ConfigPatch::SpecializeBuiltin(BuiltinSpecialization {
                environment: "company".into(),
                label: Some("x".into()),
                clear_label: false,
                tls: None,
                clear_tls: false,
            }),
            custom.revision(),
        )
        .unwrap_err();
    assert_eq!(code(&error), "invalid_argument");
    drop(dir);
    drop(updated);
}

#[test]
fn config_env_ac_12_remove_unknown_custom_environment_is_typed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    let error = configuration
        .update(
            &ConfigPatch::RemoveCustomEnvironment {
                environment: "missing".into(),
            },
            configuration.revision(),
        )
        .unwrap_err();
    assert_eq!(code(&error), "unknown_environment");
}

#[test]
fn config_env_ac_05_catalog_validation_rejects_each_malformed_shape() {
    let base: serde_json::Value = serde_json::from_str(CONFIG_CATALOG_JSON).unwrap();
    let mutate = |path: &[&str], value: serde_json::Value| {
        let mut catalog = base.clone();
        let mut current = &mut catalog;
        for key in &path[..path.len() - 1] {
            current = &mut current[key];
        }
        current[path[path.len() - 1]] = value;
        catalog
    };
    let invalid = |catalog: serde_json::Value| {
        document::parse_catalog(&serde_json::to_string(&catalog).unwrap()).unwrap_err()
    };
    assert_eq!(
        invalid(mutate(&["schema"], json!("other"))).code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(&["version"], json!(2))).code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "mode"],
            json!("standalone")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "issuer"],
            json!("https://app.whalerein.test/oidc")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "tls", "ca_provider"],
            json!("disabled")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "tls", "ca_file"],
            json!("/tmp/rootCA.pem")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "production", "tls", "ca_provider"],
            json!("mkcert")
        ))
        .code(),
        "invalid_syntax"
    );
    let mut extra = base.clone();
    extra["environments"]["extra"] = base["environments"]["local"].clone();
    assert_eq!(invalid(extra).code(), "invalid_syntax");
    let mut missing = base;
    missing["environments"]
        .as_object_mut()
        .unwrap()
        .remove("local");
    assert_eq!(invalid(missing).code(), "invalid_syntax");
}

#[test]
fn config_env_ac_05_document_validation_covers_shapes_and_tls_defaults() {
    let builtin_unknown =
        document::parse_configuration("version = 1\n[environments.production]\nregion = \"eu\"\n")
            .unwrap_err();
    assert_eq!(builtin_unknown.code(), "unknown_field");
    assert_eq!(
        document::parse_configuration(
            "version = 1\n[environments.company]\nmode = \"account\"\napi_origin = \"https://cloud.example.com\"\nissuer = \"https://auth.example.com/oidc\"\nnative_client_id = \"bad client\"\n",
        )
        .unwrap_err()
        .code(),
        "invalid_mode_fields"
    );
    assert_eq!(
        document::parse_configuration(
            "version = 1\n[environments.company]\nmode = \"standalone\"\ntls = \"system\"\n",
        )
        .unwrap_err()
        .code(),
        "invalid_tls"
    );
    assert_eq!(
        document::parse_configuration(
            "version = 1\n[environments.company]\nmode = \"standalone\"\n[environments.company.tls]\nca_provider = \"system\"\nunknown = 1\n",
        )
        .unwrap_err()
        .code(),
        "unknown_field"
    );
    for text in [
        "version = 1\n[environments.company]\nmode = \"standalone\"\n[environments.company.tls]\nca_file = \"\"\n",
        "version = 1\n[environments.company]\nmode = \"standalone\"\n[environments.company.tls]\nca_provider = \"system\"\nca_file = \"/tmp/ca.pem\"\n",
        "version = 1\n[environments.company]\nmode = \"account\"\napi_origin = \"https://cloud.example.com\"\nissuer = \"https://auth.example.com/oidc\"\nnative_client_id = \"whalerein-cli\"\n[environments.company.tls]\nca_provider = \"file\"\nca_file = \"\"\n",
    ] {
        let error = document::parse_configuration(text).unwrap_err();
        assert_eq!(error.code(), "invalid_tls");
    }
    let (parsed, _) = document::parse_configuration(
        "version = 1\n[environments.local]\n[environments.local.tls]\n",
    )
    .unwrap();
    assert_eq!(
        parsed.environments["local"]
            .tls
            .as_ref()
            .unwrap()
            .ca_provider,
        TlsProvider::Mkcert
    );
    let (parsed, _) = document::parse_configuration(
        "version = 1\n[environments.company]\nmode = \"standalone\"\n[environments.company.tls]\n",
    )
    .unwrap();
    assert_eq!(
        parsed.environments["company"]
            .tls
            .as_ref()
            .unwrap()
            .ca_provider,
        TlsProvider::System
    );
}

#[test]
fn config_env_ac_05_url_validation_covers_empty_loopback_and_credentials() {
    for value in [
        "",
        "https://",
        "https://user@cloud.example.com",
        "https://cloud.example.com?q=1",
        "https://cloud.example.com#f",
    ] {
        assert_eq!(
            document::validate_url(value, "api_origin")
                .unwrap_err()
                .code(),
            "invalid_url",
            "{value}"
        );
    }
    assert_eq!(
        document::validate_url("http://127.0.0.1:8080", "api_origin").unwrap(),
        "http://127.0.0.1:8080"
    );
    assert_eq!(
        document::validate_url("https://cloud.example.com:8443", "api_origin").unwrap(),
        "https://cloud.example.com:8443"
    );
}

#[test]
fn config_env_ac_10_trust_definition_errors_are_typed() {
    let definition = EnvironmentDefinition {
        id: "custom".into(),
        mode: Some(EnvironmentMode::Standalone),
        label: None,
        api_origin: None,
        issuer: None,
        native_client_id: None,
        environment: None,
        tls: None,
    };
    let resolver = TrustResolver::new("/tmp/config.toml");
    assert_eq!(resolver.config_path(), Path::new("/tmp/config.toml"));
    assert_eq!(
        resolver.resolve_definition(&definition).unwrap_err().code(),
        "invalid_tls"
    );
    let mut mkcert = definition.clone();
    mkcert.tls = Some(TlsConfig {
        ca_provider: TlsProvider::Mkcert,
        ca_file: None,
    });
    assert_eq!(
        resolver.resolve_definition(&mkcert).unwrap_err().code(),
        "invalid_tls"
    );
    let mut file = definition;
    file.tls = Some(TlsConfig {
        ca_provider: TlsProvider::File,
        ca_file: None,
    });
    assert_eq!(
        resolver.resolve_definition(&file).unwrap_err().code(),
        "invalid_tls"
    );
}

#[test]
fn config_env_ac_10_tilde_and_unreadable_ca_paths_are_typed() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None);
    let config = tempfile::tempdir().unwrap().path().join("config.toml");
    assert_eq!(resolve_ca_path(&config, "~").unwrap(), home.path());
    assert_eq!(
        resolve_ca_path(&config, "~\\ca.pem").unwrap(),
        home.path().join("ca.pem")
    );
    let dir = tempfile::tempdir().unwrap();
    let ca_dir = dir.path().join("ca-dir");
    std::fs::create_dir(&ca_dir).unwrap();
    let trust = TrustResolver::new(dir.path().join("config.toml"));
    let mut definition = EnvironmentDefinition {
        id: "local".into(),
        mode: Some(EnvironmentMode::Account),
        label: None,
        api_origin: None,
        issuer: None,
        native_client_id: None,
        environment: Some("local".into()),
        tls: None,
    };
    definition.tls = Some(TlsConfig {
        ca_provider: TlsProvider::File,
        ca_file: Some(ca_dir.to_string_lossy().into_owned()),
    });
    assert_eq!(
        trust.resolve_definition(&definition).unwrap_err().code(),
        "invalid_tls"
    );
    std::fs::write(
        dir.path().join("bad.pem"),
        "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    definition.tls = Some(TlsConfig {
        ca_provider: TlsProvider::File,
        ca_file: Some("bad.pem".into()),
    });
    assert_eq!(
        trust.resolve_definition(&definition).unwrap_err().code(),
        "invalid_tls"
    );
}

#[test]
fn config_env_ac_10_missing_home_makes_tilde_and_mkcert_actionable() {
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None).without_home();
    let config = Path::new("/tmp/config.toml");
    assert_eq!(
        resolve_ca_path(config, "~").unwrap_err().code(),
        "invalid_tls"
    );
    assert_eq!(
        resolve_ca_path(config, "~/ca.pem").unwrap_err().code(),
        "invalid_tls"
    );
    assert_eq!(
        resolve_ca_path(config, "~\\ca.pem").unwrap_err().code(),
        "invalid_tls"
    );
    let mut definition = EnvironmentDefinition {
        id: "local".into(),
        mode: Some(EnvironmentMode::Account),
        label: None,
        api_origin: None,
        issuer: None,
        native_client_id: None,
        environment: Some("local".into()),
        tls: Some(TlsConfig {
            ca_provider: TlsProvider::Mkcert,
            ca_file: None,
        }),
    };
    let error = TrustResolver::new(config)
        .resolve_definition(&definition)
        .unwrap_err();
    assert_eq!(error.code(), "invalid_tls");
    definition.environment = Some("production".into());
    assert_eq!(
        TrustResolver::new(config)
            .resolve_definition(&definition)
            .unwrap_err()
            .code(),
        "invalid_tls"
    );
}

#[test]
fn config_env_ac_03_empty_explicit_path_and_missing_home_are_errors() {
    let empty = Path::new("");
    let error = Configuration::open(&ConfigOpenOptions {
        explicit_path: Some(empty),
        ..ConfigOpenOptions::default()
    })
    .unwrap_err();
    assert_eq!(code(&error), "missing_config");
    let home = tempfile::tempdir().unwrap();
    let _env = EnvScope::new(Some(home.path()), None).without_home();
    assert_eq!(default_config_path().unwrap_err().code(), "missing_config");
}

#[test]
fn config_env_ac_12_deleted_file_between_open_and_update_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "version = 1\n");
    let configuration = open_path(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("staging".into()),
    };
    let error = configuration
        .update(&patch, configuration.revision())
        .unwrap_err();
    assert_eq!(code(&error), "revision_conflict");
    assert!(!path.exists());
}
