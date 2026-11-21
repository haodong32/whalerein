//! The selected client configuration and durable instance authority.

use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{
    CoreError, CoreResult, Environment, SecretError, SecretStore, TransportBudget,
    TransportSettings,
};
use crate::configuration::{
    document::catalog, writer, DefinitionSource, EnvironmentDefinition, EnvironmentMode,
    EnvironmentResolution, SelectionSource, TlsConfig, TlsProvider, TrustResolver,
};

const MANIFEST: &str = "instance.json";
const RUNTIME: &str = "state";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientService {
    #[serde(rename = "whalerein_cloud")]
    WhaleReinCloud,
    Standalone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub version: u32,
    pub service: ClientService,
    pub data_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_environment: Option<Environment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standalone_ca_certificate: Option<PathBuf>,
    /// Explicit CA bundle for the built-in local Cloud environment.
    ///
    /// The built-in local profile resolves a per-user mkcert root, which does
    /// not exist on mobile clients. This additive override lets a client
    /// (currently the iOS Debug build) import that public root bundle and pin it
    /// to the local instance. It is restricted to the local Cloud environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_ca_certificate: Option<PathBuf>,
    /// Validated packaged transport profile and optional local overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportSettings>,
}

impl ClientConfig {
    pub fn new(service: ClientService, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            version: VERSION,
            service,
            data_dir: data_dir.into(),
            cloud_environment: None,
            standalone_ca_certificate: None,
            cloud_ca_certificate: None,
            transport: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountScope {
    pub issuer: String,
    pub subject: String,
    pub workspace: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    version: u32,
    pub id: String,
    service: ClientService,
    environment: Option<String>,
    issuer: Option<String>,
    pub account: Option<AccountScope>,
}

#[derive(Clone)]
pub(crate) struct Instance {
    pub configuration: ClientConfig,
    pub config_path: PathBuf,
    pub root: PathBuf,
    pub cloud: Option<EnvironmentResolution>,
    pub ca_pem: Option<Vec<u8>>,
    pub transport: TransportBudget,
}

fn config_error(path: &Path, reason: &str) -> CoreError {
    CoreError::Config(format!("{}: {reason}", path.display()))
}

fn data_error(path: &Path) -> CoreError {
    config_error(
        path,
        "data directory has unknown, incomplete or conflicting state; choose a fresh data_dir",
    )
}

fn path_error(path: &Path) -> CoreError {
    config_error(path, "path could not be resolved")
}

fn absolute(path: &Path) -> CoreResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(path_error(path));
    }
    std::path::absolute(path).map_err(|_| path_error(path))
}

/// Resolve existing ancestors (including aliases) while preserving fresh paths.
fn resolve_path(path: &Path) -> CoreResult<PathBuf> {
    let path = absolute(path)?;
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => {
                resolved.push(other);
                match fs::canonicalize(&resolved) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        if fs::symlink_metadata(&resolved).is_ok() {
                            return Err(path_error(&path));
                        }
                    }
                    Err(_) if matches!(other, Component::Prefix(_)) => {}
                    Err(_) => return Err(path_error(&path)),
                }
            }
        }
    }
    Ok(resolved)
}

impl Instance {
    pub fn open(path: &Path) -> CoreResult<Self> {
        let requested = absolute(path)?;
        let selected = fs::canonicalize(&requested).map_err(|_| {
            config_error(
                &requested,
                "selected configuration is missing or unreadable; create a version 1 TOML file with service and data_dir",
            )
        })?;
        let text = fs::read_to_string(&selected)
            .map_err(|_| config_error(&requested, "could not read the selected TOML file"))?;
        let configuration = toml_edit::de::from_str::<ClientConfig>(&text).map_err(|_| {
            config_error(
                &requested,
                "invalid version 1 TOML; check version, service, data_dir and supported fields",
            )
        })?;
        Self::resolve(selected, configuration)
    }

    /// Validate one configuration against the same rules used by `Core::open`
    /// without writing the file or creating instance state.
    pub(crate) fn validate(path: &Path, configuration: &ClientConfig) -> CoreResult<()> {
        let selected = resolve_path(path)?;
        Self::resolve(selected, configuration.clone()).map(|_| ())
    }

    pub fn write(path: &Path, configuration: &ClientConfig) -> CoreResult<()> {
        let selected = resolve_path(path)?;
        let instance = Self::resolve(selected.clone(), configuration.clone())?;
        let lock = writer::ConfigLock::acquire(&writer::lock_path(&selected))?;
        instance.manifest()?;
        let text = toml_edit::ser::to_string(configuration)
            .map_err(|_| config_error(&selected, "could not encode configuration"))?;
        writer::write_atomically(&selected, text.as_bytes())?;
        lock.commit();
        Ok(())
    }

    fn resolve(config_path: PathBuf, configuration: ClientConfig) -> CoreResult<Self> {
        let error = |message| config_error(&config_path, message);
        if configuration.version != VERSION {
            return Err(error("unsupported configuration version; use version = 1"));
        }
        if configuration.data_dir.as_os_str().is_empty() {
            return Err(error("data_dir must be a nonempty path"));
        }
        let transport = match &configuration.transport {
            Some(settings) => TransportBudget::resolve(settings).map_err(|reason| {
                config_error(
                    &config_path,
                    &format!("invalid transport configuration: {reason}"),
                )
            })?,
            None => TransportBudget::default(),
        };
        let parent = config_path
            .parent()
            .ok_or_else(|| path_error(&config_path))?;
        let root = resolve_path(&parent.join(&configuration.data_dir))?;
        if config_path.starts_with(&root) {
            return Err(error("data_dir must not contain the selected TOML file"));
        }
        let cloud = match configuration.service {
            ClientService::WhaleReinCloud => {
                if configuration.standalone_ca_certificate.is_some() {
                    return Err(error(
                        "standalone_ca_certificate requires standalone service",
                    ));
                }
                if configuration.cloud_ca_certificate.is_some()
                    && configuration.cloud_environment != Some(Environment::Local)
                {
                    return Err(error(
                        "cloud_ca_certificate is only supported for the local Cloud environment",
                    ));
                }
                let name = match configuration.cloud_environment {
                    None | Some(Environment::Production) => "production",
                    Some(Environment::Staging) => "staging",
                    Some(Environment::Local) => "local",
                    _ => {
                        return Err(error(
                            "cloud_environment must be local, staging or production",
                        ))
                    }
                };
                let entries = catalog()?;
                let entry = entries
                    .get(name)
                    .ok_or_else(|| error("official Cloud catalog entry is missing"))?;
                let ca_provider = TlsProvider::parse(&entry.tls.ca_provider)
                    .ok_or_else(|| error("official Cloud catalog trust is invalid"))?;
                Some(EnvironmentResolution {
                    environment: name.into(),
                    selection_source: if configuration.cloud_environment.is_some() {
                        SelectionSource::File
                    } else {
                        SelectionSource::Default
                    },
                    definition_source: DefinitionSource::Builtin,
                    definition: EnvironmentDefinition {
                        id: entry.id.clone(),
                        label: Some(entry.label.clone()),
                        mode: Some(EnvironmentMode::Account),
                        api_origin: Some(entry.api_origin.clone()),
                        issuer: Some(entry.issuer.clone()),
                        native_client_id: Some(entry.native_client_id.clone()),
                        environment: Some(name.into()),
                        // An explicit local CA replaces mkcert discovery so
                        // mobile clients can pin the imported root bundle.
                        tls: Some(match configuration.cloud_ca_certificate.as_deref() {
                            Some(path) => TlsConfig {
                                ca_provider: TlsProvider::File,
                                ca_file: Some(path.to_string_lossy().into_owned()),
                            },
                            None => TlsConfig {
                                ca_provider,
                                ca_file: entry.tls.ca_file.clone(),
                            },
                        }),
                    },
                })
            }
            ClientService::Standalone if configuration.cloud_environment.is_some() => {
                return Err(error("cloud_environment requires whalerein_cloud service"));
            }
            ClientService::Standalone if configuration.cloud_ca_certificate.is_some() => {
                return Err(error(
                    "cloud_ca_certificate requires whalerein_cloud service",
                ));
            }
            ClientService::Standalone => None,
        };
        let ca_pem = if let Some(cloud) = &cloud {
            TrustResolver::new(&config_path)
                .resolve(cloud)
                .map_err(|_| error("official Cloud trust is unavailable; install the required CA"))?
                .ca_pem
        } else if let Some(path) = &configuration.standalone_ca_certificate {
            if path.as_os_str().is_empty() {
                return Err(error("standalone_ca_certificate must be a nonempty path"));
            }
            let ca_path = resolve_path(&parent.join(path))?;
            Some(
                crate::configuration::trust::read_ca(&ca_path).map_err(|_| {
                    error("standalone_ca_certificate must be a readable PEM CA bundle")
                })?,
            )
        } else {
            None
        };
        if let Some(pem) = &ca_pem {
            let mut roots = rustls::RootCertStore::empty();
            for certificate in rustls_pemfile::certs(&mut pem.as_slice()) {
                roots
                    .add(certificate.map_err(|_| error("invalid TLS CA certificate"))?)
                    .map_err(|_| error("invalid TLS CA certificate"))?;
            }
        }
        let instance = Self {
            configuration,
            config_path,
            root,
            cloud,
            ca_pem,
            transport,
        };
        instance.manifest()?;
        Ok(instance)
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join(RUNTIME)
    }

    pub fn manifest(&self) -> CoreResult<Option<Manifest>> {
        let path = self.root.join(MANIFEST);
        let read = || match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => Err(data_error(&self.root)),
            Ok(_) => fs::read(&path)
                .map(Some)
                .map_err(|_| data_error(&self.root)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err(data_error(&self.root)),
        };
        let bytes = if let Some(bytes) = read()? {
            bytes
        } else {
            match fs::read_dir(&self.root) {
                Ok(mut entries) => {
                    if entries.next().is_none() {
                        return Ok(None);
                    }
                }
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                _ => (),
            }
            // A cooperating initializer installs the manifest atomically.
            read()?.ok_or_else(|| data_error(&self.root))?
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| data_error(&self.root))?;
        let environment = self.cloud.as_ref().map(|value| value.environment.as_str());
        let issuer = self
            .cloud
            .as_ref()
            .and_then(|value| value.definition.issuer.as_deref());
        if manifest.version != VERSION
            || manifest.id.len() != 32
            || !manifest.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || manifest.service != self.configuration.service
            || manifest.environment.as_deref() != environment
            || manifest.issuer.as_deref() != issuer
            || manifest.account.as_ref().is_some_and(|account| {
                issuer != Some(account.issuer.as_str())
                    || account.subject.is_empty()
                    || account.workspace.is_empty()
            })
        {
            return Err(data_error(&self.root));
        }
        for entry in fs::read_dir(&self.root).map_err(|_| data_error(&self.root))? {
            let entry = entry.map_err(|_| data_error(&self.root))?;
            let name = entry.file_name();
            let file_type = entry.file_type().map_err(|_| data_error(&self.root))?;
            if !((name == MANIFEST && file_type.is_file())
                || (name == RUNTIME && file_type.is_dir()))
            {
                return Err(data_error(&self.root));
            }
        }
        match fs::symlink_metadata(self.runtime_dir().join("config.json")) {
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            _ => return Err(data_error(&self.root)),
        }
        Ok(Some(manifest))
    }

    pub fn initialize(&self) -> CoreResult<Manifest> {
        if let Some(manifest) = self.manifest()? {
            return Ok(manifest);
        }
        let lock = writer::ConfigLock::acquire(&writer::lock_path(&self.root))?;
        let manifest = if let Some(manifest) = self.manifest()? {
            manifest
        } else {
            create_private_dir(&self.root)?;
            let manifest = Manifest {
                version: VERSION,
                id: crate::util::random_id(16),
                service: self.configuration.service,
                environment: self.cloud.as_ref().map(|value| value.environment.clone()),
                issuer: self
                    .cloud
                    .as_ref()
                    .and_then(|value| value.definition.issuer.clone()),
                account: None,
            };
            self.save_manifest(&manifest)?;
            manifest
        };
        lock.commit();
        Ok(manifest)
    }

    pub fn prepare_runtime(&self) -> CoreResult<()> {
        self.initialize()?;
        create_private_dir(&self.runtime_dir())
    }

    pub fn bind_account(&self, account: &AccountScope) -> CoreResult<()> {
        let mut manifest = self.manifest()?.ok_or_else(|| data_error(&self.root))?;
        if manifest.issuer.as_deref() != Some(account.issuer.as_str())
            || account.subject.is_empty()
            || account.workspace.is_empty()
        {
            return Err(CoreError::Unauthorized);
        }
        if let Some(bound) = manifest.account {
            return if bound == *account {
                Ok(())
            } else {
                Err(CoreError::Conflict(
                    "this instance belongs to another account or workspace; choose a fresh data_dir".into(),
                ))
            };
        }
        manifest.account = Some(account.clone());
        self.save_manifest(&manifest)
    }

    fn save_manifest(&self, manifest: &Manifest) -> CoreResult<()> {
        let bytes = serde_json::to_vec(manifest)
            .map_err(|_| config_error(&self.root, "failed to encode instance state"))?;
        writer::write_with_temporary_parent(
            &self.root.join(MANIFEST),
            &bytes,
            self.root.parent().ok_or_else(|| data_error(&self.root))?,
        )?;
        Ok(())
    }
}

fn create_private_dir(path: &Path) -> CoreResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|_| data_error(path))
}

/// Namespaces existing device/cache/transport roles by the durable instance ID.
pub(crate) struct InstanceSecrets {
    pub instance: Arc<Instance>,
    pub backend: Arc<dyn SecretStore>,
}

impl InstanceSecrets {
    fn key(&self, key: &str) -> Result<Option<String>, SecretError> {
        let Some(manifest) = self.instance.manifest().map_err(|_| SecretError::Failed)? else {
            return Ok(None);
        };
        let role = if key.starts_with("whalerein:v2:identity:") {
            "device-identity"
        } else {
            key
        };
        Ok(Some(format!("whalerein:instance:{}:{role}", manifest.id)))
    }
}

impl SecretStore for InstanceSecrets {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
        match self.key(key)? {
            Some(key) => self.backend.load(&key),
            None => Ok(None),
        }
    }

    fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
        let key = self.key(key)?.ok_or(SecretError::Failed)?;
        self.backend.save(&key, value)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        if let Some(key) = self.key(key)? {
            self.backend.delete(&key)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Core, MemorySecretStore};

    fn file(root: &Path, name: &str, service: ClientService, data: &str) -> PathBuf {
        let path = root.join(name);
        Core::write_config(&path, &ClientConfig::new(service, data)).unwrap();
        path
    }

    #[test]
    fn cli_single_instance_toml_fixture_still_opens_and_validates() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("config.toml");
        fs::write(
            &selected,
            "version = 1\nservice = \"standalone\"\ndata_dir = \"instance\"\n",
        )
        .unwrap();
        let client = Core::open(&selected, Arc::new(MemorySecretStore::new())).unwrap();
        assert_eq!(client.client_config().version, 1);
        assert_eq!(client.client_config().service, ClientService::Standalone);
        assert_eq!(
            client.configured_data_dir(),
            root.path().canonicalize().unwrap().join("instance")
        );

        // The shared validation entry accepts the same fixture without writing
        // or creating the data directory.
        Core::validate_client_config(
            &selected,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&selected).unwrap(),
            "version = 1\nservice = \"standalone\"\ndata_dir = \"instance\"\n"
        );
        assert!(!root.path().join("instance").exists());

        let mut invalid = ClientConfig::new(ClientService::Standalone, "instance");
        invalid.version = 2;
        assert!(Core::validate_client_config(&selected, &invalid).is_err());
        assert!(!root.path().join("instance").exists());
    }

    #[test]
    fn client_instance_open_resolves_relative_data_and_remains_read_only() {
        let root = tempfile::tempdir().unwrap();
        let config = file(
            root.path(),
            "selected.toml",
            ClientService::WhaleReinCloud,
            "one",
        );
        let backend = Arc::new(MemorySecretStore::new());
        let client = Core::open(&config, backend.clone()).unwrap();
        assert_eq!(client.service_label(), "WhaleRein Cloud");
        assert!(fs::read_to_string(&config)
            .unwrap()
            .contains("service = \"whalerein_cloud\""));
        assert_eq!(
            client
                .account_status()
                .unwrap()
                .map(|value| value.account.id),
            None
        );
        assert_eq!(client.instance_id().unwrap(), None);
        assert!(matches!(
            client.cached_sessions("unused"),
            Err(CoreError::NotPaired)
        ));
        assert!(client.pairing_history().unwrap().is_empty());
        assert_eq!(
            client.configured_data_dir(),
            root.path().canonicalize().unwrap().join("one")
        );
        assert!(!client.configured_data_dir().exists());
        assert!(backend.is_empty());

        let alternate = file(
            root.path(),
            "alternate.toml",
            ClientService::WhaleReinCloud,
            "./one",
        );
        let reopened = Core::open(&alternate, backend).unwrap();
        assert_eq!(reopened.configured_data_dir(), client.configured_data_dir());
        assert_eq!(reopened.instance_id().unwrap(), None);
    }

    #[test]
    fn client_instance_serializes_initialization_and_namespaces_credentials() {
        let root = tempfile::tempdir().unwrap();
        let config = file(
            root.path(),
            "config.toml",
            ClientService::Standalone,
            "instance",
        );
        let backend = Arc::new(MemorySecretStore::new());
        let first = Core::open(&config, backend.clone()).unwrap();
        let second = Core::open(&config, backend.clone()).unwrap();

        std::thread::scope(|threads| {
            let first = threads.spawn(|| first.ensure_instance_state());
            second.ensure_instance_state().unwrap();
            first.join().unwrap().unwrap();
        });

        let id = first
            .instance_id()
            .unwrap()
            .expect("stable instance identifier");
        assert_eq!(second.instance_id().unwrap(), Some(id.clone()));
        let third = Core::open(&config, backend.clone()).unwrap();
        assert_eq!(third.instance_id().unwrap(), Some(id));
        let instance = Arc::new(Instance::open(&config).unwrap());
        let namespaced = InstanceSecrets {
            instance,
            backend: backend.clone(),
        };
        backend
            .save("whalerein:v2:identity:old-path", b"fixture")
            .unwrap();
        assert_eq!(
            namespaced.load("whalerein:v2:identity:old-path").unwrap(),
            None
        );
        namespaced
            .save("whalerein:v2:identity:first-path", b"instance fixture")
            .unwrap();
        assert_eq!(
            namespaced
                .load("whalerein:v2:identity:alias-path")
                .unwrap()
                .as_deref(),
            Some(b"instance fixture".as_slice())
        );
        assert_eq!(
            backend
                .load("whalerein:v2:identity:old-path")
                .unwrap()
                .as_deref(),
            Some(b"fixture".as_slice())
        );
        let isolated = file(
            root.path(),
            "isolated.toml",
            ClientService::Standalone,
            "isolated",
        );
        let isolated = Core::open(&isolated, backend.clone()).unwrap();
        isolated.ensure_instance_state().unwrap();
        assert_ne!(
            isolated.instance_id().unwrap(),
            first.instance_id().unwrap()
        );
    }

    #[test]
    fn client_instance_rejects_selected_file_and_legacy_state_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing.toml");
        let backend = Arc::new(MemorySecretStore::new());
        let missing_error = match Core::open(&missing, backend.clone()) {
            Ok(_) => panic!("missing configuration was accepted"),
            Err(error) => error,
        };
        assert!(missing_error.to_string().contains("missing.toml"));
        assert!(!missing.exists());

        let selected = root.path().join("selected.toml");
        let legacy = root.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        let old = legacy.join("config.json");
        fs::write(&old, b"fixture legacy state").unwrap();
        for (source, reason) in [
            ("version = 1\nservice = 'standalone'\n", "invalid version 1 TOML"),
            ("version = 1\nservice = 'standalone'\ndata_dir = 'new'\nenvironment = 'local'\n", "invalid version 1 TOML"),
            ("version = 1\nservice = 'standalone'\ndata_dir = 'new'\ncloud_environment = 'local'\n", "requires whalerein_cloud"),
            ("version = 1\nservice = 'whalerein_cloud'\ndata_dir = 'new'\ncloud_environment = 'custom'\n", "local, staging or production"),
            ("version = 1\nservice = 'standalone'\ndata_dir = 'legacy'\n", "choose a fresh data_dir"),
        ] {
            fs::write(&selected, source).unwrap();
            let error = match Core::open(&selected, backend.clone()) {
                Ok(_) => panic!("invalid configuration was accepted"),
                Err(error) => error,
            };
            assert!(error.to_string().contains(reason), "{error}");
        }
        assert_eq!(fs::read(&old).unwrap(), b"fixture legacy state");
        assert_eq!(fs::read_dir(&legacy).unwrap().count(), 1);
        assert!(!root.path().join("new").exists());
        assert!(backend.is_empty());
    }

    #[test]
    fn local_cloud_instance_accepts_an_explicit_ca_and_rejects_other_services() {
        let root = tempfile::tempdir().unwrap();
        let certificate =
            rcgen::generate_simple_self_signed(vec!["app.whalerein.test".into()]).unwrap();
        fs::write(root.path().join("local-ca.pem"), certificate.cert.pem()).unwrap();
        let path = root.path().join("local.toml");
        let mut config = ClientConfig::new(ClientService::WhaleReinCloud, "instance");
        config.cloud_environment = Some(Environment::Local);
        config.cloud_ca_certificate = Some("local-ca.pem".into());
        Core::write_config(&path, &config).unwrap();
        let instance = Instance::open(&path).unwrap();
        assert!(instance.ca_pem.is_some());

        // The override is restricted to the local environment.
        let mut staging = config.clone();
        staging.cloud_environment = Some(Environment::Staging);
        staging.data_dir = "staging".into();
        assert!(Core::write_config(&root.path().join("staging.toml"), &staging).is_err());

        // Standalone trust uses its own field.
        let mut standalone = ClientConfig::new(ClientService::Standalone, "standalone");
        standalone.cloud_ca_certificate = Some("local-ca.pem".into());
        assert!(Core::write_config(&root.path().join("standalone.toml"), &standalone).is_err());

        // A missing or non-PEM bundle is rejected during open.
        let mut missing = config.clone();
        missing.cloud_ca_certificate = Some("missing-ca.pem".into());
        assert!(Core::write_config(&root.path().join("missing.toml"), &missing).is_err());
    }

    #[test]
    fn client_instance_rejects_authority_or_account_binding_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = file(
            root.path(),
            "config.toml",
            ClientService::WhaleReinCloud,
            "instance",
        );
        let instance = Instance::open(&path).unwrap();
        instance.initialize().unwrap();
        let scope = AccountScope {
            issuer: instance
                .cloud
                .as_ref()
                .unwrap()
                .definition
                .issuer
                .clone()
                .unwrap(),
            subject: "account-a".into(),
            workspace: "workspace-a".into(),
        };
        instance.bind_account(&scope).unwrap();
        instance.bind_account(&scope).unwrap();
        for mismatch in [
            AccountScope {
                issuer: "https://invalid.test/oidc".into(),
                ..scope.clone()
            },
            AccountScope {
                subject: "account-b".into(),
                ..scope.clone()
            },
            AccountScope {
                workspace: "workspace-b".into(),
                ..scope.clone()
            },
        ] {
            assert!(instance.bind_account(&mismatch).is_err());
        }
        let manifest = instance.root.join(MANIFEST);
        let snapshot = fs::read(&manifest).unwrap();
        let alternate = root.path().join("different.toml");
        let mut staging = ClientConfig::new(ClientService::WhaleReinCloud, "instance");
        staging.cloud_environment = Some(Environment::Staging);
        assert!(Core::write_config(&alternate, &staging).is_err());
        assert!(Core::write_config(
            &alternate,
            &ClientConfig::new(ClientService::Standalone, "instance")
        )
        .is_err());
        assert!(!alternate.exists());
        assert_eq!(fs::read(&manifest).unwrap(), snapshot);
    }

    #[test]
    fn client_instance_validates_transport_profiles_before_connection_use() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        let mut config = ClientConfig::new(ClientService::Standalone, "instance");
        config.transport = Some(crate::api::TransportSettings {
            profile: crate::api::TransportProfile::Compact,
            overrides: Some(crate::api::TransportOverrides {
                envelope_bytes: Some(4 * 1024 * 1024),
                transfer_timeout_ms: Some(5_000),
                ..Default::default()
            }),
        });
        Core::write_config(&path, &config).unwrap();
        let core = Core::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        assert_eq!(
            core.client_config().transport.as_ref().unwrap().profile,
            crate::api::TransportProfile::Compact
        );
        assert_eq!(
            core.transport_budget().profile,
            crate::api::TransportProfile::Compact
        );
        // Local overrides are validated and effective for this instance.
        assert_eq!(core.transport_budget().envelope_bytes, 4 * 1024 * 1024);
        assert_eq!(core.transport_budget().transfer_timeout_ms, 5_000);

        // Invalid relations fail during open, before any channel or cache is
        // created, and leave the selected file readable for correction.
        let invalid = root.path().join("invalid.toml");
        fs::write(
            &invalid,
            "version = 1\nservice = 'standalone'\ndata_dir = 'other'\n[transport]\nprofile = 'roomy'\n[transport.overrides]\nreserved_bytes = 1\n",
        )
        .unwrap();
        let error = match Core::open(&invalid, Arc::new(MemorySecretStore::new())) {
            Ok(_) => panic!("invalid transport configuration was accepted"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("invalid transport configuration"));
        assert!(!root.path().join("other").exists());
    }

    #[test]
    fn client_instance_rejects_invalid_tls_partial_manifest_and_old_regional_state() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let file = root.join("client.toml");
        let mut config = ClientConfig::new(ClientService::Standalone, "instance");
        config.standalone_ca_certificate = Some("bad-ca.pem".into());
        let invalid = b"-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        fs::write(root.join("bad-ca.pem"), invalid).unwrap();
        let failure = Core::write_config(&file, &config).unwrap_err();
        assert!(failure.to_string().contains("invalid TLS CA certificate"));
        assert!(!file.exists());
        assert!(!root.join("instance").exists());

        config.standalone_ca_certificate = None;
        Core::write_config(&file, &config).unwrap();
        let backend = Arc::new(MemorySecretStore::new());
        let client = Core::open(&file, backend.clone()).unwrap();
        client.ensure_instance_state().unwrap();
        let state = client.data_dir();
        let config_path = state.join("regional.json");
        fs::write(&config_path, br#"{"schema":"whalerein/v1/config","access_url":"https://fixture.test","host_id":"host","device_id":"device","protocol_version":1}"#).unwrap();
        let error = match Core::open(&file, backend.clone()) {
            Ok(_) => panic!("old regional state was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("choose a fresh data_dir"));
        assert!(error
            .to_string()
            .contains("unsupported regional state schema"));

        fs::remove_file(config_path).unwrap();
        fs::write(
            client.configured_data_dir().join(MANIFEST),
            br#"{"version":1}"#,
        )
        .unwrap();
        assert!(Core::open(&file, backend.clone()).is_err());
        assert!(backend.is_empty());
    }
}
