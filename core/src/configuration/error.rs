//! Stable client-configuration errors.
//!
//! Every variant maps to one shared contract code. Messages never contain file
//! bytes, URL credentials, certificate bytes, or private key material.

use crate::configuration::patch::ConfigPatch;

/// Typed client-environment configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("configuration file is missing")]
    MissingConfig,
    #[error("configuration file could not be read")]
    UnreadableConfig,
    #[error("configuration file is not valid TOML")]
    InvalidSyntax,
    #[error("configuration declares a duplicate key")]
    DuplicateKey,
    #[error("configuration version is not supported")]
    UnsupportedVersion,
    #[error("unknown configuration field: {field}")]
    UnknownField { field: String },
    #[error("invalid configuration identifier for {field}")]
    InvalidIdentifier { field: &'static str },
    #[error("invalid configuration URL for {field}")]
    InvalidUrl { field: &'static str },
    #[error("invalid TLS configuration: {message}")]
    InvalidTls { message: String },
    #[error("environment mode is missing or invalid for {environment}")]
    InvalidMode { environment: String },
    #[error("environment fields do not match its mode for {environment}")]
    InvalidModeFields { environment: String },
    #[error("built-in environment authority cannot be overridden for {environment}")]
    AuthorityOverride { environment: String },
    #[error("unknown environment: {environment}")]
    UnknownEnvironment { environment: String },
    #[error("configuration revision conflict")]
    RevisionConflict {
        expected: Option<String>,
        current_revision: Option<String>,
        draft: Box<ConfigPatch>,
    },
    #[error("configuration file could not be written")]
    Storage,
    #[error("invalid configuration patch: {message}")]
    InvalidPatch { message: String },
    #[error("legacy import conflicts with the current state: {message}")]
    ImportConflict { message: String },
    #[error("legacy import configuration revision conflict")]
    ImportRevisionConflict {
        expected: Option<String>,
        current: Option<String>,
    },
    #[error("legacy import is unsupported: {message}")]
    ImportUnsupported { message: String },
    #[error("legacy import cannot use secure storage: {message}")]
    ImportStorageUnavailable { message: String },
    #[error("embedded configuration catalog is invalid")]
    CatalogInvalid,
}

impl ConfigurationError {
    /// Stable shared contract code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingConfig => "missing_config",
            Self::UnreadableConfig | Self::Storage => "unreadable_config",
            Self::InvalidSyntax | Self::CatalogInvalid => "invalid_syntax",
            Self::DuplicateKey => "duplicate_key",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnknownField { .. } => "unknown_field",
            Self::InvalidIdentifier { .. } => "invalid_identifier",
            Self::InvalidUrl { .. } => "invalid_url",
            Self::InvalidTls { .. } => "invalid_tls",
            Self::InvalidMode { .. } => "invalid_mode",
            Self::InvalidModeFields { .. } => "invalid_mode_fields",
            Self::AuthorityOverride { .. } => "authority_override",
            Self::UnknownEnvironment { .. } => "unknown_environment",
            Self::RevisionConflict { .. } => "revision_conflict",
            Self::InvalidPatch { .. } => "invalid_argument",
            Self::ImportConflict { .. } => "import_conflict",
            Self::ImportRevisionConflict { .. } => "revision_conflict",
            Self::ImportUnsupported { .. } => "import_unsupported",
            Self::ImportStorageUnavailable { .. } => "import_storage_unavailable",
        }
    }

    /// Environment named by this failure, when the contract carries one.
    pub fn environment(&self) -> Option<&str> {
        match self {
            Self::InvalidMode { environment }
            | Self::InvalidModeFields { environment }
            | Self::AuthorityOverride { environment }
            | Self::UnknownEnvironment { environment } => Some(environment),
            _ => None,
        }
    }

    /// Revision currently on disk for a stale update.
    pub fn current_revision(&self) -> Option<&str> {
        match self {
            Self::RevisionConflict {
                current_revision, ..
            } => current_revision.as_deref(),
            Self::ImportRevisionConflict { current, .. } => current.as_deref(),
            _ => None,
        }
    }

    /// Caller draft retained by a stale update.
    pub fn draft(&self) -> Option<&ConfigPatch> {
        match self {
            Self::RevisionConflict { draft, .. } => Some(draft.as_ref()),
            _ => None,
        }
    }
}
