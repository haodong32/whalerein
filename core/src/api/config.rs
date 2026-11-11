//! Non-secret local configuration.
//!
//! The access URL, host id and device id are opaque routing values. Private
//! keys and device tokens live only in `SecretStore`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{CoreError, CoreResult};

pub const CONFIG_SCHEMA: &str = "whalerein/v1/config";
pub const CONFIG_FILE: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedConfig {
    pub schema: String,
    pub access_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mqtt_url: Option<String>,
    pub host_id: String,
    pub device_id: String,
    pub protocol_version: u32,
}

impl PersistedConfig {
    pub fn new(
        access_url: String,
        mqtt_url: Option<String>,
        host_id: String,
        device_id: String,
    ) -> Self {
        Self {
            schema: CONFIG_SCHEMA.into(),
            access_url,
            host_fingerprint: None,
            mqtt_url,
            host_id,
            device_id,
            protocol_version: whalerein_proto::PROTOCOL_VERSION,
        }
    }

    pub fn validate(&self) -> CoreResult<()> {
        if self.schema != CONFIG_SCHEMA {
            return Err(CoreError::Config("unsupported configuration schema".into()));
        }
        if self
            .host_fingerprint
            .as_ref()
            .is_some_and(|pin| pin.len() != 64 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(CoreError::Config("invalid Host fingerprint".into()));
        }
        if self.host_id.is_empty() || self.device_id.is_empty() {
            return Err(CoreError::Config(
                "configuration is missing identifiers".into(),
            ));
        }
        let url = url::Url::parse(&self.access_url)
            .map_err(|_| CoreError::Config("invalid Access URL".into()))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CoreError::Config("invalid Access URL".into()));
        }
        if let Some(mqtt) = &self.mqtt_url {
            crate::access::MqttSettings::parse(mqtt)
                .map_err(|_| CoreError::Config("invalid MQTT URL".into()))?;
        }
        if self.protocol_version != whalerein_proto::PROTOCOL_VERSION {
            return Err(CoreError::ProtocolIncompatible);
        }
        Ok(())
    }
}

pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(CONFIG_FILE),
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> CoreResult<Option<PersistedConfig>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(CoreError::Config("configuration could not be read".into())),
        };
        let config: PersistedConfig = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Config("configuration is not valid JSON".into()))?;
        config.validate()?;
        Ok(Some(config))
    }

    pub fn save(&self, config: &PersistedConfig) -> CoreResult<()> {
        config.validate()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| CoreError::Config("data directory could not be created".into()))?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|_| CoreError::Config("configuration could not encode".into()))?;
        std::fs::write(&temporary, bytes)
            .map_err(|_| CoreError::Config("configuration could not be written".into()))?;
        set_private_permissions(&temporary)?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|_| CoreError::Config("configuration could not be installed".into()))?;
        Ok(())
    }

    pub fn delete(&self) -> CoreResult<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(CoreError::Config(
                "configuration could not be removed".into(),
            )),
        }
    }
}

pub fn set_private_permissions(path: &Path) -> CoreResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, permissions)
            .map_err(|_| CoreError::Config("file permissions could not be set".into()))?;
    }
    Ok(())
}

pub fn default_data_dir() -> PathBuf {
    if let Ok(value) = std::env::var("WHALEREIN_DATA_DIR") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    directories::ProjectDirs::from("dev", "whalerein", "whalerein")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".whalerein"))
}

pub fn ca_pem_from_env() -> Option<Vec<u8>> {
    let path = std::env::var("WHALEREIN_CA_FILE").ok()?;
    if path.is_empty() {
        return None;
    }
    std::fs::read(path).ok()
}

pub fn mqtt_url_from_env() -> Option<String> {
    std::env::var("WHALEREIN_MQTT_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(_dir: &Path) -> PersistedConfig {
        PersistedConfig::new(
            "https://access.test".into(),
            None,
            "h-1".into(),
            "d-1".into(),
        )
    }

    #[test]
    fn saves_configuration_then_loads_and_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        assert!(store.load().unwrap().is_none());
        store.save(&config(dir.path())).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.host_id, "h-1");
        assert_eq!(loaded.protocol_version, 1);
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
        store.delete().unwrap();
    }

    #[test]
    fn rejects_invalid_configuration() {
        let mut value = config(Path::new("."));
        value.access_url = "ftp://x".into();
        assert!(value.validate().is_err());
        value.access_url = "https://access.test".into();
        value.host_id.clear();
        assert!(value.validate().is_err());
        value.host_id = "h".into();
        value.device_id.clear();
        assert!(matches!(value.validate(), Err(CoreError::Config(_))));
        value.device_id = "d".into();
        value.schema = "other".into();
        assert!(value.validate().is_err());
        value.schema = CONFIG_SCHEMA.into();
        value.mqtt_url = Some("http://x".into());
        assert!(value.validate().is_err());
        value.mqtt_url = Some("mqtts://x".into());
        value.protocol_version = 99;
        assert!(matches!(
            value.validate(),
            Err(CoreError::ProtocolIncompatible)
        ));
    }

    #[test]
    fn pinned_host_fingerprints_require_a_complete_sha256_hex_value() {
        let mut value = config(Path::new("."));
        for pin in ["f".repeat(63), "f".repeat(65), "z".repeat(64)] {
            value.host_fingerprint = Some(pin);
            assert!(matches!(value.validate(), Err(CoreError::Config(_))));
        }
        value.host_fingerprint = Some("AB".repeat(32));
        assert!(value.validate().is_ok());
    }

    #[test]
    fn rejects_non_json_configuration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), b"not json").unwrap();
        let store = ConfigStore::new(dir.path());
        assert!(store.load().is_err());
    }

    #[test]
    fn config_file_permissions_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        store.save(&config(dir.path())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn uses_environment_override_for_default_data_directory() {
        let previous = std::env::var("WHALEREIN_DATA_DIR").ok();
        std::env::set_var("WHALEREIN_DATA_DIR", "/tmp/whalerein-test-dir");
        assert_eq!(default_data_dir(), PathBuf::from("/tmp/whalerein-test-dir"));
        std::env::remove_var("WHALEREIN_DATA_DIR");
        let default = default_data_dir();
        assert!(!default.as_os_str().is_empty());
        if let Some(previous) = previous {
            std::env::set_var("WHALEREIN_DATA_DIR", previous);
        }
    }
}
