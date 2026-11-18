//! Stable client-configuration errors.
//!
//! Every variant maps to one shared contract code. Messages never contain file
//! bytes, URL credentials, certificate bytes, or private key material.

/// Typed client-environment configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("invalid configuration URL for {field}")]
    InvalidUrl { field: &'static str },
    #[error("invalid TLS configuration: {message}")]
    InvalidTls { message: String },
    #[cfg(test)]
    #[error("environment mode is missing or invalid for {environment}")]
    InvalidMode { environment: String },
    #[cfg(test)]
    #[error("environment fields do not match its mode for {environment}")]
    InvalidModeFields { environment: String },
    #[cfg(test)]
    #[error("built-in environment authority cannot be overridden for {environment}")]
    AuthorityOverride { environment: String },
    #[error("configuration file could not be written")]
    Storage,
    #[error("embedded configuration catalog is invalid")]
    CatalogInvalid,
}

impl ConfigurationError {
    /// Stable shared contract code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Storage => "unreadable_config",
            Self::CatalogInvalid => "invalid_syntax",
            Self::InvalidUrl { .. } => "invalid_url",
            Self::InvalidTls { .. } => "invalid_tls",
            #[cfg(test)]
            Self::InvalidMode { .. } => "invalid_mode",
            #[cfg(test)]
            Self::InvalidModeFields { .. } => "invalid_mode_fields",
            #[cfg(test)]
            Self::AuthorityOverride { .. } => "authority_override",
        }
    }
}
