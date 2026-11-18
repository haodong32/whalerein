//! Provider-issued Cloud account sessions and official environment names.

pub(crate) mod native;

use serde::{Deserialize, Serialize};

/// Visible environment classification. Standalone is not an account service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Environment {
    Production,
    Staging,
    Local,
    Custom,
    Standalone,
}

#[cfg(test)]
mod legacy;
#[cfg(test)]
pub use legacy::*;
