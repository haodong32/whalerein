//! Version-1 TOML parsing, validation, normalization and the built-in catalog.
//!
//! Parsing is delegated to the maintained `toml_edit` parser. Validation order
//! follows the shared client-environment contract so Rust and Node report the
//! same first error for every golden fixture.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, TableLike};
use url::Url;

use super::ConfigurationError;

pub const CONFIG_VERSION: i64 = 1;
pub const CATALOG_SCHEMA: &str = "whalerein/v1/client-environment-catalog";
pub const CATALOG_VERSION: i64 = 1;
pub const BUILTIN_NAMES: [&str; 3] = ["local", "staging", "production"];

/// Canonical Rust distribution input. The binary never resolves a source-tree path.
pub const CONFIG_CATALOG_JSON: &str =
    include_str!("../../../contracts/client-environment/catalog.json");
/// Canonical golden corpus embedded for the parity/packaging tests.
pub const CONFIG_CORPUS_JSON: &str =
    include_str!("../../../contracts/client-environment/cases.json");

/// Environment operating mode. A hostname never implies one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentMode {
    Account,
    Standalone,
}

impl EnvironmentMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "account" => Some(Self::Account),
            "standalone" => Some(Self::Standalone),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
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

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Mkcert => "mkcert",
            Self::File => "file",
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

/// Normalized version-1 configuration document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigurationDocument {
    pub version: i64,
    pub environment: Option<String>,
    pub environments: BTreeMap<String, EnvironmentDefinition>,
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

    pub(crate) fn is_builtin(&self, name: &str) -> bool {
        self.environments.contains_key(name)
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

/// Parse and validate one version-1 document.
///
/// The editable form is returned so a later revisioned update can modify only
/// the patched values while preserving comments and untouched keys.
pub(crate) fn parse_configuration(
    input: &str,
) -> Result<(ConfigurationDocument, DocumentMut), ConfigurationError> {
    let document: DocumentMut = input.parse().map_err(|error: toml_edit::TomlError| {
        if error.message().contains("duplicate key") {
            ConfigurationError::DuplicateKey
        } else {
            ConfigurationError::InvalidSyntax
        }
    })?;

    let version = document
        .as_table()
        .get("version")
        .and_then(Item::as_integer)
        .filter(|version| *version == CONFIG_VERSION)
        .ok_or(ConfigurationError::UnsupportedVersion)?;

    for (field, _) in document.as_table().iter() {
        if !matches!(field, "version" | "environment" | "environments") {
            return Err(ConfigurationError::UnknownField {
                field: display_field(field),
            });
        }
    }

    let selection = match document.as_table().get("environment") {
        Some(item) => {
            let value = item.as_str().ok_or(ConfigurationError::InvalidIdentifier {
                field: "environment",
            })?;
            validate_identifier(value)?;
            Some(value.to_string())
        }
        None => None,
    };

    let catalog = catalog()?;
    let mut environments = BTreeMap::new();
    if let Some(item) = document.as_table().get("environments") {
        let table = item
            .as_table_like()
            .ok_or(ConfigurationError::UnknownField {
                field: "environments".into(),
            })?;
        // Ascending name order makes the first reported environment error
        // deterministic across runtimes and map implementations.
        let mut entries: Vec<(&str, &Item)> = table.iter().collect();
        entries.sort_by(|left, right| left.0.cmp(right.0));
        for (name, item) in entries {
            validate_identifier(name)?;
            let entry = item
                .as_table_like()
                .ok_or(ConfigurationError::UnknownField {
                    field: "environments".into(),
                })?;
            let builtin = catalog.is_builtin(name);
            let definition = parse_environment(name, entry, builtin)?;
            environments.insert(name.to_string(), definition);
        }
    }

    Ok((
        ConfigurationDocument {
            version,
            environment: selection,
            environments,
        },
        document,
    ))
}

fn parse_environment(
    name: &str,
    table: &dyn TableLike,
    builtin: bool,
) -> Result<EnvironmentDefinition, ConfigurationError> {
    if builtin {
        for (field, _) in table.iter() {
            if !matches!(
                field,
                "label"
                    | "mode"
                    | "api_origin"
                    | "issuer"
                    | "native_client_id"
                    | "environment"
                    | "tls"
            ) {
                return Err(ConfigurationError::UnknownField {
                    field: display_field(field),
                });
            }
        }
        for (field, _) in table.iter() {
            if !matches!(field, "label" | "tls") {
                return Err(ConfigurationError::AuthorityOverride {
                    environment: name.to_string(),
                });
            }
        }
        let label = optional_string(table, "label")?;
        let tls = optional_tls(table, name, true)?;
        return Ok(EnvironmentDefinition {
            id: name.to_string(),
            mode: None,
            label,
            api_origin: None,
            issuer: None,
            native_client_id: None,
            environment: None,
            tls,
        });
    }

    for (field, _) in table.iter() {
        if !matches!(
            field,
            "label" | "mode" | "api_origin" | "issuer" | "native_client_id" | "tls"
        ) {
            return Err(ConfigurationError::UnknownField {
                field: display_field(field),
            });
        }
    }
    let label = optional_string(table, "label")?;
    let mode = table
        .get("mode")
        .and_then(Item::as_str)
        .and_then(EnvironmentMode::parse)
        .ok_or_else(|| ConfigurationError::InvalidMode {
            environment: name.to_string(),
        })?;
    let account_fields = ["api_origin", "issuer", "native_client_id"];
    let mut api_origin = None;
    let mut issuer = None;
    let mut native_client_id = None;
    match mode {
        EnvironmentMode::Account => {
            for field in account_fields {
                let value = table
                    .get(field)
                    .and_then(Item::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| ConfigurationError::InvalidModeFields {
                        environment: name.to_string(),
                    })?;
                match field {
                    "api_origin" => api_origin = Some(value.to_string()),
                    "issuer" => issuer = Some(value.to_string()),
                    _ => {
                        if value.len() > 128
                            || value.chars().any(|c| c.is_control() || c.is_whitespace())
                        {
                            return Err(ConfigurationError::InvalidModeFields {
                                environment: name.to_string(),
                            });
                        }
                        native_client_id = Some(value.to_string());
                    }
                }
            }
            let api = validate_url(api_origin.as_deref().unwrap_or_default(), "api_origin")?;
            let idp = validate_url(issuer.as_deref().unwrap_or_default(), "issuer")?;
            if api == idp {
                return Err(ConfigurationError::InvalidUrl { field: "issuer" });
            }
        }
        EnvironmentMode::Standalone => {
            for field in account_fields {
                if table.get(field).is_some() {
                    return Err(ConfigurationError::InvalidModeFields {
                        environment: name.to_string(),
                    });
                }
            }
        }
    }
    let tls = optional_tls(table, name, false)?;
    Ok(EnvironmentDefinition {
        id: name.to_string(),
        mode: Some(mode),
        label,
        api_origin,
        issuer,
        native_client_id,
        environment: None,
        tls,
    })
}

fn optional_string(
    table: &dyn TableLike,
    field: &'static str,
) -> Result<Option<String>, ConfigurationError> {
    match table.get(field) {
        Some(item) => item.as_str().map(|value| Some(value.to_string())).ok_or(
            ConfigurationError::UnknownField {
                field: field.to_string(),
            },
        ),
        None => Ok(None),
    }
}

fn optional_tls(
    table: &dyn TableLike,
    environment: &str,
    builtin: bool,
) -> Result<Option<TlsConfig>, ConfigurationError> {
    let Some(item) = table.get("tls") else {
        return Ok(None);
    };
    let tls = item
        .as_table_like()
        .ok_or_else(|| ConfigurationError::InvalidTls {
            message: "tls must be a table".into(),
        })?;
    for (field, _) in tls.iter() {
        if !matches!(field, "ca_provider" | "ca_file") {
            return Err(ConfigurationError::UnknownField {
                field: display_field(field),
            });
        }
    }
    let provider = match tls.get("ca_provider") {
        Some(item) => TlsProvider::parse(item.as_str().unwrap_or_default()).ok_or_else(|| {
            ConfigurationError::InvalidTls {
                message: "ca_provider must be system, mkcert or file".into(),
            }
        })?,
        None if builtin && environment == "local" => TlsProvider::Mkcert,
        None => TlsProvider::System,
    };
    if provider == TlsProvider::Mkcert && (!builtin || environment != "local") {
        return Err(ConfigurationError::InvalidTls {
            message: "mkcert is only valid for the built-in local environment".into(),
        });
    }
    let ca_file = match tls.get("ca_file") {
        Some(item) => Some(
            item.as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ConfigurationError::InvalidTls {
                    message: "ca_file must be a non-empty string".into(),
                })?
                .to_string(),
        ),
        None => None,
    };
    if provider == TlsProvider::File && ca_file.is_none() {
        return Err(ConfigurationError::InvalidTls {
            message: "ca_file is required when ca_provider is file".into(),
        });
    }
    if provider != TlsProvider::File && ca_file.is_some() {
        return Err(ConfigurationError::InvalidTls {
            message: "ca_file is only valid when ca_provider is file".into(),
        });
    }
    Ok(Some(TlsConfig {
        ca_provider: provider,
        ca_file,
    }))
}

pub(crate) fn validate_identifier(value: &str) -> Result<(), ConfigurationError> {
    let valid = value.len() <= 64
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(ConfigurationError::InvalidIdentifier {
            field: "environment",
        })
    }
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

fn display_field(field: &str) -> String {
    field.chars().filter(|c| !c.is_control()).take(64).collect()
}
