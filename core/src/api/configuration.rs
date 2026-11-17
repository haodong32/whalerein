//! Public typed client-environment configuration API.
//!
//! CLI, DSH adapters and future bindings call only these typed operations. They
//! never parse TOML, resolve catalog authority, or write the configuration file
//! themselves.

pub use crate::configuration::{
    commit_import, default_config_path, detect_legacy, legacy_state_present, mkcert_ca_path,
    preview_import, recover_import, resolve_ca_path, resolve_environment, BuiltinSpecialization,
    ConfigErrorView, ConfigOpenOptions, ConfigPatch, ConfigSnapshot, ConfigSource,
    ConfigSourceKind, ConfigValidation, ConfigView, Configuration, ConfigurationDocument,
    ConfigurationError, CustomEnvironmentInput, DefinitionSource, EnvironmentDefinition,
    EnvironmentMode, EnvironmentResolution, FieldSource, ImportCommit, ImportCommitOptions,
    ImportConflict, ImportIssueKind, ImportJournalState, ImportOutcome, ImportOutcomeStatus,
    ImportPreview, ImportPreviewOptions, ImportPreviewStatus, ImportRecovery, ImportRecoveryStatus,
    LegacySource, LegacySourceKind, LiveReadiness, ProposedConfiguration, ProposedEnvironment,
    RetainedReference, SecureStoreHandoff, SelectionSource, TlsConfig, TlsConfiguration,
    TlsProvider, TlsTrust, TlsTrustScope, TrustResolver, ValidationScope, CONFIG_CATALOG_JSON,
    CONFIG_CORPUS_JSON, CONFIG_VERSION, IMPORT_JOURNAL_SCHEMA, IMPORT_JOURNAL_VERSION,
};
