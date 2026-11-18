//! Shared official service catalog, trust lookup, and atomic file storage.

pub(crate) mod document;
mod error;
#[cfg(test)]
mod tests;
pub(crate) mod trust;
pub(crate) mod writer;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(test)]
pub use document::CONFIG_CATALOG_JSON;
pub use document::{EnvironmentDefinition, EnvironmentMode, TlsConfig, TlsProvider};
pub use error::ConfigurationError;
pub use trust::TrustResolver;

/// Where the effective environment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectionSource {
    #[cfg(test)]
    Invocation,
    File,
    Default,
}

/// Whether the effective definition is catalog authority or a user entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefinitionSource {
    Builtin,
    #[cfg(test)]
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
