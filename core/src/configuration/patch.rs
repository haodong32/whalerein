//! Typed configuration patches.
//!
//! The patch surface deliberately cannot address arbitrary keys: callers set a
//! default, upsert a complete custom environment, specialize a built-in label
//! and trust, or remove a custom environment. Anything else stays in the file
//! untouched and unreachable through this API.

use toml_edit::{DocumentMut, Item, Table};

use super::document::{validate_identifier, Catalog, EnvironmentMode, TlsProvider};
use super::ConfigurationError;

/// Class of trust configuration accepted by a typed patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsConfiguration {
    System,
    Mkcert,
    File { ca_file: String },
}

impl TlsConfiguration {
    fn provider(&self) -> TlsProvider {
        match self {
            Self::System => TlsProvider::System,
            Self::Mkcert => TlsProvider::Mkcert,
            Self::File { .. } => TlsProvider::File,
        }
    }

    fn ca_file(&self) -> Option<&str> {
        match self {
            Self::File { ca_file } => Some(ca_file),
            _ => None,
        }
    }
}

/// A complete custom environment upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomEnvironmentInput {
    pub environment: String,
    pub label: Option<String>,
    pub mode: EnvironmentMode,
    pub api_origin: Option<String>,
    pub issuer: Option<String>,
    pub native_client_id: Option<String>,
    pub tls: Option<TlsConfiguration>,
}

/// Label/trust specialization for a built-in environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinSpecialization {
    pub environment: String,
    pub label: Option<String>,
    pub clear_label: bool,
    pub tls: Option<TlsConfiguration>,
    pub clear_tls: bool,
}

/// One typed settings patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigPatch {
    /// Set or clear the persistent default environment.
    SetDefaultEnvironment { environment: Option<String> },
    /// Create or replace a complete custom environment.
    UpsertCustomEnvironment(CustomEnvironmentInput),
    /// Change only the display label and/or TLS trust of a built-in.
    SpecializeBuiltin(BuiltinSpecialization),
    /// Remove a custom environment. Built-ins cannot be removed.
    RemoveCustomEnvironment { environment: String },
}

pub(crate) fn apply_patch(
    document: &mut DocumentMut,
    patch: &ConfigPatch,
    catalog: &Catalog,
) -> Result<(), ConfigurationError> {
    match patch {
        ConfigPatch::SetDefaultEnvironment { environment } => {
            if let Some(environment) = environment {
                validate_identifier(environment)?;
                let exists = document
                    .as_table()
                    .get("environments")
                    .and_then(Item::as_table_like)
                    .is_some_and(|table| table.contains_key(environment));
                if !catalog.is_builtin(environment) && !exists {
                    return Err(ConfigurationError::UnknownEnvironment {
                        environment: environment.clone(),
                    });
                }
                set_string(document.as_table_mut(), "environment", environment);
            } else {
                document.as_table_mut().remove("environment");
            }
            Ok(())
        }
        ConfigPatch::UpsertCustomEnvironment(input) => {
            validate_identifier(&input.environment)?;
            if catalog.is_builtin(&input.environment) {
                return Err(ConfigurationError::AuthorityOverride {
                    environment: input.environment.clone(),
                });
            }
            match input.mode {
                EnvironmentMode::Account => {
                    if input.api_origin.as_deref().unwrap_or_default().is_empty()
                        || input.issuer.as_deref().unwrap_or_default().is_empty()
                        || input
                            .native_client_id
                            .as_deref()
                            .unwrap_or_default()
                            .is_empty()
                    {
                        return Err(ConfigurationError::InvalidModeFields {
                            environment: input.environment.clone(),
                        });
                    }
                }
                EnvironmentMode::Standalone => {
                    if input.api_origin.is_some()
                        || input.issuer.is_some()
                        || input.native_client_id.is_some()
                    {
                        return Err(ConfigurationError::InvalidModeFields {
                            environment: input.environment.clone(),
                        });
                    }
                }
            }
            let table = environment_table(document, &input.environment);
            for key in [
                "label",
                "mode",
                "api_origin",
                "issuer",
                "native_client_id",
                "tls",
                "environment",
            ] {
                table.remove(key);
            }
            set_string(table, "mode", input.mode.as_str());
            if let Some(label) = &input.label {
                set_string(table, "label", label);
            }
            if let Some(value) = &input.api_origin {
                set_string(table, "api_origin", value);
            }
            if let Some(value) = &input.issuer {
                set_string(table, "issuer", value);
            }
            if let Some(value) = &input.native_client_id {
                set_string(table, "native_client_id", value);
            }
            write_tls(table, input.tls.as_ref());
            Ok(())
        }
        ConfigPatch::SpecializeBuiltin(input) => {
            validate_identifier(&input.environment)?;
            if !catalog.is_builtin(&input.environment) {
                return Err(ConfigurationError::InvalidPatch {
                    message: "only built-in environments accept label/trust specialization".into(),
                });
            }
            let has_label_change = input.clear_label || input.label.is_some();
            let has_tls_change = input.clear_tls || input.tls.is_some();
            if !has_label_change && !has_tls_change {
                return Ok(());
            }
            let table = environment_table(document, &input.environment);
            if input.clear_label {
                table.remove("label");
            } else if let Some(label) = &input.label {
                set_string(table, "label", label);
            }
            if input.clear_tls {
                table.remove("tls");
            } else if let Some(tls) = &input.tls {
                write_tls(table, Some(tls));
            }
            Ok(())
        }
        ConfigPatch::RemoveCustomEnvironment { environment } => {
            validate_identifier(environment)?;
            if catalog.is_builtin(environment) {
                return Err(ConfigurationError::AuthorityOverride {
                    environment: environment.clone(),
                });
            }
            let Some(item) = document
                .as_table_mut()
                .get_mut("environments")
                .and_then(Item::as_table_like_mut)
                .and_then(|table| table.remove(environment))
            else {
                return Err(ConfigurationError::UnknownEnvironment {
                    environment: environment.clone(),
                });
            };
            drop(item);
            Ok(())
        }
    }
}

fn environment_table<'a>(document: &'a mut DocumentMut, name: &str) -> &'a mut Table {
    let root = document.as_table_mut();
    let environments = root.entry("environments").or_insert_with(toml_edit::table);
    if !environments.is_table() {
        *environments = toml_edit::table();
    }
    let table = environments
        .as_table_mut()
        .expect("environments was normalized to a table");
    let entry = table.entry(name).or_insert_with(toml_edit::table);
    if !entry.is_table() {
        *entry = toml_edit::table();
    }
    entry
        .as_table_mut()
        .expect("environment entry was normalized to a table")
}

fn write_tls(table: &mut Table, tls: Option<&TlsConfiguration>) {
    let Some(tls) = tls else {
        table.remove("tls");
        return;
    };
    let tls_item = table.entry("tls").or_insert_with(toml_edit::table);
    if !tls_item.is_table() {
        *tls_item = toml_edit::table();
    }
    let tls_table = tls_item
        .as_table_mut()
        .expect("tls was normalized to a table");
    tls_table.remove("ca_provider");
    tls_table.remove("ca_file");
    set_string(tls_table, "ca_provider", tls.provider().as_str());
    if let Some(ca_file) = tls.ca_file() {
        set_string(tls_table, "ca_file", ca_file);
    }
}

fn set_string(table: &mut Table, key: &str, value: &str) {
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|value| value.decor().clone());
    let mut item = toml_edit::value(value);
    if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
        *value.decor_mut() = decor;
    }
    table.insert(key, item);
}
