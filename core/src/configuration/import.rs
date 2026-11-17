//! Explicit legacy-adoption preview and confirmed import.
//!
//! Preview is a pure, read-only projection over the five legacy layouts listed
//! in the F-03 inventory. It never creates a config default, lock, profile
//! directory, account, key or secret entry, and it never executes YAML/JS
//! expressions found in a Cordis profile.
//!
//! Confirmed import reuses the existing `<config>.lock.sqlite` sidecar from
//! [`super::writer`], rechecks source and target revisions, publishes a
//! validated configuration durably and advances an idempotent journal at
//! `<canonical-config>.import-journal.json` (mode 0600). The completed journal
//! is the adoption marker.
//!
//! IMPORTER PROHIBITIONS (asserted by tests): this module must not call
//! `security::device::load_or_create`, `Profiles::{add_profile,select_environment,
//! activate}`, any credential `set`/`prepareProfileIdentity`, Host identity
//! creation or enrollment-journal writes. Legacy files, state directories and
//! secret namespaces are never moved, deleted or rewritten. Profile activation
//! remains an explicit, separate operation performed through the existing
//! activation boundary (`crate::profiles::Profile...`), not by import.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml_edit::{value, DocumentMut, Item, Table};

use crate::api::CoreResult;

use super::document::{self, EnvironmentMode, TlsConfig, TlsProvider};
use super::writer;
use super::{canonical_target, ConfigurationError, DefinitionSource};

/// Journal schema written next to the canonical target.
pub const IMPORT_JOURNAL_SCHEMA: &str = "whalerein/v1/import-journal";
/// Only version 1 is understood. A newer value refuses import rather than
/// guessing at future semantics.
pub const IMPORT_JOURNAL_VERSION: i64 = 1;
const JOURNAL_SUFFIX: &str = ".import-journal.json";

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// The five legacy layouts an explicit adoption may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacySourceKind {
    /// Flat Core `config.json` plus ProjectDirs state.
    CoreFlat,
    /// Core `profiles.json` registry/custom descriptors.
    CoreProfiles,
    /// Evaluated DSH managed profile plus Host registry.
    CordisManaged,
    /// Evaluated DSH Standalone profile with manual routes.
    CordisStandalone,
    /// Unmerged generated local-environment prototype.
    Prototype,
}

/// One detected legacy layout. Detection only checks path existence; it never
/// reads a secret file, executes a profile expression or mutates anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacySource {
    pub kind: LegacySourceKind,
    /// Directory that owns the legacy metadata/state.
    pub root: PathBuf,
    /// Non-secret files that prove the source shape.
    pub evidence: Vec<PathBuf>,
}

/// Detect the first legacy layout under `state_root`, read-only and
/// deterministically ordered: prototype, flat Core, Core profiles, DSH
/// managed, DSH Standalone.
pub fn detect_legacy(state_root: &Path) -> Option<LegacySource> {
    let root = absolute(state_root);
    let mut evidence = Vec::new();
    for name in ["environment.json", "local-environment.json"] {
        let path = root.join(name);
        if path.is_file() {
            evidence.push(path);
            return Some(LegacySource {
                kind: LegacySourceKind::Prototype,
                root,
                evidence,
            });
        }
    }

    if flat_core_evidence(&root).is_some() {
        return Some(LegacySource {
            kind: LegacySourceKind::CoreFlat,
            evidence: flat_core_evidence(&root).unwrap_or_default(),
            root,
        });
    }

    let profile_candidates = [
        root.join("profiles.json"),
        root.join("cli").join("profiles.json"),
        root.join("clients").join("cli").join("profiles.json"),
    ];
    if let Some(path) = first_file(&profile_candidates) {
        let owner = path.parent().map(Path::to_path_buf).unwrap_or(root);
        return Some(LegacySource {
            kind: LegacySourceKind::CoreProfiles,
            evidence: vec![path, owner.join("profiles")],
            root: owner,
        });
    }

    let managed_candidates = [
        root.join("host-profiles.json"),
        root.join("dsh").join("host-profiles.json"),
        root.join("clients").join("dsh").join("host-profiles.json"),
    ];
    if let Some(path) = first_file(&managed_candidates) {
        let owner = path.parent().map(Path::to_path_buf).unwrap_or(root);
        return Some(LegacySource {
            kind: LegacySourceKind::CordisManaged,
            evidence: vec![path.clone(), owner.join("cordis.patch.yml")],
            root: owner,
        });
    }

    let standalone_candidates = [
        root.join("cordis.patch.yml"),
        root.join("dsh").join("cordis.patch.yml"),
    ];
    if let Some(path) = first_file(&standalone_candidates) {
        let owner = path.parent().map(Path::to_path_buf).unwrap_or(root);
        return Some(LegacySource {
            kind: LegacySourceKind::CordisStandalone,
            evidence: vec![path],
            root: owner,
        });
    }
    None
}

/// Probe used by `ConfigOpenOptions.legacy_state_probe`: missing-default
/// startup must refuse to initialize when a legacy layout exists either at the
/// supplied parent or at the old ProjectDirs data root.
pub fn legacy_state_present(state_root: &Path) -> bool {
    if detect_legacy(state_root).is_some() {
        return true;
    }
    let project_root = crate::api::config::default_data_dir();
    project_root != state_root && detect_legacy(&project_root).is_some()
}

fn flat_core_evidence(root: &Path) -> Option<Vec<PathBuf>> {
    const MARKERS: [&str; 13] = [
        "config.json",
        "cache.sqlite3",
        "cache.sqlite3-journal",
        "cache.sqlite3-wal",
        "cache.sqlite3-shm",
        "commands.sqlite",
        "unconfirmed-commands",
        "admission-revoked",
        "revocation-retention-warning",
        "pairing-epoch",
        "identity.lock",
        "admission.lock",
        "last-failure.json",
    ];
    let mut evidence: Vec<PathBuf> = MARKERS
        .iter()
        .map(|name| root.join(name))
        .filter(|path| path.exists())
        .collect();
    if evidence.is_empty() {
        None
    } else {
        evidence.sort();
        Some(evidence)
    }
}

fn first_file(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

// ---------------------------------------------------------------------------
// Preview types
// ---------------------------------------------------------------------------

/// Options for one read-only preview.
#[derive(Debug, Clone, Copy)]
pub struct ImportPreviewOptions<'a> {
    pub state_root: &'a Path,
    /// Canonical (or to-be-canonicalized) TOML target.
    pub target_path: &'a Path,
    /// Revision the caller observed; a mismatch is a conflict, not an overwrite.
    pub target_revision: Option<&'a str>,
    /// Whether the caller has a usable secure store for a literal-token handoff.
    pub secure_store_available: bool,
}

/// Preview readiness. `RetainOnly` is a valid outcome: identity references are
/// recorded without converting the legacy source to managed TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportPreviewStatus {
    Ready,
    RetainOnly,
    Blocked,
    Conflict,
}

/// How seriously one preview issue blocks confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportIssueKind {
    Conflict,
    Blocked,
    Unsupported,
}

/// One non-secret preview issue. Messages never contain tokens, keys or file
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportConflict {
    pub code: String,
    pub message: String,
    pub kind: ImportIssueKind,
    pub path: Option<PathBuf>,
}

/// A non-secret secure-store handoff requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecureStoreHandoff {
    pub required: bool,
    pub satisfied: bool,
    /// Purpose/namespace names only, never values.
    pub namespaces: Vec<String>,
}

/// One proposed environment. Built-in entries carry only label/TLS
/// specialization; custom entries carry their complete authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposedEnvironment {
    pub name: String,
    pub definition_source: DefinitionSource,
    pub label: Option<String>,
    pub mode: Option<EnvironmentMode>,
    pub api_origin: Option<String>,
    pub issuer: Option<String>,
    pub native_client_id: Option<String>,
    pub tls: Option<TlsConfig>,
}

/// Non-secret settings the confirmed config would contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposedConfiguration {
    pub default_environment: Option<String>,
    pub environments: Vec<ProposedEnvironment>,
}

/// Typed identity/state reference retained by reference, never copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RetainedReference {
    StatePath { path: PathBuf },
    ProfileId { profile_id: String },
    CredentialNamespace { namespace: String },
    SecretSlot { slot: String },
    PendingJournal { path: PathBuf },
    TrustFile { path: PathBuf },
}

/// Deterministic, read-only import plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportPreview {
    pub preview_id: String,
    pub source: LegacySource,
    pub source_revision: String,
    pub target_path: PathBuf,
    pub target_revision: Option<String>,
    pub status: ImportPreviewStatus,
    pub proposed: Option<ProposedConfiguration>,
    pub retained_references: Vec<RetainedReference>,
    pub conflicts: Vec<ImportConflict>,
    pub secure_store_handoff: Option<SecureStoreHandoff>,
}

/// Explicit confirmation request. The caller echoes the preview the user
/// approved; commit revalidates its revisions rather than trusting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportCommit {
    pub preview: ImportPreview,
}

impl ImportCommit {
    pub fn from_preview(preview: &ImportPreview) -> Self {
        Self {
            preview: preview.clone(),
        }
    }
}

/// Options for confirmation.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportCommitOptions {
    pub secure_store_available: bool,
}

/// Terminal import outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcomeStatus {
    Imported,
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportOutcome {
    pub status: ImportOutcomeStatus,
    pub preview_id: String,
    pub target_path: PathBuf,
    pub target_revision: Option<String>,
    pub journal_path: PathBuf,
    pub journal_state: ImportJournalState,
    pub retained_references: Vec<RetainedReference>,
    /// The journal was advanced during this call instead of a fresh publish.
    pub recovered: bool,
}

/// Journal state machine. `previewed` is written before `prepared` so a crash
/// during plan persistence is distinguishable from a crash during publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportJournalState {
    Previewed,
    Prepared,
    Published,
    Activated,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LegacySettingsStatus {
    Active,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImportJournalSource {
    kind: LegacySourceKind,
    root: PathBuf,
    revision: String,
    profile_ids: Vec<String>,
    state_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImportJournalTarget {
    path: PathBuf,
    revision_before: Option<String>,
    revision_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImportJournalEnvironment {
    name: String,
    definition_source: DefinitionSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImportJournalStep {
    at_unix_ms: u64,
    state: ImportJournalState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportJournal {
    schema: String,
    version: i64,
    preview_id: String,
    state: ImportJournalState,
    source: ImportJournalSource,
    target: ImportJournalTarget,
    environment: Option<ImportJournalEnvironment>,
    retained: Vec<RetainedReference>,
    legacy_settings: LegacySettingsStatus,
    steps: Vec<ImportJournalStep>,
}

/// Recovery result for one target. A conflict is reported as data because the
/// caller needs the retained source paths even when adoption cannot proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportRecoveryStatus {
    NotImported,
    Imported,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportRecovery {
    pub status: ImportRecoveryStatus,
    pub journal_state: Option<ImportJournalState>,
    pub preview_id: Option<String>,
    pub target_revision: Option<String>,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// Build a read-only preview. No lock, directory, file, account, key or
/// network access is created; the only writes happen in [`commit_import`].
pub fn preview_import(options: &ImportPreviewOptions<'_>) -> CoreResult<ImportPreview> {
    let source =
        detect_legacy(options.state_root).ok_or_else(|| ConfigurationError::ImportUnsupported {
            message: "no legacy client state was detected".into(),
        })?;
    let target_path = canonical_target(options.target_path);
    let target_revision = read_target_revision(&target_path)?;

    let mut plan = build_plan(&source, options.secure_store_available);
    if let Some(expected) = options.target_revision {
        if target_revision.as_deref() != Some(expected) {
            plan.issues.push(ImportConflict {
                code: "target_revision_conflict".into(),
                message: "the configuration target changed after the preview began".into(),
                kind: ImportIssueKind::Conflict,
                path: Some(target_path.clone()),
            });
        }
    }
    let source_revision = plan.source_revision.clone();
    let (status, conflicts, handoff, proposed, retained) = finalize_plan(plan);
    let preview_id = preview_id(
        source.kind,
        &source_revision,
        &target_path,
        target_revision.as_deref(),
    );
    Ok(ImportPreview {
        preview_id,
        source,
        source_revision,
        target_path,
        target_revision,
        status,
        proposed,
        retained_references: retained,
        conflicts,
        secure_store_handoff: handoff,
    })
}

fn preview_id(
    kind: LegacySourceKind,
    source_revision: &str,
    target_path: &Path,
    target_revision: Option<&str>,
) -> String {
    let value = serde_json::json!({
        "source_kind": kind,
        "source_revision": source_revision,
        "target_path": target_path.to_string_lossy(),
        "target_revision": target_revision,
    });
    digest_value(&value)
}

// ---------------------------------------------------------------------------
// Source projections
// ---------------------------------------------------------------------------

struct SourcePlan {
    source_revision: String,
    proposed: Option<ProposedConfiguration>,
    retained: Vec<RetainedReference>,
    issues: Vec<ImportConflict>,
    literal_secret: bool,
    handoff_satisfied: bool,
    handoff_namespaces: Vec<String>,
    conversion_possible: bool,
}

fn finalize_plan(
    plan: SourcePlan,
) -> (
    ImportPreviewStatus,
    Vec<ImportConflict>,
    Option<SecureStoreHandoff>,
    Option<ProposedConfiguration>,
    Vec<RetainedReference>,
) {
    let issues = plan.issues;
    let mut handoff = None;
    if plan.literal_secret {
        handoff = Some(SecureStoreHandoff {
            required: true,
            satisfied: plan.handoff_satisfied,
            namespaces: plan.handoff_namespaces,
        });
    }
    let status = if issues.iter().any(|issue| {
        matches!(
            issue.kind,
            ImportIssueKind::Unsupported | ImportIssueKind::Conflict
        )
    }) {
        ImportPreviewStatus::Conflict
    } else if issues
        .iter()
        .any(|issue| issue.kind == ImportIssueKind::Blocked)
    {
        ImportPreviewStatus::Blocked
    } else if plan.conversion_possible {
        ImportPreviewStatus::Ready
    } else {
        ImportPreviewStatus::RetainOnly
    };
    (status, issues, handoff, plan.proposed, plan.retained)
}

fn build_plan(source: &LegacySource, secure_store_available: bool) -> SourcePlan {
    let plan = match source.kind {
        LegacySourceKind::CoreFlat => plan_core_flat(source),
        LegacySourceKind::CoreProfiles => plan_core_profiles(source),
        LegacySourceKind::CordisManaged => plan_cordis_managed(source),
        LegacySourceKind::CordisStandalone => plan_cordis_standalone(source),
        LegacySourceKind::Prototype => plan_prototype(source),
    };
    let mut plan = plan;
    plan.handoff_satisfied = secure_store_available || !plan.literal_secret;
    if plan.literal_secret && !secure_store_available {
        plan.issues.push(ImportConflict {
            code: "missing_secure_store".into(),
            message: "the legacy source contains a literal connection token but no secure store is available"
                .into(),
            kind: ImportIssueKind::Blocked,
            path: None,
        });
    }
    plan
}

fn plan_core_flat(source: &LegacySource) -> SourcePlan {
    let root = &source.root;
    let mut retained = vec![
        RetainedReference::StatePath { path: root.clone() },
        RetainedReference::ProfileId {
            profile_id: "default".into(),
        },
    ];
    let mut issues = Vec::new();
    let mut projection = BTreeMap::new();
    projection.insert(
        "root".to_string(),
        serde_json::Value::String(display_path(root)),
    );
    let markers: Vec<String> = source
        .evidence
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .collect();
    projection.insert(
        "markers".to_string(),
        serde_json::Value::Array(markers.into_iter().map(serde_json::Value::String).collect()),
    );

    let config_path = root.join("config.json");
    let mut device_id = None;
    if config_path.is_file() {
        match read_json(&config_path) {
            Ok(value) => {
                if value.get("schema").and_then(|v| v.as_str())
                    != Some(crate::api::config::CONFIG_SCHEMA)
                {
                    issues.push(issue(
                        "unsupported_source_version",
                        ImportIssueKind::Unsupported,
                        "the legacy Core configuration schema is not supported",
                        Some(config_path.clone()),
                    ));
                }
                let sanitized = core_flat_projection(&value);
                if let Some(id) = sanitized.get("device_id").and_then(|v| v.as_str()) {
                    device_id = Some(id.to_string());
                }
                projection.insert("config".to_string(), sanitized);
            }
            Err(()) => issues.push(issue(
                "source_invalid",
                ImportIssueKind::Conflict,
                "legacy Core config.json is not a readable non-secret document",
                Some(config_path.clone()),
            )),
        }
    }

    if let Some(device_id) = device_id {
        for slot in [
            crate::security::keys::pairing_history(&device_id),
            crate::security::keys::pending_pairing(&device_id),
            crate::security::keys::device_private_key(&device_id),
            crate::security::keys::device_token(&device_id),
            crate::security::keys::cache_key(&device_id),
            crate::security::keys::diagnostic_salt(&device_id),
        ] {
            retained.push(RetainedReference::SecretSlot { slot });
        }
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        retained.push(RetainedReference::SecretSlot {
            slot: format!(
                "whalerein:v2:identity:{}",
                sha256_hex(canonical.to_string_lossy().as_bytes())
            ),
        });
    }
    for journal in ["unconfirmed-commands", "pairing-epoch", "last-failure.json"] {
        let path = root.join(journal);
        if path.exists() {
            retained.push(RetainedReference::PendingJournal { path });
        }
    }
    SourcePlan {
        source_revision: digest_btree(&projection),
        proposed: None,
        retained,
        issues,
        literal_secret: false,
        handoff_satisfied: true,
        handoff_namespaces: Vec::new(),
        conversion_possible: false,
    }
}

fn core_flat_projection(value: &serde_json::Value) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for key in [
        "schema",
        "host_id",
        "device_id",
        "protocol_version",
        "host_fingerprint",
        "host_fingerprints",
    ] {
        if let Some(field) = value.get(key) {
            map.insert(key.to_string(), redact_value(field.clone()));
        }
    }
    for key in ["access_url", "mqtt_url"] {
        if let Some(field) = value.get(key).and_then(|v| v.as_str()) {
            let safe = sanitize_route(field);
            map.insert(
                key.to_string(),
                serde_json::Value::String(safe.unwrap_or_else(|| "redacted".into())),
            );
        }
    }
    serde_json::Value::Object(map)
}

fn plan_core_profiles(source: &LegacySource) -> SourcePlan {
    let root = &source.root;
    let registry_path = root.join(crate::profiles::PROFILES_FILE);
    let mut retained = vec![
        RetainedReference::StatePath { path: root.clone() },
        RetainedReference::StatePath {
            path: registry_path.clone(),
        },
    ];
    let mut issues = Vec::new();
    let mut projection = BTreeMap::new();
    projection.insert(
        "root".to_string(),
        serde_json::Value::String(display_path(root)),
    );

    let bytes = match std::fs::read(&registry_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return SourcePlan {
                source_revision: digest_bytes(&[]),
                proposed: None,
                retained,
                issues: vec![issue(
                    "source_invalid",
                    ImportIssueKind::Conflict,
                    "the Core profile registry could not be read",
                    Some(registry_path.clone()),
                )],
                literal_secret: false,
                handoff_satisfied: true,
                handoff_namespaces: Vec::new(),
                conversion_possible: false,
            }
        }
    };
    let registry: crate::profiles::ProfilesRegistry = match serde_json::from_slice(&bytes) {
        Ok(registry) => registry,
        Err(_) => {
            return SourcePlan {
                source_revision: writer::content_revision(&bytes),
                proposed: None,
                retained,
                issues: vec![issue(
                    "source_invalid",
                    ImportIssueKind::Conflict,
                    "the Core profile registry is not valid JSON",
                    Some(registry_path.clone()),
                )],
                literal_secret: false,
                handoff_satisfied: true,
                handoff_namespaces: Vec::new(),
                conversion_possible: false,
            }
        }
    };
    if registry.schema != crate::profiles::PROFILES_SCHEMA {
        issues.push(issue(
            "unsupported_source_version",
            ImportIssueKind::Unsupported,
            "the Core profile registry schema is not supported",
            Some(registry_path.clone()),
        ));
    }
    if let Err(error) = registry.validate() {
        issues.push(issue(
            "profile_registry_invalid",
            ImportIssueKind::Conflict,
            &format!("the Core profile registry is invalid: {error}"),
            Some(registry_path),
        ));
    }
    projection.insert(
        "registry".to_string(),
        serde_json::to_value(&registry).unwrap_or(serde_json::Value::Null),
    );

    let mut environments: Vec<ProposedEnvironment> = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut active_environment = None;
    for record in &registry.profiles {
        retained.push(RetainedReference::ProfileId {
            profile_id: record.id.clone(),
        });
        let state_dir = if record.legacy {
            root.clone()
        } else {
            root.join(crate::profiles::PROFILES_DIR).join(&record.id)
        };
        retained.push(RetainedReference::StatePath { path: state_dir });
        for journal in ["unconfirmed-commands", "pairing-epoch"] {
            let path = root.join(journal);
            if record.legacy && path.exists() {
                retained.push(RetainedReference::PendingJournal { path });
            }
        }
        if let Some(service) = &record.service {
            let official_name = match service.kind {
                crate::account::ServiceKind::OfficialProduction => Some("production"),
                crate::account::ServiceKind::OfficialStaging => Some("staging"),
                crate::account::ServiceKind::OfficialLocal => Some("local"),
                crate::account::ServiceKind::Custom => None,
            };
            let name = if !record.environment.is_empty() {
                record.environment.clone()
            } else if let Some(name) = official_name {
                name.to_string()
            } else {
                sanitized_name(&record.name)
            };
            let builtin = official_name.is_some();
            {
                let proposed = if builtin {
                    ProposedEnvironment {
                        name: name.clone(),
                        definition_source: DefinitionSource::Builtin,
                        label: None,
                        mode: None,
                        api_origin: None,
                        issuer: None,
                        native_client_id: None,
                        tls: None,
                    }
                } else {
                    ProposedEnvironment {
                        name: name.clone(),
                        definition_source: DefinitionSource::File,
                        label: Some(record.name.clone()),
                        mode: Some(EnvironmentMode::Account),
                        api_origin: service.api_origin.clone(),
                        issuer: service.issuer.clone(),
                        native_client_id: service.native_client_id.clone(),
                        tls: None,
                    }
                };
                if name.is_empty() {
                    issues.push(issue(
                        "invalid_environment_name",
                        ImportIssueKind::Conflict,
                        "a custom profile name cannot be represented as an environment identifier",
                        None,
                    ));
                } else if !names.insert(name.to_string()) {
                    issues.push(issue(
                        "ambiguous_environment",
                        ImportIssueKind::Conflict,
                        "two legacy profiles map to the same environment name",
                        None,
                    ));
                } else {
                    environments.push(proposed);
                }
                if registry.active == record.id {
                    active_environment = Some(name.clone());
                }
            }
        }
    }

    let conversion_possible = !environments.is_empty();
    let proposed = conversion_possible.then_some(ProposedConfiguration {
        default_environment: active_environment,
        environments,
    });
    SourcePlan {
        source_revision: digest_btree(&projection),
        proposed,
        retained,
        issues,
        literal_secret: false,
        handoff_satisfied: true,
        handoff_namespaces: Vec::new(),
        conversion_possible,
    }
}

fn plan_cordis_standalone(source: &LegacySource) -> SourcePlan {
    let root = &source.root;
    let profile_path = root.join("cordis.patch.yml");
    let mut issues = Vec::new();
    let mut retained = vec![
        RetainedReference::StatePath { path: root.clone() },
        RetainedReference::ProfileId {
            profile_id: "standalone".into(),
        },
        RetainedReference::CredentialNamespace {
            namespace: "whalerein-dsh".into(),
        },
    ];
    let mut projection = BTreeMap::new();
    projection.insert(
        "root".to_string(),
        serde_json::Value::String(display_path(root)),
    );
    scan_cordis_profile(&profile_path, &mut projection, &mut issues);
    let mut literal = false;
    if let Some(host_id) = root
        .join("host-id")
        .is_file()
        .then_some(root.join("host-id"))
    {
        if let Ok(bytes) = std::fs::read(&host_id) {
            let text = String::from_utf8_lossy(&bytes).trim().to_string();
            if !text.is_empty() {
                projection.insert("host_id".into(), serde_json::Value::String(text));
            }
            retained.push(RetainedReference::StatePath { path: host_id });
        }
    }
    for key in ["unconfirmed-commands", "pairing-epoch"] {
        let path = root.join(key);
        if path.exists() {
            retained.push(RetainedReference::PendingJournal { path });
        }
    }
    if profile_path.is_file() {
        if let Ok(text) = std::fs::read_to_string(&profile_path) {
            literal = has_literal_secret(&text);
        }
    }
    if !profile_path.is_file() {
        issues.push(issue(
            "source_invalid",
            ImportIssueKind::Conflict,
            "the DSH Standalone profile document is missing",
            Some(profile_path),
        ));
    }
    SourcePlan {
        source_revision: digest_btree(&projection),
        proposed: None,
        retained,
        issues,
        literal_secret: literal,
        handoff_satisfied: !literal,
        handoff_namespaces: vec![
            "whalerein-dsh".into(),
            "whalerein.host-private-key.v1".into(),
        ],
        conversion_possible: false,
    }
}

fn plan_cordis_managed(source: &LegacySource) -> SourcePlan {
    let root = &source.root;
    let registry_path = root.join("host-profiles.json");
    let mut issues = Vec::new();
    let mut retained = vec![
        RetainedReference::StatePath { path: root.clone() },
        RetainedReference::StatePath {
            path: registry_path.clone(),
        },
    ];
    let mut projection = BTreeMap::new();
    projection.insert(
        "root".to_string(),
        serde_json::Value::String(display_path(root)),
    );
    let bytes = match std::fs::read(&registry_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return SourcePlan {
                source_revision: digest_bytes(&[]),
                proposed: None,
                retained,
                issues: vec![issue(
                    "source_invalid",
                    ImportIssueKind::Conflict,
                    "the Host profile registry could not be read",
                    Some(registry_path.clone()),
                )],
                literal_secret: false,
                handoff_satisfied: true,
                handoff_namespaces: Vec::new(),
                conversion_possible: false,
            }
        }
    };
    let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return SourcePlan {
                source_revision: writer::content_revision(&bytes),
                proposed: None,
                retained,
                issues: vec![issue(
                    "source_invalid",
                    ImportIssueKind::Conflict,
                    "the Host profile registry is not valid JSON",
                    Some(registry_path.clone()),
                )],
                literal_secret: false,
                handoff_satisfied: true,
                handoff_namespaces: Vec::new(),
                conversion_possible: false,
            }
        }
    };
    match host_registry_projection(&value) {
        Ok(sanitized) => {
            projection.insert("registry".to_string(), sanitized);
        }
        Err(message) => issues.push(issue(
            "unsupported_source_version",
            ImportIssueKind::Unsupported,
            &message,
            Some(registry_path.clone()),
        )),
    }
    let profiles = value
        .get("profiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if profiles.is_empty() {
        issues.push(issue(
            "source_invalid",
            ImportIssueKind::Conflict,
            "the Host profile registry has no profiles",
            Some(registry_path.clone()),
        ));
    }
    let mut environments: Vec<ProposedEnvironment> = Vec::new();
    let mut names = BTreeSet::new();
    for record in &profiles {
        let id = record
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            issues.push(issue(
                "source_invalid",
                ImportIssueKind::Conflict,
                "a Host registry profile is missing its opaque id",
                None,
            ));
            continue;
        }
        retained.push(RetainedReference::ProfileId {
            profile_id: id.clone(),
        });
        let state_dir = host_state_dir(root, record);
        retained.push(RetainedReference::StatePath {
            path: state_dir.clone(),
        });
        retained.push(RetainedReference::CredentialNamespace {
            namespace: format!("whalerein-host/v1/dsh/{id}"),
        });
        let pending = state_dir.join("enrollment-intent.json");
        if pending.exists() {
            retained.push(RetainedReference::PendingJournal { path: pending });
        }
        let environment = record
            .get("environment")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !matches!(environment, "production" | "staging" | "local") {
            issues.push(issue(
                "unsupported_environment",
                ImportIssueKind::Unsupported,
                "a Host profile declares an environment that managed TOML cannot represent",
                None,
            ));
            continue;
        }
        if environment == "local"
            && record
                .get("service")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                != Some("official-local")
        {
            issues.push(issue(
                "authority_mismatch",
                ImportIssueKind::Conflict,
                "a local Host profile does not use the built-in official-local service",
                None,
            ));
        }
        if let Some(expected) = verify_local_authority(root, record) {
            if !expected {
                issues.push(issue(
                    "authority_mismatch",
                    ImportIssueKind::Conflict,
                    "the Host service authority pin does not match the generated local authority",
                    Some(registry_path.clone()),
                ));
            }
        }
        if names.insert(environment.to_string()) {
            environments.push(ProposedEnvironment {
                name: environment.to_string(),
                definition_source: DefinitionSource::Builtin,
                label: None,
                mode: None,
                api_origin: None,
                issuer: None,
                native_client_id: None,
                tls: None,
            });
        }
    }
    let active_id = value
        .get("active")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !profiles
        .iter()
        .any(|record| record.get("id").and_then(|v| v.as_str()) == Some(active_id))
    {
        issues.push(issue(
            "source_invalid",
            ImportIssueKind::Conflict,
            "the Host profile registry active profile is unavailable",
            Some(registry_path.clone()),
        ));
    }
    let default_environment = profiles
        .iter()
        .find(|record| record.get("id").and_then(|v| v.as_str()) == Some(active_id))
        .and_then(|record| record.get("environment").and_then(|v| v.as_str()))
        .filter(|name| matches!(*name, "production" | "staging" | "local"))
        .map(str::to_string);
    let profile_path = root.join("cordis.patch.yml");
    if profile_path.is_file() {
        scan_cordis_profile(&profile_path, &mut projection, &mut issues);
    }
    let mut literal = false;
    if let Ok(text) = std::fs::read_to_string(&profile_path) {
        literal = has_literal_secret(&text);
    }
    if !profile_path.is_file() {
        issues.push(issue(
            "source_invalid",
            ImportIssueKind::Conflict,
            "the DSH managed profile document is missing",
            Some(profile_path),
        ));
    }
    let conversion_possible = !environments.is_empty();
    let proposed = conversion_possible.then_some(ProposedConfiguration {
        default_environment,
        environments,
    });
    SourcePlan {
        source_revision: digest_btree(&projection),
        proposed,
        retained,
        issues,
        literal_secret: literal,
        handoff_satisfied: !literal,
        handoff_namespaces: vec!["whalerein-host/v1/dsh".into()],
        conversion_possible,
    }
}

fn host_state_dir(root: &Path, record: &serde_json::Value) -> PathBuf {
    let state_dir = record
        .get("stateDir")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    if state_dir == "." || state_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(state_dir)
    }
}

/// `None` means there is no local environment document to compare against.
fn verify_local_authority(root: &Path, record: &serde_json::Value) -> Option<bool> {
    if record.get("environment").and_then(|v| v.as_str()) != Some("local") {
        return None;
    }
    let pin = record
        .get("bindings")
        .and_then(|v| v.get("serviceAuthority"))
        .and_then(|v| v.as_str())?;
    let doc_path = find_prototype_document(root)?;
    let doc = read_prototype_document(&doc_path).ok()?;
    let expected = digest_value(&serde_json::json!([
        doc.service,
        doc.api_origin,
        doc.issuer
    ]));
    Some(expected == format!("sha256:{pin}"))
}

fn find_prototype_document(root: &Path) -> Option<PathBuf> {
    let mut candidates = vec![
        root.join("environment.json"),
        root.join("local-environment.json"),
    ];
    if let Some(parent) = root.parent() {
        candidates.push(parent.join("environment.json"));
        candidates.push(parent.join("local-environment.json"));
        if let Some(grand) = parent.parent() {
            candidates.push(grand.join("environment.json"));
        }
    }
    first_file(&candidates)
}

fn plan_prototype(source: &LegacySource) -> SourcePlan {
    let root = &source.root;
    let doc_path = source
        .evidence
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| root.join("environment.json"));
    let mut retained = vec![
        RetainedReference::StatePath { path: root.clone() },
        RetainedReference::StatePath {
            path: doc_path.clone(),
        },
    ];
    let mut issues = Vec::new();
    match read_prototype_document(&doc_path) {
        Ok(doc) => {
            let state_root = absolute(&doc.state_root);
            retained.push(RetainedReference::StatePath {
                path: state_root.clone(),
            });
            let ca_file = absolute(&doc.ca_file);
            retained.push(RetainedReference::TrustFile {
                path: ca_file.clone(),
            });
            if !ca_file.is_file() {
                issues.push(issue(
                    "ca_file_missing",
                    ImportIssueKind::Conflict,
                    "the prototype trust file is unavailable; import cannot copy or invent a CA",
                    Some(ca_file.clone()),
                ));
            }
            for sub in ["cli", "dsh"] {
                let path = state_root.join(sub);
                if path.is_dir() {
                    retained.push(RetainedReference::StatePath { path });
                }
            }
            let mut projection = BTreeMap::new();
            projection.insert(
                "root".to_string(),
                serde_json::Value::String(display_path(root)),
            );
            projection.insert(
                "environment".to_string(),
                serde_json::json!({
                    "schema": doc.schema,
                    "environment": doc.environment,
                    "service": doc.service,
                    "api_origin": doc.api_origin,
                    "issuer": doc.issuer,
                    "ca_file": display_path(&doc.ca_file),
                    "state_root": display_path(&doc.state_root),
                }),
            );
            let catalog = match document::catalog() {
                Ok(catalog) => catalog,
                Err(_) => {
                    return SourcePlan {
                        source_revision: digest_btree(&projection),
                        proposed: None,
                        retained,
                        issues: vec![issue(
                            "catalog_invalid",
                            ImportIssueKind::Conflict,
                            "the embedded environment catalog is invalid",
                            None,
                        )],
                        literal_secret: false,
                        handoff_satisfied: true,
                        handoff_namespaces: Vec::new(),
                        conversion_possible: false,
                    }
                }
            };
            let builtin_local = catalog.get("local").cloned();
            let authority_matches = builtin_local.is_some_and(|local| {
                local.api_origin == doc.api_origin && local.issuer == doc.issuer
            });
            if !authority_matches {
                issues.push(issue(
                    "authority_mismatch",
                    ImportIssueKind::Conflict,
                    "the generated prototype authority does not match the built-in local catalog; use a separately named custom environment",
                    Some(doc_path.clone()),
                ));
            }
            let conversion_possible = authority_matches;
            let proposed = conversion_possible.then(|| ProposedConfiguration {
                default_environment: Some("local".into()),
                environments: vec![ProposedEnvironment {
                    name: "local".into(),
                    definition_source: DefinitionSource::Builtin,
                    label: None,
                    mode: None,
                    api_origin: None,
                    issuer: None,
                    native_client_id: None,
                    tls: Some(TlsConfig {
                        ca_provider: TlsProvider::File,
                        ca_file: Some(display_path(&ca_file)),
                    }),
                }],
            });
            SourcePlan {
                source_revision: digest_btree(&projection),
                proposed,
                retained,
                issues,
                literal_secret: false,
                handoff_satisfied: true,
                handoff_namespaces: Vec::new(),
                conversion_possible,
            }
        }
        Err(message) => SourcePlan {
            source_revision: digest_bytes(std::fs::read(&doc_path).unwrap_or_default().as_slice()),
            proposed: None,
            retained,
            issues: vec![issue(
                "source_invalid",
                ImportIssueKind::Conflict,
                &message,
                Some(doc_path),
            )],
            literal_secret: false,
            handoff_satisfied: true,
            handoff_namespaces: Vec::new(),
            conversion_possible: false,
        },
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrototypeDocument {
    schema: String,
    environment: String,
    service: String,
    api_origin: String,
    issuer: String,
    ca_file: PathBuf,
    state_root: PathBuf,
}

fn read_prototype_document(path: &Path) -> Result<PrototypeDocument, String> {
    let bytes = std::fs::read(path)
        .map_err(|_| "the generated local-environment document could not be read".to_string())?;
    let doc: PrototypeDocument = serde_json::from_slice(&bytes)
        .map_err(|_| "the generated local-environment document is not valid".to_string())?;
    let valid = doc.schema == "whalerein/v1/local-environment"
        && doc.environment == "local"
        && doc.service == "official-local"
        && doc.api_origin.starts_with("https://app.")
        && doc.api_origin.ends_with(".test")
        && doc.issuer.ends_with("/oidc")
        && doc.ca_file.is_absolute()
        && doc.state_root.is_absolute();
    if !valid {
        return Err(
            "the generated local-environment document does not satisfy the prototype contract"
                .into(),
        );
    }
    Ok(doc)
}

fn scan_cordis_profile(
    profile_path: &Path,
    projection: &mut BTreeMap<String, serde_json::Value>,
    issues: &mut Vec<ImportConflict>,
) {
    let text = match std::fs::read_to_string(profile_path) {
        Ok(text) => text,
        Err(_) => return,
    };
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || (trimmed.starts_with('-') && !trimmed.contains(':')) {
            continue;
        }
        let Some((key, raw)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if !matches!(
            key,
            "connection_mode"
                | "environment"
                | "state_root"
                | "state_dir"
                | "profile_id"
                | "ca_file"
                | "local_environment_file"
                | "server"
                | "access_port"
                | "mqtt_port"
                | "host_id"
                | "adapter"
                | "log_level"
                | "allow_insecure_mqtt"
                | "developer_tools_enabled"
        ) {
            continue;
        }
        let raw = raw.trim();
        let scalar = raw.split(" #").next().unwrap_or("").trim();
        if scalar.starts_with("!!js") {
            values.insert(
                key.to_string(),
                serde_json::Value::String("<evaluated>".into()),
            );
            continue;
        }
        let unquoted = scalar.trim_matches('"').trim_matches('\'').trim();
        if unquoted.is_empty() {
            values.insert(key.to_string(), serde_json::Value::String(String::new()));
        } else {
            values.insert(
                key.to_string(),
                serde_json::Value::String(unquoted.to_string()),
            );
        }
    }
    let connection_mode = values.get("connection_mode").and_then(|v| v.as_str());
    if connection_mode.is_some_and(|value| value != "managed" && value != "standalone") {
        issues.push(issue(
            "unsupported_source_version",
            ImportIssueKind::Unsupported,
            "the Cordis connection mode is not supported",
            Some(profile_path.to_path_buf()),
        ));
    }
    projection.insert(
        "cordis".to_string(),
        serde_json::Value::Object(values.into_iter().collect()),
    );
}

fn has_literal_secret(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            return false;
        }
        let Some((key, raw)) = trimmed.split_once(':') else {
            return false;
        };
        let key = key.trim();
        if !matches!(
            key,
            "host_token"
                | "admin_token"
                | "access_token"
                | "broker_password"
                | "password"
                | "client_secret"
        ) {
            return false;
        }
        let value = raw.trim().split(" #").next().unwrap_or("").trim();
        if value.is_empty() || value.starts_with("!!js") || value == "\"\"" || value == "''" {
            return false;
        }
        !value.starts_with('$')
    })
}

fn host_registry_projection(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    if value.get("schema").and_then(|v| v.as_str()) != Some("whalerein-host/v1/profiles") {
        return Err("the Host profile registry schema is not supported".into());
    }
    let version = value
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if version != 1 {
        return Err("the Host profile registry version is not supported".into());
    }
    let mut map = serde_json::Map::new();
    for key in [
        "schema",
        "schemaVersion",
        "revision",
        "activationGeneration",
        "active",
        "developerToolsEnabled",
        "prepared",
    ] {
        if let Some(field) = value.get(key) {
            map.insert(key.to_string(), redact_value(field.clone()));
        }
    }
    let profiles = value
        .get("profiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut sanitized_profiles = Vec::new();
    for record in profiles {
        let mut record_map = serde_json::Map::new();
        for key in [
            "id",
            "label",
            "environment",
            "mode",
            "service",
            "stateDir",
            "bindings",
            "pending",
            "legacy",
            "createdAt",
        ] {
            if let Some(field) = record.get(key) {
                if key == "service" {
                    let mut service = serde_json::Map::new();
                    for service_key in ["id", "environment", "label", "apiOrigin"] {
                        if let Some(value) = field.get(service_key) {
                            service.insert(service_key.to_string(), value.clone());
                        }
                    }
                    record_map.insert(key.to_string(), serde_json::Value::Object(service));
                } else if key == "bindings" {
                    let mut bindings = serde_json::Map::new();
                    for binding_key in [
                        "serviceAuthority",
                        "hostId",
                        "signingPublicKey",
                        "noisePublicKey",
                        "workspaceId",
                        "enrollmentRevision",
                        "operationId",
                        "accessOrigin",
                        "region",
                    ] {
                        if let Some(value) = field.get(binding_key) {
                            bindings.insert(binding_key.to_string(), value.clone());
                        }
                    }
                    record_map.insert(key.to_string(), serde_json::Value::Object(bindings));
                } else if key == "pending" {
                    let mut pending = serde_json::Map::new();
                    for pending_key in ["paused", "phase", "intentId", "operationId", "uncertain"] {
                        if let Some(value) = field.get(pending_key) {
                            pending.insert(pending_key.to_string(), value.clone());
                        }
                    }
                    record_map.insert(key.to_string(), serde_json::Value::Object(pending));
                } else {
                    record_map.insert(key.to_string(), redact_value(field.clone()));
                }
            }
        }
        sanitized_profiles.push(serde_json::Value::Object(record_map));
    }
    map.insert(
        "profiles".into(),
        serde_json::Value::Array(sanitized_profiles),
    );
    Ok(serde_json::Value::Object(map))
}

// ---------------------------------------------------------------------------
// Commit and recovery
// ---------------------------------------------------------------------------

/// Confirm a previewed import. Reuses the shared configuration sidecar lock,
/// rechecks source/target revisions, validates the proposed document, publishes
/// it atomically and advances the journal. Retain-only sources write the
/// journal only; their identity state is never moved or converted.
pub fn commit_import(
    commit: &ImportCommit,
    options: &ImportCommitOptions,
) -> CoreResult<ImportOutcome> {
    let preview = &commit.preview;
    let target_path = canonical_target(&preview.target_path);
    if target_path != preview.target_path {
        // A path that only became canonical after publication (for example a
        // /var -> /private/var symlink) still designates the previewed target.
        let resolved = std::fs::canonicalize(&preview.target_path).ok();
        if resolved.as_deref() != Some(target_path.as_path()) {
            return Err(revision_conflict_error(
                Some(preview.target_path.to_string_lossy().to_string()),
                Some(target_path.to_string_lossy().to_string()),
            ));
        }
    }
    let _lock = writer::ConfigLock::acquire(&writer::lock_path(&target_path))?;
    let journal_path = journal_path(&target_path);
    let target_revision = read_target_revision(&target_path)?;
    let mut existing = read_journal(&journal_path)?;

    // Terminal or post-publication journals are resolved before re-reading the
    // source so a repeated confirmation after the target appeared is still an
    // idempotent no-op, not a revision conflict.
    if let Some(journal) = existing.as_mut() {
        match journal.state {
            ImportJournalState::Complete => {
                if journal.preview_id != preview.preview_id {
                    return Err(ConfigurationError::ImportConflict {
                        message: "another import already completed for this configuration target"
                            .into(),
                    }
                    .into());
                }
                ensure_journal_target(journal, target_revision.as_deref())?;
                return Ok(outcome(
                    ImportOutcomeStatus::AlreadyApplied,
                    journal,
                    &journal_path,
                    false,
                ));
            }
            ImportJournalState::Published | ImportJournalState::Activated => {
                if journal.preview_id != preview.preview_id {
                    return Err(ConfigurationError::ImportConflict {
                        message:
                            "another import journal is in progress for this configuration target"
                                .into(),
                    }
                    .into());
                }
                ensure_journal_target(journal, target_revision.as_deref())?;
                mark_complete(&journal_path, journal)?;
                return Ok(outcome(
                    ImportOutcomeStatus::Imported,
                    journal,
                    &journal_path,
                    true,
                ));
            }
            ImportJournalState::Prepared => {
                if journal.preview_id != preview.preview_id {
                    return Err(ConfigurationError::ImportConflict {
                        message:
                            "another import journal is in progress for this configuration target"
                                .into(),
                    }
                    .into());
                }
                if journal.target.revision_after.is_some() && target_revision.is_some() {
                    // Crash after the atomic rename but before the journal was
                    // advanced is recovered as publication, never republished.
                    ensure_journal_target(journal, target_revision.as_deref())?;
                    mark_published(&journal_path, journal)?;
                    mark_activated(&journal_path, journal)?;
                    mark_complete(&journal_path, journal)?;
                    return Ok(outcome(
                        ImportOutcomeStatus::Imported,
                        journal,
                        &journal_path,
                        true,
                    ));
                }
                if journal.target.revision_after.is_none() && target_revision.is_some() {
                    return Err(ConfigurationError::ImportConflict {
                        message: "a configuration target appeared before publication".into(),
                    }
                    .into());
                }
            }
            ImportJournalState::Previewed => {
                if journal.preview_id != preview.preview_id {
                    return Err(ConfigurationError::ImportConflict {
                        message:
                            "another import journal is in progress for this configuration target"
                                .into(),
                    }
                    .into());
                }
            }
        }
    }

    if target_revision.as_deref() != preview.target_revision.as_deref() {
        return Err(revision_conflict_error(
            preview.target_revision.clone(),
            target_revision,
        ));
    }

    let current = preview_import(&ImportPreviewOptions {
        state_root: &preview.source.root,
        target_path: &target_path,
        target_revision: preview.target_revision.as_deref(),
        secure_store_available: options.secure_store_available,
    })?;
    if current.source.kind != preview.source.kind {
        return Err(ConfigurationError::ImportConflict {
            message: "the detected legacy source changed after the preview".into(),
        }
        .into());
    }
    if current.source_revision != preview.source_revision
        || current.preview_id != preview.preview_id
    {
        return Err(ConfigurationError::ImportConflict {
            message: "the legacy source changed after the preview; run preview again".into(),
        }
        .into());
    }
    if let Some(issue) = current
        .conflicts
        .iter()
        .find(|issue| issue.kind == ImportIssueKind::Unsupported)
    {
        return Err(ConfigurationError::ImportUnsupported {
            message: issue.message.clone(),
        }
        .into());
    }
    if current.status == ImportPreviewStatus::Conflict {
        return Err(ConfigurationError::ImportConflict {
            message: current
                .conflicts
                .first()
                .map(|issue| issue.message.clone())
                .unwrap_or_else(|| "the import plan is no longer valid".into()),
        }
        .into());
    }
    if current.status == ImportPreviewStatus::Blocked {
        let missing_store = current
            .conflicts
            .iter()
            .any(|issue| issue.code == "missing_secure_store");
        return Err(if missing_store {
            ConfigurationError::ImportStorageUnavailable {
                message: "the literal legacy token cannot be handed off without secure storage"
                    .into(),
            }
        } else {
            ConfigurationError::ImportUnsupported {
                message: current
                    .conflicts
                    .first()
                    .map(|issue| issue.message.clone())
                    .unwrap_or_else(|| "the import cannot be confirmed yet".into()),
            }
        }
        .into());
    }

    let mut journal = match existing {
        Some(journal) => journal,
        None => {
            let mut journal = initial_journal(&current, target_revision.clone());
            journal.state = ImportJournalState::Previewed;
            push_step(&mut journal, ImportJournalState::Previewed);
            write_journal(&journal_path, &journal)?;
            journal
        }
    };
    if journal.preview_id != current.preview_id {
        return Err(ConfigurationError::ImportConflict {
            message: "another import journal is in progress for this configuration target".into(),
        }
        .into());
    }

    if current.status == ImportPreviewStatus::RetainOnly {
        // No config conversion. The journal is the adoption marker; legacy
        // identity paths and secrets remain exactly where they are.
        journal.state = ImportJournalState::Prepared;
        push_step(&mut journal, ImportJournalState::Prepared);
        write_journal(&journal_path, &journal)?;
        mark_complete(&journal_path, &mut journal)?;
        return Ok(outcome(
            ImportOutcomeStatus::Imported,
            &journal,
            &journal_path,
            false,
        ));
    }

    let proposed =
        current
            .proposed
            .as_ref()
            .ok_or_else(|| ConfigurationError::ImportUnsupported {
                message: "the import has no convertible non-secret settings".into(),
            })?;
    let rendered = render_configuration(proposed)?;
    let revision_after = writer::content_revision(rendered.as_bytes());
    journal.target.revision_before = target_revision;
    journal.target.revision_after = Some(revision_after.clone());
    journal.state = ImportJournalState::Prepared;
    push_step(&mut journal, ImportJournalState::Prepared);
    write_journal(&journal_path, &journal)?;

    writer::write_atomically(&target_path, rendered.as_bytes())?;
    let published = std::fs::read(&target_path)
        .map(|bytes| writer::content_revision(&bytes))
        .map_err(|_| ConfigurationError::ImportStorageUnavailable {
            message: "the published configuration could not be re-read".into(),
        })?;
    if published != revision_after {
        return Err(ConfigurationError::ImportConflict {
            message: "the published configuration revision does not match the prepared plan".into(),
        }
        .into());
    }
    mark_published(&journal_path, &mut journal)?;
    mark_activated(&journal_path, &mut journal)?;
    mark_complete(&journal_path, &mut journal)?;
    Ok(outcome(
        ImportOutcomeStatus::Imported,
        &journal,
        &journal_path,
        false,
    ))
}

fn render_configuration(proposed: &ProposedConfiguration) -> Result<String, ConfigurationError> {
    let mut document = DocumentMut::new();
    document["version"] = value(super::CONFIG_VERSION);
    if let Some(default) = &proposed.default_environment {
        document::validate_identifier(default)?;
        document["environment"] = value(default);
    }
    if !proposed.environments.is_empty() {
        let mut environments = Table::new();
        for environment in &proposed.environments {
            document::validate_identifier(&environment.name)?;
            let catalog = document::catalog()?;
            if catalog.is_builtin(&environment.name) {
                if environment.api_origin.is_some()
                    || environment.issuer.is_some()
                    || environment.native_client_id.is_some()
                {
                    return Err(ConfigurationError::AuthorityOverride {
                        environment: environment.name.clone(),
                    });
                }
                let mut table = Table::new();
                if let Some(label) = &environment.label {
                    table["label"] = value(label);
                }
                if let Some(tls) = &environment.tls {
                    table["tls"] = Item::Table(tls_table(tls));
                }
                if !table.is_empty() {
                    environments[&environment.name] = Item::Table(table);
                }
            } else {
                let mut table = Table::new();
                table["mode"] = value(
                    environment
                        .mode
                        .unwrap_or(EnvironmentMode::Account)
                        .as_str(),
                );
                if let Some(label) = &environment.label {
                    table["label"] = value(label);
                }
                if let Some(origin) = &environment.api_origin {
                    table["api_origin"] = value(origin);
                }
                if let Some(issuer) = &environment.issuer {
                    table["issuer"] = value(issuer);
                }
                if let Some(client) = &environment.native_client_id {
                    table["native_client_id"] = value(client);
                }
                if let Some(tls) = &environment.tls {
                    table["tls"] = Item::Table(tls_table(tls));
                }
                environments[&environment.name] = Item::Table(table);
            }
        }
        document["environments"] = Item::Table(environments);
    }
    let text = document.to_string();
    let (_, _) = document::parse_configuration(&text)?;
    Ok(text)
}

fn tls_table(tls: &TlsConfig) -> Table {
    let mut table = Table::new();
    table["ca_provider"] = value(tls.ca_provider.as_str());
    if let Some(ca_file) = &tls.ca_file {
        table["ca_file"] = value(ca_file);
    }
    table
}

fn ensure_journal_target(
    journal: &ImportJournal,
    target_revision: Option<&str>,
) -> Result<(), ConfigurationError> {
    let expected = journal.target.revision_after.as_deref();
    if journal.target.revision_after.is_none() {
        if target_revision.is_some() {
            return Err(ConfigurationError::ImportConflict {
                message: "a retain-only journal unexpectedly has a configuration target".into(),
            });
        }
        return Ok(());
    }
    if expected != target_revision {
        return Err(ConfigurationError::ImportConflict {
            message: "the configuration target revision does not match the import journal".into(),
        });
    }
    Ok(())
}

fn outcome(
    status: ImportOutcomeStatus,
    journal: &ImportJournal,
    journal_path: &Path,
    recovered: bool,
) -> ImportOutcome {
    ImportOutcome {
        status,
        preview_id: journal.preview_id.clone(),
        target_path: journal.target.path.clone(),
        target_revision: journal.target.revision_after.clone(),
        journal_path: journal_path.to_path_buf(),
        journal_state: journal.state,
        retained_references: journal.retained.clone(),
        recovered,
    }
}

/// Recover deterministic adoption state for one canonical target.
pub fn recover_import(config_path: &Path) -> CoreResult<ImportRecovery> {
    let target_path = canonical_target(config_path);
    let journal_path = journal_path(&target_path);
    if !journal_path.exists() {
        return Ok(ImportRecovery {
            status: ImportRecoveryStatus::NotImported,
            journal_state: None,
            preview_id: None,
            target_revision: None,
            message: None,
        });
    }
    let _lock = writer::ConfigLock::acquire(&writer::lock_path(&target_path))?;
    let mut journal = match read_journal(&journal_path) {
        Ok(Some(journal)) => journal,
        Ok(None) => {
            return Ok(ImportRecovery {
                status: ImportRecoveryStatus::NotImported,
                journal_state: None,
                preview_id: None,
                target_revision: None,
                message: None,
            })
        }
        Err(ConfigurationError::ImportUnsupported { message }) => {
            return Err(ConfigurationError::ImportUnsupported { message }.into())
        }
        Err(_) => {
            return Ok(ImportRecovery {
                status: ImportRecoveryStatus::Conflict,
                journal_state: None,
                preview_id: None,
                target_revision: None,
                message: Some("the import journal is malformed".into()),
            })
        }
    };
    let target_revision = read_target_revision(&target_path)?;
    let state = journal.state;
    match state {
        ImportJournalState::Complete => {
            if ensure_journal_target(&journal, target_revision.as_deref()).is_ok() {
                Ok(ImportRecovery {
                    status: ImportRecoveryStatus::Imported,
                    journal_state: Some(state),
                    preview_id: Some(journal.preview_id),
                    target_revision,
                    message: None,
                })
            } else {
                Ok(recovery_conflict(
                    journal,
                    target_revision,
                    "the completed import target changed externally",
                ))
            }
        }
        ImportJournalState::Published | ImportJournalState::Activated => {
            if ensure_journal_target(&journal, target_revision.as_deref()).is_ok() {
                mark_complete(&journal_path, &mut journal)?;
                Ok(ImportRecovery {
                    status: ImportRecoveryStatus::Imported,
                    journal_state: Some(ImportJournalState::Complete),
                    preview_id: Some(journal.preview_id),
                    target_revision,
                    message: None,
                })
            } else {
                Ok(recovery_conflict(
                    journal,
                    target_revision,
                    "the published import target does not match its journal",
                ))
            }
        }
        ImportJournalState::Prepared => match (
            journal.target.revision_after.as_deref(),
            target_revision.as_deref(),
        ) {
            (Some(expected), Some(actual)) if expected == actual => {
                // Crash after the atomic rename: finish the journal rather
                // than republishing or discarding the validated file.
                mark_published(&journal_path, &mut journal)?;
                mark_activated(&journal_path, &mut journal)?;
                mark_complete(&journal_path, &mut journal)?;
                Ok(ImportRecovery {
                    status: ImportRecoveryStatus::Imported,
                    journal_state: Some(ImportJournalState::Complete),
                    preview_id: Some(journal.preview_id),
                    target_revision,
                    message: None,
                })
            }
            (Some(_), Some(_)) => Ok(recovery_conflict(
                journal,
                target_revision,
                "the target appeared or changed before publication completed",
            )),
            (Some(_), None) | (None, None) => Ok(ImportRecovery {
                status: ImportRecoveryStatus::NotImported,
                journal_state: Some(state),
                preview_id: Some(journal.preview_id),
                target_revision,
                message: Some("publication did not start".into()),
            }),
            (None, Some(_)) => Ok(recovery_conflict(
                journal,
                target_revision,
                "a target appeared before publication completed",
            )),
        },
        ImportJournalState::Previewed => Ok(ImportRecovery {
            status: ImportRecoveryStatus::NotImported,
            journal_state: Some(state),
            preview_id: Some(journal.preview_id),
            target_revision,
            message: Some("the import was not prepared".into()),
        }),
    }
}

fn recovery_conflict(
    journal: ImportJournal,
    target_revision: Option<String>,
    message: &str,
) -> ImportRecovery {
    ImportRecovery {
        status: ImportRecoveryStatus::Conflict,
        journal_state: Some(journal.state),
        preview_id: Some(journal.preview_id),
        target_revision,
        message: Some(message.into()),
    }
}

// ---------------------------------------------------------------------------
// Journal persistence
// ---------------------------------------------------------------------------

fn journal_path(config_path: &Path) -> PathBuf {
    let mut path = config_path.as_os_str().to_os_string();
    path.push(JOURNAL_SUFFIX);
    PathBuf::from(path)
}

fn initial_journal(preview: &ImportPreview, target_revision: Option<String>) -> ImportJournal {
    let profile_ids = preview
        .retained_references
        .iter()
        .filter_map(|reference| match reference {
            RetainedReference::ProfileId { profile_id } => Some(profile_id.clone()),
            _ => None,
        })
        .collect();
    let state_paths = preview
        .retained_references
        .iter()
        .filter_map(|reference| match reference {
            RetainedReference::StatePath { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    ImportJournal {
        schema: IMPORT_JOURNAL_SCHEMA.into(),
        version: IMPORT_JOURNAL_VERSION,
        preview_id: preview.preview_id.clone(),
        state: ImportJournalState::Previewed,
        source: ImportJournalSource {
            kind: preview.source.kind,
            root: preview.source.root.clone(),
            revision: preview.source_revision.clone(),
            profile_ids,
            state_paths,
        },
        target: ImportJournalTarget {
            path: preview.target_path.clone(),
            revision_before: target_revision,
            revision_after: None,
        },
        environment: preview.proposed.as_ref().and_then(|proposed| {
            proposed
                .environments
                .first()
                .map(|environment| ImportJournalEnvironment {
                    name: environment.name.clone(),
                    definition_source: environment.definition_source,
                })
        }),
        retained: preview.retained_references.clone(),
        legacy_settings: LegacySettingsStatus::Active,
        steps: Vec::new(),
    }
}

fn push_step(journal: &mut ImportJournal, state: ImportJournalState) {
    journal.steps.push(ImportJournalStep {
        at_unix_ms: now_unix_ms(),
        state,
    });
}

fn write_journal(path: &Path, journal: &ImportJournal) -> Result<(), ConfigurationError> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|_| {
        ConfigurationError::ImportStorageUnavailable {
            message: "the import journal could not be encoded".into(),
        }
    })?;
    writer::write_atomically(path, &bytes)
}

fn read_journal(path: &Path) -> Result<Option<ImportJournal>, ConfigurationError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(ConfigurationError::ImportConflict {
                message: "the import journal could not be read".into(),
            })
        }
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| ConfigurationError::ImportConflict {
            message: "the import journal is not valid JSON".into(),
        })?;
    if value.get("schema").and_then(|v| v.as_str()) != Some(IMPORT_JOURNAL_SCHEMA)
        || value.get("version").and_then(|v| v.as_i64()) != Some(IMPORT_JOURNAL_VERSION)
    {
        return Err(ConfigurationError::ImportUnsupported {
            message: "the import journal version is not supported by this client".into(),
        });
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| ConfigurationError::ImportConflict {
            message: "the import journal is malformed".into(),
        })
}

fn mark_published(path: &Path, journal: &mut ImportJournal) -> Result<(), ConfigurationError> {
    journal.state = ImportJournalState::Published;
    push_step(journal, ImportJournalState::Published);
    write_journal(path, journal)
}

fn mark_activated(path: &Path, journal: &mut ImportJournal) -> Result<(), ConfigurationError> {
    journal.state = ImportJournalState::Activated;
    journal.legacy_settings = LegacySettingsStatus::Superseded;
    push_step(journal, ImportJournalState::Activated);
    write_journal(path, journal)
}

fn mark_complete(path: &Path, journal: &mut ImportJournal) -> Result<(), ConfigurationError> {
    // Completion is only recorded after the target is valid/durable and legacy
    // settings are marked superseded. Old files remain recovery material.
    journal.state = ImportJournalState::Complete;
    push_step(journal, ImportJournalState::Complete);
    write_journal(path, journal)
}

// ---------------------------------------------------------------------------
// Small deterministic helpers
// ---------------------------------------------------------------------------

fn read_target_revision(path: &Path) -> Result<Option<String>, ConfigurationError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(writer::content_revision(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ConfigurationError::UnreadableConfig),
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value, ()> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(())
}

fn sanitize_route(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let mut safe = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        safe.push(':');
        safe.push_str(&port.to_string());
    }
    safe.push_str(url.path());
    Some(safe)
}

fn redact_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            if text.starts_with("http://") || text.starts_with("https://") {
                serde_json::Value::String(
                    sanitize_route(&text).unwrap_or_else(|| "redacted".into()),
                )
            } else if text.len() > 512 {
                serde_json::Value::String("<redacted>".into())
            } else {
                serde_json::Value::String(text)
            }
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, redact_value(value)))
                .collect(),
        ),
        other => other,
    }
}

fn digest_value(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("sha256:{}", sha256_hex(&bytes))
}

fn digest_btree(value: &BTreeMap<String, serde_json::Value>) -> String {
    let value = serde_json::Value::Object(value.clone().into_iter().collect());
    digest_value(&value)
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn sanitized_name(name: &str) -> String {
    let mut result: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while result.starts_with('-') || result.contains("--") {
        result = result.trim_start_matches('-').replace("--", "-");
    }
    result.truncate(64);
    result.trim_end_matches('-').to_string()
}

fn issue(
    code: &str,
    kind: ImportIssueKind,
    message: &str,
    path: Option<PathBuf>,
) -> ImportConflict {
    ImportConflict {
        code: code.into(),
        message: message.into(),
        kind,
        path,
    }
}

fn revision_conflict_error(
    expected: Option<String>,
    current: Option<String>,
) -> crate::api::CoreError {
    ConfigurationError::ImportRevisionConflict { expected, current }.into()
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct DataDirScope {
        previous: Option<std::ffi::OsString>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl DataDirScope {
        fn new(path: &Path) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let previous = std::env::var_os("WHALEREIN_DATA_DIR");
            std::env::set_var("WHALEREIN_DATA_DIR", path);
            Self {
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for DataDirScope {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("WHALEREIN_DATA_DIR", value),
                None => std::env::remove_var("WHALEREIN_DATA_DIR"),
            }
        }
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn tree_hash(root: &Path) -> String {
        let mut entries = Vec::new();
        collect(root, root, &mut entries);
        entries.sort();
        let mut hasher = Sha256::new();
        for (path, bytes) in entries {
            hasher.update(path.as_bytes());
            hasher.update(bytes);
        }
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }

    fn collect(root: &Path, current: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        let Ok(reader) = std::fs::read_dir(current) else {
            return;
        };
        for entry in reader.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, entries);
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                entries.push((relative, std::fs::read(&path).unwrap_or_default()));
            }
        }
    }

    fn preview_options<'a>(
        root: &'a Path,
        target: &'a Path,
        secure: bool,
    ) -> ImportPreviewOptions<'a> {
        ImportPreviewOptions {
            state_root: root,
            target_path: target,
            target_revision: None,
            secure_store_available: secure,
        }
    }

    fn fixture_core_flat(root: &Path) {
        write(
            &root.join("config.json"),
            br#"{
              "schema": "whalerein/v1/config",
              "access_url": "http://127.0.0.1:8080",
              "host_id": "host-a",
              "device_id": "device-a",
              "protocol_version": 1,
              "mqtt_url": "mqtts://user:secret@broker.example.test:8883",
              "host_fingerprint": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "host_fingerprints": {"host-b": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
            }"#,
        );
        write(&root.join("cache.sqlite3"), b"not-a-real-database");
        write(&root.join("identity.lock"), b"");
        write(&root.join("unconfirmed-commands"), b"{}");
    }

    fn profiles_json(active: &str, include_custom: bool) -> Vec<u8> {
        let mut profiles = vec![
            serde_json::json!({
                "id": "11111111111111111111111111111111",
                "name": "official-production",
                "mode": "account",
                "legacy": false,
                "environment": "production",
                "service": {"kind": "official-production"},
                "created_unix_ms": 1
            }),
            serde_json::json!({
                "id": "22222222222222222222222222222222",
                "name": "official-local",
                "mode": "account",
                "legacy": false,
                "environment": "local",
                "service": {"kind": "official-local"},
                "created_unix_ms": 2
            }),
        ];
        if include_custom {
            profiles.push(serde_json::json!({
                "id": "33333333333333333333333333333333",
                "name": "company",
                "mode": "account",
                "legacy": false,
                "environment": "company",
                "service": {
                    "kind": "custom",
                    "api_origin": "https://cloud.example.com",
                    "issuer": "https://auth.example.com/oidc",
                    "native_client_id": "whalerein-cli"
                },
                "created_unix_ms": 3
            }));
        }
        let registry = serde_json::json!({
            "schema": "whalerein/v1/profiles",
            "revision": 4,
            "activation_generation": 2,
            "active": active,
            "profiles": profiles
        });
        serde_json::to_vec(&registry).unwrap()
    }

    fn fixture_core_profiles(root: &Path, active: &str, include_custom: bool) {
        write(
            &root.join("profiles.json"),
            &profiles_json(active, include_custom),
        );
    }

    fn host_registry(active: &str, authority: Option<&str>, version: u64) -> Vec<u8> {
        let bindings = match authority {
            Some(value) => serde_json::json!({"serviceAuthority": value, "hostId": "host-a"}),
            None => serde_json::json!({}),
        };
        let registry = serde_json::json!({
            "schema": "whalerein-host/v1/profiles",
            "schemaVersion": version,
            "revision": 13,
            "activationGeneration": 2,
            "active": active,
            "developerToolsEnabled": true,
            "prepared": null,
            "profiles": [
                {
                    "id": "01b20d294064155fd821035f3d2a2d8f",
                    "label": "WhaleRein Cloud",
                    "environment": "production",
                    "mode": "managed",
                    "service": {
                        "id": "official-production",
                        "environment": "production",
                        "label": "WhaleRein Cloud",
                        "apiOrigin": "https://app.whalerein.com"
                    },
                    "stateDir": "profiles/01b20d294064155fd821035f3d2a2d8f",
                    "bindings": {},
                    "pending": {},
                    "legacy": false,
                    "createdAt": 1
                },
                {
                    "id": "d64d69952060fa6682a2a83b9f3326f8",
                    "label": "WhaleRein Cloud Local",
                    "environment": "local",
                    "mode": "managed",
                    "service": {
                        "id": "official-local",
                        "environment": "local",
                        "label": "WhaleRein Cloud Local",
                        "apiOrigin": null
                    },
                    "stateDir": "profiles/d64d69952060fa6682a2a83b9f3326f8",
                    "bindings": bindings,
                    "pending": {
                        "phase": "enrollment",
                        "intentId": "intent-1",
                        "operationId": "op-1",
                        "uncertain": true
                    },
                    "legacy": false,
                    "createdAt": 2
                }
            ]
        });
        serde_json::to_vec(&registry).unwrap()
    }

    fn fixture_cordis_managed(root: &Path, authority: Option<&str>, version: u64) {
        let dsh = root.join("dsh");
        write(
            &dsh.join("host-profiles.json"),
            &host_registry("d64d69952060fa6682a2a83b9f3326f8", authority, version),
        );
        write(
            &dsh.join("cordis.patch.yml"),
            b"- id: whalerein-dsh\n  config:\n    connection_mode: managed\n    environment: local\n    state_root: !!js process.env.WHALEREIN_DATA_DIR\n    ca_file: \"\"\n    adapter: real\n",
        );
    }

    fn fixture_cordis_standalone(root: &Path, literal_token: bool) {
        let dsh = root.join("dsh");
        let token = if literal_token {
            "host_token: \"SENTINEL-TOKEN-DO-NOT-PRINT\"\n"
        } else {
            "host_token: !!js process.env.WHALEREIN_HOST_TOKEN\n"
        };
        let text = format!(
            "- id: whalerein-dsh\n  config:\n    connection_mode: standalone\n    server: 127.0.0.1\n    host_id: host-standalone\n    state_dir: {}\n    allow_insecure_mqtt: true\n    log_level: debug\n{t}",
            root.join("dsh").to_string_lossy(),
            t = token
        );
        write(&dsh.join("cordis.patch.yml"), text.as_bytes());
        write(&dsh.join("host-id"), b"host-standalone");
    }

    fn fixture_prototype(root: &Path, api_origin: &str, issuer: &str) {
        let ca = root.join("ca.pem");
        write(&ca, b"PUBLIC-CA-CERTIFICATE");
        let state_root = root.join("clients");
        std::fs::create_dir_all(state_root.join("cli")).unwrap();
        std::fs::create_dir_all(state_root.join("dsh")).unwrap();
        let doc = serde_json::json!({
            "schema": "whalerein/v1/local-environment",
            "environment": "local",
            "service": "official-local",
            "api_origin": api_origin,
            "issuer": issuer,
            "ca_file": ca.to_string_lossy(),
            "state_root": state_root.to_string_lossy()
        });
        write(
            &root.join("environment.json"),
            serde_json::to_vec(&doc).unwrap().as_slice(),
        );
    }

    fn local_doc(_root: &Path) -> (String, String) {
        (
            "https://app.whalerein.test".into(),
            "https://auth.whalerein.test/oidc".into(),
        )
    }

    fn commit_of(preview: &ImportPreview) -> ImportCommit {
        ImportCommit::from_preview(preview)
    }

    fn code(error: &crate::api::CoreError) -> &'static str {
        error.code()
    }

    // AC-21: detection and read-only preview for every source type.

    #[test]
    fn config_env_ac_21_detects_all_five_legacy_source_kinds() {
        let base = tempfile::tempdir().unwrap();
        let flat = base.path().join("flat");
        fixture_core_flat(&flat);
        assert_eq!(
            detect_legacy(&flat).unwrap().kind,
            LegacySourceKind::CoreFlat
        );

        let profiles = base.path().join("profiles");
        fixture_core_profiles(&profiles, "22222222222222222222222222222222", false);
        assert_eq!(
            detect_legacy(&profiles).unwrap().kind,
            LegacySourceKind::CoreProfiles
        );

        let managed = base.path().join("managed");
        fixture_cordis_managed(&managed, None, 1);
        assert_eq!(
            detect_legacy(&managed).unwrap().kind,
            LegacySourceKind::CordisManaged
        );

        let standalone = base.path().join("standalone");
        fixture_cordis_standalone(&standalone, false);
        assert_eq!(
            detect_legacy(&standalone).unwrap().kind,
            LegacySourceKind::CordisStandalone
        );

        let prototype = base.path().join("prototype");
        let (api, issuer) = local_doc(&prototype);
        fixture_prototype(&prototype, &api, &issuer);
        assert_eq!(
            detect_legacy(&prototype).unwrap().kind,
            LegacySourceKind::Prototype
        );

        assert!(detect_legacy(&base.path().join("empty")).is_none());
    }

    #[test]
    fn config_env_ac_21_legacy_state_probe_suppresses_default_startup() {
        let base = tempfile::tempdir().unwrap();
        let legacy = base.path().join("project");
        fixture_core_flat(&legacy);
        let _env = DataDirScope::new(&legacy);
        assert!(legacy_state_present(&base.path().join("empty")));
        let empty = base.path().join("other");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("WHALEREIN_DATA_DIR", &empty);
        assert!(!legacy_state_present(&empty));
    }

    #[test]
    fn config_env_ac_21_core_flat_preview_is_retain_only_and_read_only() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_flat(&source);
        let target_dir = base.path().join("target");
        let target = target_dir.join("config.toml");
        let before = tree_hash(&source);
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::RetainOnly);
        assert!(preview.proposed.is_none());
        assert!(!target.exists());
        assert!(!target_dir.exists());
        assert!(!journal_path(&target).exists());
        assert_eq!(tree_hash(&source), before);
        let retained: Vec<String> = preview
            .retained_references
            .iter()
            .filter_map(|reference| match reference {
                RetainedReference::SecretSlot { slot } => Some(slot.clone()),
                _ => None,
            })
            .collect();
        assert!(retained.contains(&"whalerein:v1:device-key:device-a".into()));
        assert!(retained
            .iter()
            .any(|slot| slot.starts_with("whalerein:v2:identity:")));
    }

    #[test]
    fn config_env_ac_21_core_profiles_preview_maps_authority_without_writing() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", true);
        let target = base.path().join("target").join("config.toml");
        let before = tree_hash(&source);
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Ready);
        let proposed = preview.proposed.as_ref().unwrap();
        assert_eq!(proposed.default_environment.as_deref(), Some("local"));
        assert_eq!(proposed.environments.len(), 3);
        assert!(proposed.environments.iter().any(|environment| {
            environment.name == "company"
                && environment.definition_source == DefinitionSource::File
                && environment.api_origin.as_deref() == Some("https://cloud.example.com")
        }));
        assert!(preview.retained_references.iter().any(|reference| matches!(
            reference,
            RetainedReference::ProfileId { profile_id } if profile_id == "11111111111111111111111111111111"
        )));
        assert!(!source.join("profiles").exists());
        assert!(!target.exists());
        assert_eq!(tree_hash(&source), before);
        let again = preview_import(&preview_options(&source, &target, false)).unwrap();
        assert_eq!(again.preview_id, preview.preview_id);
    }

    #[test]
    fn config_env_ac_21_cordis_managed_and_prototype_previews_are_read_only() {
        let base = tempfile::tempdir().unwrap();
        let managed = base.path().join("managed");
        fixture_cordis_managed(&managed, None, 1);
        let dsh_root = managed.join("dsh");
        let target = base.path().join("managed-target.toml");
        let before = tree_hash(&managed);
        let preview = preview_import(&preview_options(&dsh_root, &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Ready);
        assert_eq!(
            preview
                .proposed
                .as_ref()
                .unwrap()
                .default_environment
                .as_deref(),
            Some("local")
        );
        assert!(preview.retained_references.iter().any(|reference| matches!(
            reference,
            RetainedReference::CredentialNamespace { namespace }
                if namespace == "whalerein-host/v1/dsh/d64d69952060fa6682a2a83b9f3326f8"
        )));
        assert!(preview.retained_references.iter().any(|reference| matches!(
            reference,
            RetainedReference::ProfileId { profile_id }
                if profile_id == "d64d69952060fa6682a2a83b9f3326f8"
        )));
        assert_eq!(tree_hash(&managed), before);
        assert!(!target.exists());

        let prototype = base.path().join("prototype");
        let (api, issuer) = local_doc(&prototype);
        fixture_prototype(&prototype, &api, &issuer);
        let ptarget = base.path().join("prototype-target.toml");
        let pbefore = tree_hash(&prototype);
        let preview = preview_import(&preview_options(&prototype, &ptarget, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Ready);
        let environment = &preview.proposed.as_ref().unwrap().environments[0];
        assert_eq!(environment.name, "local");
        assert!(environment.tls.as_ref().unwrap().ca_file.is_some());
        assert!(preview
            .retained_references
            .iter()
            .any(|reference| matches!(reference, RetainedReference::TrustFile { .. })));
        assert_eq!(tree_hash(&prototype), pbefore);
    }

    #[test]
    fn config_env_ac_21_standalone_preview_never_emits_literal_token() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_cordis_standalone(&source, true);
        let target = base.path().join("target.toml");
        let before = tree_hash(&source);
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Blocked);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "missing_secure_store"));
        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("SENTINEL-TOKEN-DO-NOT-PRINT"));
        assert_eq!(tree_hash(&source), before);

        let with_store = preview_import(&preview_options(&source, &target, true)).unwrap();
        assert_eq!(with_store.status, ImportPreviewStatus::RetainOnly);
        assert!(with_store.secure_store_handoff.as_ref().unwrap().required);
        assert!(with_store.secure_store_handoff.as_ref().unwrap().satisfied);
    }

    #[test]
    fn config_env_ac_21_cancel_changes_nothing_and_confirm_writes_only_target_and_journal() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target_dir = base.path().join("target");
        let target = target_dir.join("config.toml");
        let before = tree_hash(&source);
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        drop(preview);
        assert_eq!(tree_hash(&source), before);
        assert!(!target.exists());

        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let outcome = commit_import(
            &commit_of(&preview),
            &ImportCommitOptions {
                secure_store_available: false,
            },
        )
        .unwrap();
        assert_eq!(outcome.status, ImportOutcomeStatus::Imported);
        assert!(target.exists());
        assert!(journal_path(&target).exists());
        assert_eq!(tree_hash(&source), before);
        let entries: Vec<_> = std::fs::read_dir(&target_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.iter().all(|name| name == "config.toml"
            || name == "config.toml.import-journal.json"
            || name == "config.toml.lock.sqlite"));
        let journal = read_journal(&journal_path(&target)).unwrap().unwrap();
        assert_eq!(journal.state, ImportJournalState::Complete);
        assert_eq!(journal.legacy_settings, LegacySettingsStatus::Superseded);
        assert_eq!(journal.source.kind, LegacySourceKind::CoreProfiles);
        assert_eq!(
            journal.target.revision_after.as_deref(),
            Some(writer::content_revision(&std::fs::read(&target).unwrap()).as_str())
        );
    }

    // AC-22: identity-preserving references.

    #[test]
    fn config_env_ac_22_retained_paths_profile_ids_and_namespaces_are_preserved() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_cordis_managed(&source, None, 1);
        let dsh = source.join("dsh");
        let target = base.path().join("target.toml");
        let preview = preview_import(&preview_options(&dsh, &target, true)).unwrap();
        let paths: Vec<PathBuf> = preview
            .retained_references
            .iter()
            .filter_map(|reference| match reference {
                RetainedReference::StatePath { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert!(paths.iter().all(|path| path.is_absolute()));
        assert!(paths
            .iter()
            .any(|path| path.ends_with("profiles/d64d69952060fa6682a2a83b9f3326f8")));
        assert_eq!(
            preview
                .retained_references
                .iter()
                .filter(|reference| matches!(reference, RetainedReference::ProfileId { .. }))
                .count(),
            2
        );
        let entry = serde_json::to_value(&preview).unwrap();
        assert!(!entry.to_string().contains("SENTINEL"));
    }

    #[test]
    fn config_env_ac_22_import_never_creates_profile_dirs_or_touches_secret_files() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let before = tree_hash(&source);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        commit_import(
            &commit_of(&preview),
            &ImportCommitOptions {
                secure_store_available: false,
            },
        )
        .unwrap();
        assert!(!source.join("profiles").exists());
        assert_eq!(tree_hash(&source), before);
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(!text.contains("private"));
        assert!(!text.contains("device-key"));
        assert!(!text.contains("host_token"));
    }

    // AC-24: crash boundaries, recovery, retry and conflicts.

    #[test]
    fn config_env_ac_24_each_persistence_boundary_recovers_deterministically() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let jpath = journal_path(&target);
        let rendered = render_configuration(preview.proposed.as_ref().unwrap()).unwrap();
        let revision = writer::content_revision(rendered.as_bytes());

        // Previewed: nothing was published.
        let mut journal = initial_journal(&preview, None);
        journal.state = ImportJournalState::Previewed;
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::NotImported);
        assert!(recovery.journal_state.is_some());
        assert!(!target.exists());

        // Prepared with no target: not imported.
        journal.state = ImportJournalState::Prepared;
        journal.target.revision_after = Some(revision.clone());
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::NotImported);
        assert!(!target.exists());

        // Prepared but the target was truncated/mismatched: conflict.
        write(&target, b"version = 1\nenvironment = \"production\"\n");
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Conflict);
        assert!(recovery.message.is_some());

        // Prepared with the exact published bytes (crash after rename): imported.
        write(&target, rendered.as_bytes());
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Imported);
        assert_eq!(recovery.journal_state, Some(ImportJournalState::Complete));

        // Published with a matching target completes; a mismatch conflicts.
        journal.state = ImportJournalState::Published;
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Imported);
        write(&target, b"version = 1\nenvironment = \"staging\"\n");
        journal.state = ImportJournalState::Published;
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Conflict);

        // Complete: target changed externally is a conflict, not an overwrite.
        journal.state = ImportJournalState::Complete;
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Conflict);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"version = 1\nenvironment = \"staging\"\n"
        );
    }

    #[test]
    fn config_env_ac_24_absent_journal_is_not_imported_and_retain_only_crash_retries() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_cordis_standalone(&source, false);
        let target = base.path().join("target").join("config.toml");
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::NotImported);
        assert!(!target.exists());
        assert!(!journal_path(&target).exists());

        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::RetainOnly);
        let jpath = journal_path(&target);
        let mut journal = initial_journal(&preview, None);
        journal.state = ImportJournalState::Prepared;
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::NotImported);
        let outcome = commit_import(
            &commit_of(&preview),
            &ImportCommitOptions {
                secure_store_available: false,
            },
        )
        .unwrap();
        assert_eq!(outcome.status, ImportOutcomeStatus::Imported);
        assert!(!target.exists());
        let journal = read_journal(&jpath).unwrap().unwrap();
        assert_eq!(journal.state, ImportJournalState::Complete);
        assert!(journal.target.revision_after.is_none());
    }

    #[test]
    fn config_env_ac_24_repeated_commit_is_an_idempotent_no_op() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let before = tree_hash(&source);
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let first = commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap();
        assert_eq!(first.status, ImportOutcomeStatus::Imported);
        let journal_bytes = std::fs::read(journal_path(&target)).unwrap();
        let target_bytes = std::fs::read(&target).unwrap();
        let second = commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap();
        assert_eq!(second.status, ImportOutcomeStatus::AlreadyApplied);
        assert_eq!(std::fs::read(journal_path(&target)).unwrap(), journal_bytes);
        assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
        assert_eq!(tree_hash(&source), before);
        assert!(!source.join("profiles").exists());
    }

    #[test]
    fn config_env_ac_24_concurrent_commits_serialize_on_the_shared_lock() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                commit_import(&commit_of(&preview), &ImportCommitOptions::default())
                    .unwrap()
                    .status
            });
            let second = scope.spawn(|| {
                commit_import(&commit_of(&preview), &ImportCommitOptions::default())
                    .unwrap()
                    .status
            });
            vec![first.join().unwrap(), second.join().unwrap()]
        });
        let imported = results
            .iter()
            .filter(|status| **status == ImportOutcomeStatus::Imported)
            .count();
        let noop = results
            .iter()
            .filter(|status| **status == ImportOutcomeStatus::AlreadyApplied)
            .count();
        assert_eq!(imported, 1);
        assert_eq!(noop, 1);
    }

    #[test]
    fn config_env_ac_24_source_change_after_preview_conflicts() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let mut registry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source.join("profiles.json")).unwrap()).unwrap();
        registry["revision"] = serde_json::json!(5);
        write(
            &source.join("profiles.json"),
            serde_json::to_vec(&registry).unwrap().as_slice(),
        );
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_conflict");
        assert!(!target.exists());
    }

    #[test]
    fn config_env_ac_24_target_change_after_preview_is_a_revision_conflict() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        write(&target, b"version = 1\nenvironment = \"local\"\n");
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "revision_conflict");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"version = 1\nenvironment = \"local\"\n"
        );
    }

    #[test]
    fn config_env_ac_24_future_journal_and_source_versions_are_refused() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let jpath = journal_path(&target);
        let mut value = serde_json::to_value(initial_journal(&preview, None)).unwrap();
        value["version"] = serde_json::json!(2);
        write(&jpath, serde_json::to_vec(&value).unwrap().as_slice());
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_unsupported");
        assert!(!target.exists());
        let recovery = recover_import(&target);
        assert!(recovery.is_err());
        assert_eq!(code(&recovery.unwrap_err()), "import_unsupported");

        let future = base.path().join("future");
        fixture_cordis_managed(&future, None, 2);
        let dsh = future.join("dsh");
        let preview =
            preview_import(&preview_options(&dsh, &base.path().join("f.toml"), false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.kind == ImportIssueKind::Unsupported));
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_unsupported");
    }

    #[test]
    fn config_env_ac_24_literal_token_blocking_and_authority_conflicts() {
        let base = tempfile::tempdir().unwrap();
        let standalone = base.path().join("standalone");
        fixture_cordis_standalone(&standalone, true);
        let target = base.path().join("s.toml");
        let preview =
            preview_import(&preview_options(&standalone.join("dsh"), &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Blocked);
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_storage_unavailable");
        assert!(!target.exists());

        let prototype = base.path().join("prototype");
        fixture_prototype(
            &prototype,
            "https://app.changed.test",
            "https://auth.changed.test/oidc",
        );
        let ptarget = base.path().join("p.toml");
        let preview = preview_import(&preview_options(&prototype, &ptarget, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "authority_mismatch"));
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_conflict");
        assert!(!ptarget.exists());
    }

    #[test]
    fn config_env_ac_21_invalid_or_future_sources_are_reported() {
        let base = tempfile::tempdir().unwrap();
        let empty = base.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let error = preview_import(&preview_options(&empty, &base.path().join("t.toml"), false))
            .unwrap_err();
        assert_eq!(code(&error), "import_unsupported");

        let broken = base.path().join("broken");
        write(&broken.join("config.json"), b"{not json");
        let preview = preview_import(&preview_options(
            &broken,
            &base.path().join("b.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "source_invalid"));

        let future = base.path().join("future-flat");
        write(
            &future.join("config.json"),
            br#"{"schema":"whalerein/v2/config","access_url":"https://example.test","host_id":"h","device_id":"d","protocol_version":1}"#,
        );
        let preview = preview_import(&preview_options(
            &future,
            &base.path().join("f.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.kind == ImportIssueKind::Unsupported));
    }

    #[test]
    fn config_env_ac_21_prototype_invalid_document_and_missing_ca_are_conflicts() {
        let base = tempfile::tempdir().unwrap();
        let invalid = base.path().join("invalid");
        let doc = serde_json::json!({
            "schema": "whalerein/v2/local-environment",
            "environment": "local",
            "service": "official-local",
            "api_origin": "https://app.whalerein.test",
            "issuer": "https://auth.other.test/oidc",
            "ca_file": invalid.join("ca.pem").to_string_lossy(),
            "state_root": invalid.join("clients").to_string_lossy()
        });
        write(
            &invalid.join("environment.json"),
            serde_json::to_vec(&doc).unwrap().as_slice(),
        );
        let preview = preview_import(&preview_options(
            &invalid,
            &base.path().join("i.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "source_invalid"));

        let missing = base.path().join("missing");
        let (api, issuer) = local_doc(&missing);
        fixture_prototype(&missing, &api, &issuer);
        std::fs::remove_file(missing.join("ca.pem")).unwrap();
        let preview = preview_import(&preview_options(
            &missing,
            &base.path().join("m.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "ca_file_missing"));
    }

    #[test]
    fn config_env_ac_21_core_profiles_invalid_and_future_schemas_are_reported() {
        let base = tempfile::tempdir().unwrap();
        let broken = base.path().join("broken");
        write(&broken.join("profiles.json"), b"{not json");
        let preview = preview_import(&preview_options(
            &broken,
            &base.path().join("b.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "source_invalid"));

        let future = base.path().join("future");
        let registry = serde_json::json!({
            "schema": "whalerein/v2/profiles",
            "revision": 1,
            "activation_generation": 0,
            "active": "22222222222222222222222222222222",
            "profiles": [{
                "id": "22222222222222222222222222222222",
                "name": "official-local",
                "mode": "account",
                "legacy": false,
                "environment": "local",
                "service": {"kind": "official-local"},
                "created_unix_ms": 0
            }]
        });
        write(
            &future.join("profiles.json"),
            serde_json::to_vec(&registry).unwrap().as_slice(),
        );
        let preview = preview_import(&preview_options(
            &future,
            &base.path().join("f.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "unsupported_source_version"));
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "profile_registry_invalid"));
    }

    #[test]
    fn config_env_ac_21_core_profiles_legacy_standalone_is_retain_only() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        let registry = serde_json::json!({
            "schema": "whalerein/v1/profiles",
            "revision": 1,
            "activation_generation": 0,
            "active": "default",
            "profiles": [{
                "id": "default",
                "name": "default",
                "mode": "standalone",
                "legacy": true,
                "environment": "standalone",
                "created_unix_ms": 0
            }]
        });
        write(
            &source.join("profiles.json"),
            serde_json::to_vec(&registry).unwrap().as_slice(),
        );
        let preview = preview_import(&preview_options(
            &source,
            &base.path().join("target").join("config.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::RetainOnly);
        assert!(preview.proposed.is_none());
        assert!(preview.retained_references.iter().any(|reference| matches!(
            reference,
            RetainedReference::ProfileId { profile_id } if profile_id == "default"
        )));
        assert!(preview.retained_references.iter().any(|reference| matches!(
            reference,
            RetainedReference::StatePath { path } if path == &source
        )));
    }

    #[test]
    fn config_env_ac_24_journal_malformed_and_activated_states() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let jpath = journal_path(&target);
        write(&jpath, b"{malformed");
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Conflict);
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_conflict");

        let rendered = render_configuration(preview.proposed.as_ref().unwrap()).unwrap();
        let revision = writer::content_revision(rendered.as_bytes());
        write(&target, rendered.as_bytes());
        let mut journal = initial_journal(&preview, None);
        journal.target.revision_after = Some(revision);
        journal.state = ImportJournalState::Activated;
        journal.legacy_settings = LegacySettingsStatus::Superseded;
        write_journal(&jpath, &journal).unwrap();
        let recovery = recover_import(&target).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Imported);
        assert_eq!(recovery.journal_state, Some(ImportJournalState::Complete));
    }

    #[test]
    fn config_env_ac_24_preview_target_revision_and_malformed_managed_registry() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        write(&target, b"version = 1\n");
        let current = writer::content_revision(&std::fs::read(&target).unwrap());
        let preview = preview_import(&ImportPreviewOptions {
            state_root: &source,
            target_path: &target,
            target_revision: Some("sha256:deadbeef"),
            secure_store_available: false,
        })
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "target_revision_conflict"));
        assert_eq!(preview.target_revision, Some(current));

        let managed = base.path().join("managed");
        write(
            &managed.join("dsh").join("host-profiles.json"),
            b"{not json",
        );
        let preview = preview_import(&preview_options(
            &managed.join("dsh"),
            &base.path().join("m.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "source_invalid"));
    }

    #[test]
    fn config_env_ac_22_managed_import_preserves_registry_and_reopens_paths() {
        let base = tempfile::tempdir().unwrap();
        let managed = base.path().join("managed");
        fixture_cordis_managed(&managed, None, 1);
        let dsh = managed.join("dsh");
        let target = base.path().join("target").join("config.toml");
        let before = tree_hash(&managed);
        let preview = preview_import(&preview_options(&dsh, &target, false)).unwrap();
        let outcome = commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap();
        assert_eq!(outcome.status, ImportOutcomeStatus::Imported);
        assert!(target.exists());
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("environment = \"local\""));
        assert_eq!(tree_hash(&managed), before);
        let journal = read_journal(&journal_path(&target)).unwrap().unwrap();
        assert!(journal
            .retained
            .iter()
            .any(|reference| matches!(reference, RetainedReference::ProfileId { .. })));
        assert_eq!(journal.legacy_settings, LegacySettingsStatus::Superseded);
        let recovered = recover_import(&target).unwrap();
        assert_eq!(recovered.status, ImportRecoveryStatus::Imported);
    }

    #[test]
    fn config_env_ac_21_core_profiles_custom_environment_round_trips_through_commit() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "33333333333333333333333333333333", true);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        assert_eq!(
            preview
                .proposed
                .as_ref()
                .unwrap()
                .default_environment
                .as_deref(),
            Some("company")
        );
        commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap();
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("[environments.company]"));
        assert!(text.contains("mode = \"account\""));
        assert!(text.contains("api_origin = \"https://cloud.example.com\""));
        let (document, _) = super::document::parse_configuration(&text).unwrap();
        assert_eq!(document.environment.as_deref(), Some("company"));
        assert_eq!(
            document.environments["company"].native_client_id.as_deref(),
            Some("whalerein-cli")
        );
    }

    #[test]
    fn config_env_ac_22_managed_authority_match_and_state_dir_root_are_preserved() {
        let base = tempfile::tempdir().unwrap();
        let managed = base.path().join("managed");
        fixture_cordis_managed(&managed, None, 1);
        let (api, issuer) = local_doc(&managed);
        fixture_prototype(&managed, &api, &issuer);
        let pin = digest_value(&serde_json::json!(["official-local", api, issuer]));
        let pin = pin.trim_start_matches("sha256:").to_string();
        fixture_cordis_managed(&managed, Some(&pin), 1);
        let dsh = managed.join("dsh");
        let preview =
            preview_import(&preview_options(&dsh, &base.path().join("m.toml"), false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Ready);
    }

    #[test]
    fn config_env_ac_24_journal_preview_mismatch_and_retain_only_target_conflict() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        fixture_core_profiles(&source, "22222222222222222222222222222222", false);
        let target = base.path().join("target").join("config.toml");
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let jpath = journal_path(&target);
        let mut journal = initial_journal(&preview, None);
        journal.preview_id = "sha256:other".into();
        journal.state = ImportJournalState::Complete;
        journal.target.revision_after = Some("sha256:missing".into());
        write_journal(&jpath, &journal).unwrap();
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_conflict");

        let standalone = base.path().join("standalone");
        fixture_cordis_standalone(&standalone, false);
        let starget = base.path().join("s-target").join("config.toml");
        let preview =
            preview_import(&preview_options(&standalone.join("dsh"), &starget, false)).unwrap();
        let spath = journal_path(&starget);
        let mut journal = initial_journal(&preview, None);
        journal.state = ImportJournalState::Complete;
        write_journal(&spath, &journal).unwrap();
        write(&starget, b"version = 1\n");
        let recovery = recover_import(&starget).unwrap();
        assert_eq!(recovery.status, ImportRecoveryStatus::Conflict);
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_conflict");
    }

    #[test]
    fn config_env_ac_24_standalone_secure_handoff_commit_is_retain_only_and_no_config() {
        let base = tempfile::tempdir().unwrap();
        let standalone = base.path().join("standalone");
        fixture_cordis_standalone(&standalone, true);
        let target = base.path().join("target").join("config.toml");
        let before = tree_hash(&standalone);
        let preview =
            preview_import(&preview_options(&standalone.join("dsh"), &target, true)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::RetainOnly);
        assert!(preview.secure_store_handoff.as_ref().unwrap().required);
        let outcome = commit_import(
            &commit_of(&preview),
            &ImportCommitOptions {
                secure_store_available: true,
            },
        )
        .unwrap();
        assert_eq!(outcome.status, ImportOutcomeStatus::Imported);
        assert!(!target.exists());
        assert_eq!(tree_hash(&standalone), before);
        let journal = read_journal(&journal_path(&target)).unwrap().unwrap();
        assert_eq!(journal.state, ImportJournalState::Complete);
        assert!(journal.target.revision_after.is_none());
    }

    #[test]
    fn config_env_ac_24_unsupported_cordis_mode_and_malformed_host_records_conflict() {
        let base = tempfile::tempdir().unwrap();
        let standalone = base.path().join("standalone");
        fixture_cordis_standalone(&standalone, false);
        write(
            &standalone.join("dsh").join("cordis.patch.yml"),
            b"- id: whalerein-dsh\n  config:\n    connection_mode: future\n    environment: local\n",
        );
        let preview = preview_import(&preview_options(
            &standalone.join("dsh"),
            &base.path().join("s.toml"),
            false,
        ))
        .unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "unsupported_source_version"));
        let error =
            commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap_err();
        assert_eq!(code(&error), "import_unsupported");

        let managed = base.path().join("managed-bad");
        let dsh = managed.join("dsh");
        let registry = serde_json::json!({
            "schema": "whalerein-host/v1/profiles",
            "schemaVersion": 1,
            "revision": 1,
            "activationGeneration": 0,
            "active": "x",
            "profiles": [
                {"environment": "local", "label": "no-id", "mode": "managed", "service": {"id": "official-local"}, "stateDir": ".", "bindings": {}, "pending": {}, "legacy": false},
                {"id": "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy", "environment": "custom", "label": "custom", "mode": "managed", "service": {"id": "custom"}, "stateDir": ".", "bindings": {}, "pending": {}, "legacy": true},
                {"id": "x", "environment": "local", "mode": "managed", "service": {"id": "official-local"}, "stateDir": ".", "bindings": {}, "pending": {}, "legacy": false}
            ]
        });
        write(
            &dsh.join("host-profiles.json"),
            serde_json::to_vec(&registry).unwrap().as_slice(),
        );
        let preview =
            preview_import(&preview_options(&dsh, &base.path().join("h.toml"), false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "unsupported_environment"));
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "source_invalid"));
    }

    #[test]
    fn config_env_ac_22_managed_authority_pin_mismatch_is_a_conflict() {
        let base = tempfile::tempdir().unwrap();
        let managed = base.path().join("managed");
        fixture_cordis_managed(&managed, Some("deadbeef"), 1);
        let dsh = managed.join("dsh");
        let (api, issuer) = local_doc(&managed);
        fixture_prototype(&managed, &api, &issuer);
        let target = base.path().join("m.toml");
        let preview = preview_import(&preview_options(&dsh, &target, false)).unwrap();
        assert_eq!(preview.status, ImportPreviewStatus::Conflict);
        assert!(preview
            .conflicts
            .iter()
            .any(|issue| issue.code == "authority_mismatch"));
        assert!(!target.exists());
    }

    #[test]
    fn config_env_ac_21_prototype_confirm_publishes_exact_authority_and_retains_paths() {
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        let (api, issuer) = local_doc(&source);
        fixture_prototype(&source, &api, &issuer);
        let target = base.path().join("target").join("config.toml");
        let before = tree_hash(&source);
        let preview = preview_import(&preview_options(&source, &target, false)).unwrap();
        let outcome = commit_import(&commit_of(&preview), &ImportCommitOptions::default()).unwrap();
        assert_eq!(outcome.status, ImportOutcomeStatus::Imported);
        let text = std::fs::read_to_string(&target).unwrap();
        let (document, _) = super::document::parse_configuration(&text).unwrap();
        assert_eq!(document.environment.as_deref(), Some("local"));
        let local = document.environments.get("local").unwrap();
        assert_eq!(local.tls.as_ref().unwrap().ca_provider, TlsProvider::File);
        assert_eq!(tree_hash(&source), before);
        let journal = read_journal(&journal_path(&target)).unwrap().unwrap();
        assert!(journal.retained.iter().any(|reference| matches!(
            reference,
            RetainedReference::StatePath { path } if path.ends_with("clients")
        )));
    }
}
