//! The built-in official service catalog and its normalized authority types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use super::ConfigurationError;

pub const CATALOG_SCHEMA: &str = "whalerein/v1/client-environment-catalog";
pub const CATALOG_VERSION: i64 = 1;
pub const BUILTIN_NAMES: [&str; 3] = ["local", "staging", "production"];

/// Canonical Rust distribution input. The binary never resolves a source-tree path.
pub const CONFIG_CATALOG_JSON: &str =
    include_str!("../../../contracts/client-environment/catalog.json");
/// Environment operating mode. A hostname never implies one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentMode {
    Account,
    #[cfg(test)]
    Standalone,
}

#[cfg(test)]
impl EnvironmentMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Standalone => "standalone",
        }
    }
}

/// Certificate authority provider for one environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsProvider {
    System,
    Mkcert,
    File,
}

impl TlsProvider {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "mkcert" => Some(Self::Mkcert),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// Declared TLS trust for an environment. Resolution applies defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TlsConfig {
    pub ca_provider: TlsProvider,
    pub ca_file: Option<String>,
}

/// One normalized environment entry, used both for the parsed document and for
/// an effective resolved definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentDefinition {
    pub id: String,
    pub mode: Option<EnvironmentMode>,
    pub label: Option<String>,
    pub api_origin: Option<String>,
    pub issuer: Option<String>,
    pub native_client_id: Option<String>,
    pub environment: Option<String>,
    pub tls: Option<TlsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogFile {
    schema: String,
    version: i64,
    environments: BTreeMap<String, CatalogEnvironment>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CatalogEnvironment {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) mode: String,
    pub(crate) api_origin: String,
    pub(crate) issuer: String,
    pub(crate) native_client_id: String,
    pub(crate) environment: String,
    pub(crate) tls: CatalogTls,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CatalogTls {
    pub(crate) ca_provider: String,
    pub(crate) ca_file: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Catalog {
    pub(crate) environments: BTreeMap<String, CatalogEnvironment>,
}

impl Catalog {
    pub(crate) fn get(&self, name: &str) -> Option<&CatalogEnvironment> {
        self.environments.get(name)
    }
}

/// Parse and validate the embedded catalog. It is revalidated per open so a
/// malformed build input cannot silently define built-in authority.
pub(crate) fn catalog() -> Result<Catalog, ConfigurationError> {
    parse_catalog(CONFIG_CATALOG_JSON)
}

pub(crate) fn parse_catalog(json: &str) -> Result<Catalog, ConfigurationError> {
    let parsed: CatalogFile =
        serde_json::from_str(json).map_err(|_| ConfigurationError::CatalogInvalid)?;
    if parsed.schema != CATALOG_SCHEMA || parsed.version != CATALOG_VERSION {
        return Err(ConfigurationError::CatalogInvalid);
    }
    for name in BUILTIN_NAMES {
        let entry = parsed
            .environments
            .get(name)
            .ok_or(ConfigurationError::CatalogInvalid)?;
        if entry.mode != "account"
            || entry.id.is_empty()
            || entry.native_client_id.is_empty()
            || entry.environment != name
        {
            return Err(ConfigurationError::CatalogInvalid);
        }
        let api = validate_url(&entry.api_origin, "api_origin")?;
        let issuer = validate_url(&entry.issuer, "issuer")?;
        if api == issuer {
            return Err(ConfigurationError::CatalogInvalid);
        }
        let provider =
            TlsProvider::parse(&entry.tls.ca_provider).ok_or(ConfigurationError::CatalogInvalid)?;
        if entry.tls.ca_file.is_some() {
            return Err(ConfigurationError::CatalogInvalid);
        }
        if (name == "local") != (provider == TlsProvider::Mkcert) {
            return Err(ConfigurationError::CatalogInvalid);
        }
    }
    if parsed.environments.len() != BUILTIN_NAMES.len() {
        return Err(ConfigurationError::CatalogInvalid);
    }
    Ok(Catalog {
        environments: parsed.environments,
    })
}

/// Validate one account URL and return its origin for distinctness checks.
pub(crate) fn validate_url(value: &str, field: &'static str) -> Result<String, ConfigurationError> {
    if value.is_empty() || value.len() > 2048 {
        return Err(ConfigurationError::InvalidUrl { field });
    }
    let url = Url::parse(value).map_err(|_| ConfigurationError::InvalidUrl { field })?;
    let host = url
        .host_str()
        .ok_or(ConfigurationError::InvalidUrl { field })?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback)) {
        return Err(ConfigurationError::InvalidUrl { field });
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigurationError::InvalidUrl { field });
    }
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("{}://{}{}", url.scheme(), host, port))
}
