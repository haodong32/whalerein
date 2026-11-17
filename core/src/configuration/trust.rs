//! Environment-scoped TLS trust resolution.
//!
//! Resolution is pure data: it reads the selected CA and returns PEM plus a
//! scope. It never installs a certificate, mutates a process-global TLS store,
//! runs a shell helper, or copies a CA private key.

use std::path::{Path, PathBuf};

use super::document::{EnvironmentDefinition, TlsProvider};
use super::error::ConfigurationError;
use super::home_dir;
use super::EnvironmentResolution;

/// Trust scope for the selected environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsTrustScope {
    /// Use the platform trust store; no private CA bytes are returned.
    System,
    /// Use the fixed per-user mkcert root CA.
    Mkcert,
    /// Use the explicit `ca_file` from the environment.
    File,
}

/// Loaded trust material. `ca_pem` is a public certificate bundle, never a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsTrust {
    pub scope: TlsTrustScope,
    pub ca_pem: Option<Vec<u8>>,
    pub ca_path: Option<PathBuf>,
}

/// Resolves trust for one configuration file's resolved environment.
#[derive(Debug, Clone)]
pub struct TrustResolver {
    config_path: PathBuf,
}

impl TrustResolver {
    pub fn new(config_path: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
        }
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn resolve(
        &self,
        resolution: &EnvironmentResolution,
    ) -> Result<TlsTrust, ConfigurationError> {
        self.resolve_definition(&resolution.definition)
    }

    /// Resolve trust for an effective definition. Custom environments never
    /// inherit the built-in local mkcert trust.
    pub fn resolve_definition(
        &self,
        definition: &EnvironmentDefinition,
    ) -> Result<TlsTrust, ConfigurationError> {
        let tls = definition
            .tls
            .as_ref()
            .ok_or_else(|| ConfigurationError::InvalidTls {
                message: "environment has no resolved TLS configuration".into(),
            })?;
        match tls.ca_provider {
            TlsProvider::System => Ok(TlsTrust {
                scope: TlsTrustScope::System,
                ca_pem: None,
                ca_path: None,
            }),
            TlsProvider::Mkcert => {
                if definition.environment.as_deref() != Some("local") {
                    return Err(ConfigurationError::InvalidTls {
                        message: "mkcert trust is only valid for the built-in local environment"
                            .into(),
                    });
                }
                let path = mkcert_ca_path().ok_or_else(|| ConfigurationError::InvalidTls {
                    message: "the mkcert root CA directory is unavailable; install mkcert or \
                              configure an explicit ca_file"
                        .into(),
                })?;
                let pem = read_ca(&path)?;
                Ok(TlsTrust {
                    scope: TlsTrustScope::Mkcert,
                    ca_pem: Some(pem),
                    ca_path: Some(path),
                })
            }
            TlsProvider::File => {
                let ca_file =
                    tls.ca_file
                        .as_deref()
                        .ok_or_else(|| ConfigurationError::InvalidTls {
                            message: "ca_file is required for the file provider".into(),
                        })?;
                let path = resolve_ca_path(&self.config_path, ca_file)?;
                let pem = read_ca(&path)?;
                Ok(TlsTrust {
                    scope: TlsTrustScope::File,
                    ca_pem: Some(pem),
                    ca_path: Some(path),
                })
            }
        }
    }
}

/// Resolve a relative or `~`-rooted CA path once. Relative paths are anchored
/// to the configuration file's parent directory.
pub fn resolve_ca_path(config_path: &Path, ca_file: &str) -> Result<PathBuf, ConfigurationError> {
    let path = if ca_file == "~" {
        home_dir().ok_or_else(|| ConfigurationError::InvalidTls {
            message: "cannot expand ~ because no home directory is available".into(),
        })?
    } else if let Some(rest) = ca_file
        .strip_prefix("~/")
        .or_else(|| ca_file.strip_prefix("~\\"))
    {
        home_dir()
            .ok_or_else(|| ConfigurationError::InvalidTls {
                message: "cannot expand ~ because no home directory is available".into(),
            })?
            .join(rest)
    } else {
        PathBuf::from(ca_file)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path))
    }
}

/// Fixed, shell-free mkcert discovery for the current platform.
pub fn mkcert_ca_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("mkcert")
                .join("rootCA.pem")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".local").join("share")))?;
        Some(base.join("mkcert").join("rootCA.pem"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join("AppData").join("Local")))
            .map(|base| base.join("mkcert").join("rootCA.pem"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn read_ca(path: &Path) -> Result<Vec<u8>, ConfigurationError> {
    let bytes = std::fs::read(path).map_err(|error| {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "the selected CA certificate was not found; install it or choose a valid ca_file"
        } else {
            "the selected CA certificate could not be read"
        };
        ConfigurationError::InvalidTls {
            message: message.into(),
        }
    })?;
    if !bytes
        .windows(b"-----BEGIN CERTIFICATE-----".len())
        .any(|window| window == b"-----BEGIN CERTIFICATE-----")
    {
        return Err(ConfigurationError::InvalidTls {
            message: "the selected CA file is not a PEM certificate bundle".into(),
        });
    }
    let certificates: Vec<_> = rustls_pemfile::certs(&mut &bytes[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ConfigurationError::InvalidTls {
            message: "the selected CA file is not a valid PEM certificate bundle".into(),
        })?;
    if certificates.is_empty() {
        return Err(ConfigurationError::InvalidTls {
            message: "the selected CA file contains no certificates".into(),
        });
    }
    Ok(bytes)
}
