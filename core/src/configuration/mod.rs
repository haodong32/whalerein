//! Reusable client-environment configuration.
//!
//! Core owns one file selection, strict version-1 TOML parsing, built-in/custom
//! resolution, revisioned typed updates and scoped TLS trust. CLI and future
//! bindings consume typed results and never parse TOML themselves.

mod document;
mod error;
mod import;
mod patch;
#[cfg(test)]
mod tests;
mod trust;
mod writer;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::CoreResult;

pub use document::{
    ConfigurationDocument, EnvironmentDefinition, EnvironmentMode, TlsConfig, TlsProvider,
    CONFIG_CATALOG_JSON, CONFIG_CORPUS_JSON, CONFIG_VERSION,
};
pub use error::ConfigurationError;
pub use import::{
    commit_import, detect_legacy, legacy_state_present, preview_import, recover_import,
    ImportCommit, ImportCommitOptions, ImportConflict, ImportIssueKind, ImportJournalState,
    ImportOutcome, ImportOutcomeStatus, ImportPreview, ImportPreviewOptions, ImportPreviewStatus,
    ImportRecovery, ImportRecoveryStatus, LegacySource, LegacySourceKind, ProposedConfiguration,
    ProposedEnvironment, RetainedReference, SecureStoreHandoff, IMPORT_JOURNAL_SCHEMA,
    IMPORT_JOURNAL_VERSION,
};
pub use patch::{BuiltinSpecialization, ConfigPatch, CustomEnvironmentInput, TlsConfiguration};
pub use trust::{mkcert_ca_path, resolve_ca_path, TlsTrust, TlsTrustScope, TrustResolver};

/// How the selected file was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigSourceKind {
    /// Explicit `--config` / `config_file` path.
    Explicit,
    /// `WHALEREIN_CONFIG`.
    Environment,
    /// `~/.whalerein/config.toml`.
    Default,
}

/// Selected file and selection provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigSource {
    pub path: PathBuf,
    pub kind: ConfigSourceKind,
    /// The file was created by this successful ordinary-startup open.
    pub initialized: bool,
}

/// Options for one open. Inspection/reporting opens must leave
/// `allow_default_init` false so help and checks never write.
#[derive(Default)]
pub struct ConfigOpenOptions<'a> {
    pub explicit_path: Option<&'a Path>,
    pub allow_default_init: bool,
    /// Optional caller-owned detection of legacy state under the default
    /// config directory. A detected legacy layout suppresses initialization.
    pub legacy_state_probe: Option<&'a dyn Fn(&Path) -> bool>,
}

impl<'a> ConfigOpenOptions<'a> {
    /// Read-only open for show/check/help.
    pub fn inspection() -> Self {
        Self::default()
    }

    /// Ordinary startup open that may initialize the default file.
    pub fn startup() -> Self {
        Self {
            allow_default_init: true,
            ..Self::default()
        }
    }
}

/// Where the effective environment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectionSource {
    Invocation,
    File,
    Default,
}

/// Whether the effective definition is catalog authority or a user entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefinitionSource {
    Builtin,
    File,
}

/// Effective environment selection with provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentResolution {
    pub environment: String,
    pub selection_source: SelectionSource,
    pub definition_source: DefinitionSource,
    pub definition: EnvironmentDefinition,
}

/// Where one effective field came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldSource {
    Catalog,
    File,
    Default,
}

/// Offline validation scope. Core never claims live connectivity from a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationScope {
    Offline,
}

/// Live readiness is a separate, explicitly unclaimed check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveReadiness {
    NotChecked,
}

/// Explicit offline/live distinction for config show/check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConfigValidation {
    pub scope: ValidationScope,
    pub live_readiness: LiveReadiness,
}

/// One redacted, non-secret diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigErrorView {
    pub code: String,
    pub message: String,
    pub environment: Option<String>,
}

impl From<&ConfigurationError> for ConfigErrorView {
    fn from(error: &ConfigurationError) -> Self {
        Self {
            code: error.code().into(),
            message: error.to_string(),
            environment: error.environment().map(str::to_string),
        }
    }
}

/// Explainable config show/check result. It omits file bytes, credentials and
/// unvalidated URL contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigView {
    pub path: PathBuf,
    pub source_kind: ConfigSourceKind,
    pub revision: Option<String>,
    pub selected_environment: Option<String>,
    pub selection_source: Option<SelectionSource>,
    pub definition_source: Option<DefinitionSource>,
    pub desired_default: Option<String>,
    pub effective_override: Option<String>,
    pub definition: Option<EnvironmentDefinition>,
    pub field_provenance: BTreeMap<String, FieldSource>,
    pub validation: ConfigValidation,
    pub errors: Vec<ConfigErrorView>,
}

const MINIMAL_CONFIG: &[u8] = b"version = 1\nenvironment = \"production\"\n";

/// One opened configuration snapshot. Updates return a fresh snapshot; they
/// never mutate this value.
#[derive(Debug, Clone)]
pub struct Configuration {
    source: ConfigSource,
    document: ConfigurationDocument,
    revision: Option<String>,
    requested_path: PathBuf,
    canonical_path: PathBuf,
}

/// Snapshot alias used by the public API.
pub type ConfigSnapshot = Configuration;

impl Configuration {
    /// Resolve exactly one file and open it:
    /// explicit path -> `WHALEREIN_CONFIG` -> `~/.whalerein/config.toml`.
    pub fn open(options: &ConfigOpenOptions<'_>) -> CoreResult<ConfigSnapshot> {
        let (requested, kind) = select_path(options)?;
        let mut initialized = false;
        let bytes = match std::fs::read(&requested) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if kind != ConfigSourceKind::Default || !options.allow_default_init {
                    return Err(ConfigurationError::MissingConfig.into());
                }
                if let Some(parent) = requested.parent() {
                    if options
                        .legacy_state_probe
                        .is_some_and(|probe| probe(parent))
                    {
                        return Err(ConfigurationError::MissingConfig.into());
                    }
                }
                writer::write_atomically(&requested, MINIMAL_CONFIG)?;
                initialized = true;
                std::fs::read(&requested).map_err(|_| ConfigurationError::UnreadableConfig)?
            }
            Err(_) => return Err(ConfigurationError::UnreadableConfig.into()),
        };
        let canonical_path = canonical_target(&requested);
        let text = std::str::from_utf8(&bytes).map_err(|_| ConfigurationError::InvalidSyntax)?;
        let (document, _) = document::parse_configuration(text)?;
        let revision = Some(writer::content_revision(&bytes));
        Ok(Configuration {
            source: ConfigSource {
                path: canonical_path.clone(),
                kind,
                initialized,
            },
            document,
            revision,
            requested_path: requested,
            canonical_path,
        })
    }

    pub fn source(&self) -> &ConfigSource {
        &self.source
    }

    pub fn document(&self) -> &ConfigurationDocument {
        &self.document
    }

    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    pub fn resolve(&self, invocation_override: Option<&str>) -> CoreResult<EnvironmentResolution> {
        resolve_environment(self, invocation_override)
    }

    /// Build an explainable view. Resolution failures are redacted diagnostics,
    /// not a reason to hide the selected path and desired default.
    pub fn view(&self, invocation_override: Option<&str>) -> ConfigView {
        let mut view = ConfigView {
            path: self.source.path.clone(),
            source_kind: self.source.kind,
            revision: self.revision.clone(),
            selected_environment: None,
            selection_source: None,
            definition_source: None,
            desired_default: self.document.environment.clone(),
            effective_override: invocation_override.map(str::to_string),
            definition: None,
            field_provenance: BTreeMap::new(),
            validation: ConfigValidation {
                scope: ValidationScope::Offline,
                live_readiness: LiveReadiness::NotChecked,
            },
            errors: Vec::new(),
        };
        match resolve_environment_inner(self, invocation_override) {
            Ok(resolution) => {
                view.selected_environment = Some(resolution.environment.clone());
                view.selection_source = Some(resolution.selection_source);
                view.definition_source = Some(resolution.definition_source);
                view.field_provenance = field_provenance(&self.document, &resolution);
                view.definition = Some(resolution.definition);
            }
            Err(error) => view.errors.push(ConfigErrorView::from(&error)),
        }
        view
    }

    /// Apply one typed patch. The current file is re-read under the shared
    /// sidecar lock and the caller's revision is checked before any write.
    pub fn update(
        &self,
        patch: &ConfigPatch,
        expected_revision: Option<&str>,
    ) -> CoreResult<ConfigSnapshot> {
        let lock = writer::ConfigLock::acquire(&writer::lock_path(&self.canonical_path))?;
        let live_target = canonical_target(&self.requested_path);
        if live_target != self.canonical_path {
            let current_revision = std::fs::read(&live_target)
                .ok()
                .map(|bytes| writer::content_revision(&bytes));
            return Err(ConfigurationError::RevisionConflict {
                expected: expected_revision.map(str::to_string),
                current_revision,
                draft: Box::new(patch.clone()),
            }
            .into());
        }
        let current = match std::fs::read(&self.canonical_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(ConfigurationError::UnreadableConfig.into()),
        };
        let current_revision = current.as_deref().map(writer::content_revision);
        let expected = expected_revision.map(str::to_string);
        if current_revision != expected {
            return Err(ConfigurationError::RevisionConflict {
                expected,
                current_revision,
                draft: Box::new(patch.clone()),
            }
            .into());
        }
        let current = current.ok_or(ConfigurationError::MissingConfig)?;
        let text = std::str::from_utf8(&current).map_err(|_| ConfigurationError::InvalidSyntax)?;
        let (_, mut editable) = document::parse_configuration(text)?;
        let catalog = document::catalog()?;
        patch::apply_patch(&mut editable, patch, &catalog)?;
        let serialized = editable.to_string();
        let (new_document, _) = document::parse_configuration(&serialized)?;
        writer::write_atomically(&self.canonical_path, serialized.as_bytes())?;
        lock.commit();
        Ok(Configuration {
            source: ConfigSource {
                path: self.source.path.clone(),
                kind: self.source.kind,
                initialized: false,
            },
            document: new_document,
            revision: Some(writer::content_revision(serialized.as_bytes())),
            requested_path: self.requested_path.clone(),
            canonical_path: self.canonical_path.clone(),
        })
    }
}

/// Resolve the effective environment: invocation override -> file default ->
/// production. There is no fallback for an unknown requested name.
pub fn resolve_environment(
    config: &ConfigSnapshot,
    invocation_override: Option<&str>,
) -> CoreResult<EnvironmentResolution> {
    resolve_environment_inner(config, invocation_override).map_err(Into::into)
}

fn resolve_environment_inner(
    config: &ConfigSnapshot,
    invocation_override: Option<&str>,
) -> Result<EnvironmentResolution, ConfigurationError> {
    let catalog = document::catalog()?;
    let (environment, selection_source) = match invocation_override {
        Some(name) => {
            document::validate_identifier(name)?;
            (name.to_string(), SelectionSource::Invocation)
        }
        None => match &config.document.environment {
            Some(name) => (name.clone(), SelectionSource::File),
            None => ("production".to_string(), SelectionSource::Default),
        },
    };
    let definition_source;
    let definition = if let Some(builtin) = catalog.get(&environment) {
        definition_source = DefinitionSource::Builtin;
        let user = config.document.environments.get(&environment);
        let tls = user.and_then(|entry| entry.tls.clone()).or_else(|| {
            let provider = TlsProvider::parse(&builtin.tls.ca_provider)?;
            Some(TlsConfig {
                ca_provider: provider,
                ca_file: builtin.tls.ca_file.clone(),
            })
        });
        EnvironmentDefinition {
            id: builtin.id.clone(),
            mode: Some(EnvironmentMode::Account),
            label: user
                .and_then(|entry| entry.label.clone())
                .or_else(|| Some(builtin.label.clone())),
            api_origin: Some(builtin.api_origin.clone()),
            issuer: Some(builtin.issuer.clone()),
            native_client_id: Some(builtin.native_client_id.clone()),
            environment: Some(builtin.environment.clone()),
            tls,
        }
    } else if let Some(custom) = config.document.environments.get(&environment) {
        definition_source = DefinitionSource::File;
        let mut definition = custom.clone();
        definition.tls = Some(custom.tls.clone().unwrap_or(TlsConfig {
            ca_provider: TlsProvider::System,
            ca_file: None,
        }));
        definition
    } else {
        return Err(ConfigurationError::UnknownEnvironment { environment });
    };
    Ok(EnvironmentResolution {
        environment,
        selection_source,
        definition_source,
        definition,
    })
}

fn field_provenance(
    document: &ConfigurationDocument,
    resolution: &EnvironmentResolution,
) -> BTreeMap<String, FieldSource> {
    let mut provenance = BTreeMap::new();
    let builtin = resolution.definition_source == DefinitionSource::Builtin;
    let user = document.environments.get(&resolution.environment);
    let user_tls = user.is_some_and(|entry| entry.tls.is_some());
    let user_label = user.is_some_and(|entry| entry.label.is_some());
    let file_or_default = |present: bool| {
        if present {
            FieldSource::File
        } else {
            FieldSource::Default
        }
    };
    provenance.insert(
        "id".into(),
        if builtin {
            FieldSource::Catalog
        } else {
            FieldSource::File
        },
    );
    provenance.insert(
        "mode".into(),
        if builtin {
            FieldSource::Catalog
        } else {
            FieldSource::File
        },
    );
    provenance.insert(
        "label".into(),
        if builtin {
            if user_label {
                FieldSource::File
            } else {
                FieldSource::Catalog
            }
        } else {
            file_or_default(user_label)
        },
    );
    for field in ["api_origin", "issuer", "native_client_id"] {
        let present = user.is_some_and(|entry| match field {
            "api_origin" => entry.api_origin.is_some(),
            "issuer" => entry.issuer.is_some(),
            _ => entry.native_client_id.is_some(),
        });
        provenance.insert(
            field.into(),
            if builtin {
                FieldSource::Catalog
            } else {
                file_or_default(present)
            },
        );
    }
    provenance.insert(
        "environment".into(),
        if builtin {
            FieldSource::Catalog
        } else {
            FieldSource::Default
        },
    );
    provenance.insert(
        "tls".into(),
        if builtin {
            if user_tls {
                FieldSource::File
            } else {
                FieldSource::Catalog
            }
        } else {
            file_or_default(user_tls)
        },
    );
    provenance
}

fn select_path(
    options: &ConfigOpenOptions<'_>,
) -> Result<(PathBuf, ConfigSourceKind), ConfigurationError> {
    if let Some(path) = options.explicit_path {
        if path.as_os_str().is_empty() {
            return Err(ConfigurationError::MissingConfig);
        }
        return Ok((path.to_path_buf(), ConfigSourceKind::Explicit));
    }
    if let Some(value) = std::env::var_os("WHALEREIN_CONFIG") {
        if !value.is_empty() {
            return Ok((PathBuf::from(value), ConfigSourceKind::Environment));
        }
    }
    Ok((default_config_path()?, ConfigSourceKind::Default))
}

/// Default per-device configuration file. There is no current-directory search.
pub fn default_config_path() -> Result<PathBuf, ConfigurationError> {
    home_dir()
        .map(|home| home.join(".whalerein").join("config.toml"))
        .ok_or(ConfigurationError::MissingConfig)
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
            .map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
}

pub(crate) fn canonical_target(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|directory| directory.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}
