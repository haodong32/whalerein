//! Non-secret local configuration.
//!
//! The access URL, host id and device id are opaque routing values. Private
//! keys and device tokens live only in `SecretStore`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{CoreError, CoreResult};

const REGIONAL_SCHEMA: &str = "whalerein/v1/regional-state";
#[cfg(test)]
pub const CONFIG_SCHEMA: &str = "whalerein/v1/config";
#[cfg(test)]
pub const CONFIG_FILE: &str = "config.json";
const REGIONAL_STATE_FILE: &str = "regional.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersistedConfig {
    pub schema: String,
    pub access_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
    /// Additional Host pins explicitly confirmed during pairing.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub host_fingerprints: std::collections::BTreeMap<String, String>,
    /// Host-assigned opaque grant identity for the current Host. Absent for a
    /// legacy Host that predates Host-owned grants; never derived locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_grant_id: Option<String>,
    /// Additional Host grant identities confirmed during pairing, keyed by
    /// host id. Mirrors `host_fingerprints` for multi-Host profiles.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub host_grant_ids: std::collections::BTreeMap<String, String>,
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
            schema: REGIONAL_SCHEMA.into(),
            access_url,
            host_fingerprint: None,
            host_fingerprints: std::collections::BTreeMap::new(),
            host_grant_id: None,
            host_grant_ids: std::collections::BTreeMap::new(),
            mqtt_url,
            host_id,
            device_id,
            protocol_version: whalerein_proto::PROTOCOL_VERSION,
        }
    }

    /// The Host-assigned grant identity for `host_id`. `None` means no grant
    /// identity is known: a legacy pairing, or one recorded before this field
    /// existed. An empty persisted value is treated as absent.
    pub fn host_grant_id_for(&self, host_id: &str) -> Option<&str> {
        let value = if host_id == self.host_id {
            self.host_grant_id.as_deref()
        } else {
            self.host_grant_ids.get(host_id).map(String::as_str)
        };
        value.filter(|value| !value.is_empty())
    }

    /// Record the Host-assigned grant identity for `host_id`. An empty value is
    /// a legacy Host answer and leaves the stored identity untouched; the grant
    /// is never cleared or invented locally.
    pub fn set_host_grant_id(&mut self, host_id: &str, grant_id: &str) {
        if grant_id.is_empty() {
            return;
        }
        if host_id == self.host_id {
            self.host_grant_id = Some(grant_id.to_owned());
        } else {
            self.host_grant_ids
                .insert(host_id.to_owned(), grant_id.to_owned());
        }
    }

    pub fn validate(&self) -> CoreResult<()> {
        let supported = self.schema == REGIONAL_SCHEMA;
        #[cfg(test)]
        let supported = supported || self.schema == CONFIG_SCHEMA;
        if !supported {
            return Err(CoreError::Config(
                "unsupported regional state schema".into(),
            ));
        }
        if self
            .host_fingerprint
            .as_ref()
            .is_some_and(|pin| pin.len() != 64 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(CoreError::Config("invalid Host fingerprint".into()));
        }
        if self.host_fingerprints.iter().any(|(host, pin)| {
            host.is_empty() || pin.len() != 64 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(CoreError::Config(
                "invalid additional Host fingerprint".into(),
            ));
        }
        if self.host_id.is_empty() || self.device_id.is_empty() {
            return Err(CoreError::Config(
                "configuration is missing identifiers".into(),
            ));
        }
        // A Host-owned grant identity is opaque but bounded. An absent value is
        // the legacy pairing; an empty string is accepted and used as absent.
        if self
            .host_grant_id
            .as_deref()
            .is_some_and(|grant| !crate::pairing::is_valid_host_grant_id(grant))
        {
            return Err(CoreError::Config("invalid Host grant identity".into()));
        }
        if self
            .host_grant_ids
            .iter()
            .any(|(host, grant)| host.is_empty() || !crate::pairing::is_valid_host_grant_id(grant))
        {
            return Err(CoreError::Config(
                "invalid additional Host grant identity".into(),
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
    schema: &'static str,
}

impl ConfigStore {
    #[cfg(test)]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(CONFIG_FILE),
            schema: CONFIG_SCHEMA,
        }
    }

    pub(crate) fn for_instance(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(REGIONAL_STATE_FILE),
            schema: REGIONAL_SCHEMA,
        }
    }

    pub(crate) fn file_name(&self) -> &std::ffi::OsStr {
        self.path
            .file_name()
            .expect("configuration path has a file name")
    }

    #[cfg(all(test, unix))]
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
        if config.schema != self.schema {
            return Err(CoreError::Config(
                "unsupported regional state schema".into(),
            ));
        }
        config.validate()?;
        Ok(Some(config))
    }

    pub fn save(&self, config: &PersistedConfig) -> CoreResult<()> {
        config.validate()?;
        let mut config = config.clone();
        config.schema = self.schema.into();
        let bytes = serde_json::to_vec_pretty(&config)
            .map_err(|_| CoreError::Config("configuration could not encode".into()))?;
        #[cfg(test)]
        if self.schema == CONFIG_SCHEMA {
            return self.save_profile_fixture(&bytes);
        }
        crate::configuration::writer::write_atomically(&self.path, &bytes)?;
        Ok(())
    }

    #[cfg(test)]
    fn save_profile_fixture(&self, bytes: &[u8]) -> CoreResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| CoreError::Config("data directory could not be created".into()))?;
        }
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)
            .map_err(|_| CoreError::Config("configuration could not be written".into()))?;
        set_private_permissions(&temporary)?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|_| CoreError::Config("configuration could not be installed".into()))?;
        Ok(())
    }

    #[cfg(test)]
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

pub fn set_private_permissions(_path: &Path) -> CoreResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(_path, permissions)
            .map_err(|_| CoreError::Config("file permissions could not be set".into()))?;
    }
    Ok(())
}

#[cfg(test)]
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

#[cfg(test)]
pub fn ca_pem_from_env() -> Option<Vec<u8>> {
    let path = std::env::var("WHALEREIN_CA_FILE").ok()?;
    if path.is_empty() {
        return None;
    }
    std::fs::read(path).ok()
}

#[cfg(test)]
pub fn mqtt_url_from_env() -> Option<String> {
    std::env::var("WHALEREIN_MQTT_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Environment variables are process-global; mutate them serially.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

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
    fn host_grant_identity_round_trips_and_legacy_files_stay_loadable() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        let mut value = config(dir.path());
        assert_eq!(value.host_grant_id_for("h-1"), None);
        value.host_grant_id = Some("grant_01HZX:alpha-1".into());
        value
            .host_grant_ids
            .insert("h-2".into(), "grant_second".into());
        store.save(&value).unwrap();
        let loaded = store.load().unwrap().unwrap();
        let mut expected = value.clone();
        expected.schema = CONFIG_SCHEMA.into();
        assert_eq!(loaded, expected);
        assert_eq!(loaded.host_grant_id_for("h-1"), Some("grant_01HZX:alpha-1"));
        assert_eq!(loaded.host_grant_id_for("h-2"), Some("grant_second"));
        assert_eq!(loaded.host_grant_id_for("h-3"), None);

        // A legacy file written before the field existed loads with the grant
        // unset, and saving it again stays byte-compatible (no empty keys).
        let legacy = serde_json::json!({
            "schema": CONFIG_SCHEMA,
            "access_url": "https://access.test/",
            "host_id": "h-1",
            "device_id": "d-1",
            "protocol_version": 1,
        });
        let file = dir.path().join(CONFIG_FILE);
        std::fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.host_grant_id, None);
        assert!(loaded.host_grant_ids.is_empty());
        assert_eq!(loaded.host_grant_id_for("h-1"), None);
        store.save(&loaded).unwrap();
        let encoded = std::fs::read_to_string(&file).unwrap();
        assert!(!encoded.contains("host_grant_id"));
        assert!(!encoded.contains("host_grant_ids"));
    }

    #[test]
    fn an_empty_persisted_grant_identity_is_treated_as_absent() {
        let mut value = config(Path::new("."));
        value.host_grant_id = Some(String::new());
        value.host_grant_ids.insert("h-2".into(), String::new());
        assert!(value.validate().is_ok());
        assert_eq!(value.host_grant_id_for("h-1"), None);
        assert_eq!(value.host_grant_id_for("h-2"), None);
        // Recording an empty Host answer never invents or clears an identity.
        value.set_host_grant_id("h-1", "");
        assert_eq!(value.host_grant_id.as_deref(), Some(""));
        assert_eq!(value.host_grant_id_for("h-1"), None);
        value.set_host_grant_id("h-1", "grant_kept");
        value.set_host_grant_id("h-1", "");
        assert_eq!(value.host_grant_id.as_deref(), Some("grant_kept"));
    }

    #[test]
    fn invalid_host_grant_identity_is_rejected() {
        let mut value = config(Path::new("."));
        for invalid in [
            "a".repeat(129),
            "has space".into(),
            "has\nnewline".into(),
            "has|separator".into(),
            "has/slash".into(),
            "鲸".into(),
        ] {
            value.host_grant_id = Some(invalid.clone());
            assert!(
                matches!(value.validate(), Err(CoreError::Config(_))),
                "expected reject for {invalid:?}"
            );
        }
        value.host_grant_id = None;
        value.host_grant_ids.insert("h-2".into(), "a".repeat(129));
        assert!(matches!(value.validate(), Err(CoreError::Config(_))));
        value.host_grant_ids.insert(String::new(), "grant".into());
        assert!(matches!(value.validate(), Err(CoreError::Config(_))));
    }

    #[test]
    fn unknown_configuration_fields_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let unknown = serde_json::json!({
            "schema": CONFIG_SCHEMA,
            "access_url": "https://access.test/",
            "host_id": "h-1",
            "device_id": "d-1",
            "protocol_version": 1,
            "unexpected": true,
        });
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            serde_json::to_vec(&unknown).unwrap(),
        )
        .unwrap();
        let store = ConfigStore::new(dir.path());
        assert!(store.load().is_err());
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
    fn data_directory_environment_edge_cases_keep_the_documented_precedence() {
        let _guard = serial();
        let previous = std::env::var("WHALEREIN_DATA_DIR").ok();
        // An empty value means "unset", not "current directory".
        std::env::set_var("WHALEREIN_DATA_DIR", "");
        let default = default_data_dir();
        assert!(!default.as_os_str().is_empty());
        assert_ne!(default, PathBuf::from(""));
        // A relative override is preserved verbatim; the CLI resolves nothing.
        std::env::set_var("WHALEREIN_DATA_DIR", "relative/profile");
        assert_eq!(default_data_dir(), PathBuf::from("relative/profile"));
        // A path with a trailing separator is kept as written.
        std::env::set_var("WHALEREIN_DATA_DIR", "/tmp/profile/");
        assert_eq!(default_data_dir(), PathBuf::from("/tmp/profile/"));
        match previous {
            Some(previous) => std::env::set_var("WHALEREIN_DATA_DIR", previous),
            None => std::env::remove_var("WHALEREIN_DATA_DIR"),
        }
    }

    #[test]
    fn a_missing_or_unreadable_ca_file_is_reported_by_the_caller() {
        let _guard = serial();
        let previous = std::env::var("WHALEREIN_CA_FILE").ok();
        std::env::set_var("WHALEREIN_CA_FILE", "/definitely/not/a/ca.pem");
        // Core's accessor stays an Option so startup validation owns the error;
        // the CLI rejects it as invalid_configuration before any command runs.
        assert!(ca_pem_from_env().is_none());
        std::env::set_var("WHALEREIN_CA_FILE", "");
        assert!(ca_pem_from_env().is_none());
        match previous {
            Some(previous) => std::env::set_var("WHALEREIN_CA_FILE", previous),
            None => std::env::remove_var("WHALEREIN_CA_FILE"),
        }
    }

    #[test]
    fn uses_environment_override_for_default_data_directory() {
        let _guard = serial();
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
