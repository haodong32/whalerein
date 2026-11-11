//! The secret-storage boundary and its implementations.
//!
//! Private keys, device transport tokens and cache keys only ever pass through
//! this trait. They never enter configuration files, logs or SQLite.

use std::collections::HashMap;
use std::sync::Mutex;

/// Secret storage boundary. This is the only externally injected capability.
pub trait SecretStore: Send + Sync {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError>;
    fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError>;
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("secret backend unavailable")]
    Unavailable,
    #[error("secret operation failed")]
    Failed,
}

/// Process-local secret store used by tests and by headless test harnesses.
///
/// It is intentionally not selected by default: production builds use the OS
/// keyring. Selecting this store makes secrets disappear when the process exits.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored entries for isolated secret-lifecycle checks.
    pub fn len(&self) -> usize {
        self.values.lock().expect("secret store mutex").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemorySecretStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
        Ok(self
            .values
            .lock()
            .expect("secret store mutex")
            .get(key)
            .cloned())
    }

    fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
        self.values
            .lock()
            .expect("secret store mutex")
            .insert(key.to_owned(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.values.lock().expect("secret store mutex").remove(key);
        Ok(())
    }
}

/// Test-only file-backed secret store.
///
/// It is selected only through the explicit `WHALEREIN_SECRET_BACKEND=file:<dir>`
/// override used by the integration harness. Files are written with 0600
/// permissions, but this store does **not** provide OS keyring protection and
/// must never be the production default.
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    directory: std::path::PathBuf,
}

impl FileSecretStore {
    pub fn new(directory: impl Into<std::path::PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn path(&self, key: &str) -> std::path::PathBuf {
        let encoded: String = key.bytes().map(|byte| format!("{byte:02x}")).collect();
        self.directory.join(encoded)
    }
}

impl SecretStore for FileSecretStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
        match std::fs::read(self.path(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(SecretError::Failed),
        }
    }

    fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
        std::fs::create_dir_all(&self.directory).map_err(|_| SecretError::Unavailable)?;
        let path = self.path(key);
        std::fs::write(&path, value).map_err(|_| SecretError::Failed)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| SecretError::Failed)?;
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match std::fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SecretError::Failed),
        }
    }
}

/// OS keyring implementation. Compiled in with the default `keyring` feature.
#[cfg(feature = "keyring")]
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

#[cfg(feature = "keyring")]
impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, key).map_err(map_keyring_error)
    }
}

#[cfg(feature = "keyring")]
fn map_keyring_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::NoEntry => SecretError::Unavailable,
        keyring::Error::PlatformFailure(_) => SecretError::Unavailable,
        _ => SecretError::Failed,
    }
}

#[cfg(feature = "keyring")]
impl SecretStore for KeyringSecretStore {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
        match self.entry(key)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
        self.entry(key)?
            .set_secret(value)
            .map_err(map_keyring_error)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

/// Secret key names. They contain the opaque device id, never secret material.
pub mod keys {
    pub fn device_private_key(device_id: &str) -> String {
        format!("whalerein:v1:device-key:{device_id}")
    }

    pub fn device_token(device_id: &str) -> String {
        format!("whalerein:v1:device-token:{device_id}")
    }

    pub fn cache_key(device_id: &str) -> String {
        format!("whalerein:v1:cache-key:{device_id}")
    }

    pub fn diagnostic_salt(device_id: &str) -> String {
        format!("whalerein:v1:diagnostic-salt:{device_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn secret_store_implements_object_safe_send_sync_contract() {
        assert_send_sync::<MemorySecretStore>();
        let store: Box<dyn SecretStore> = Box::new(MemorySecretStore::new());
        assert!(store.load("missing").unwrap().is_none());
    }

    #[test]
    fn loads_saved_memory_secret_and_deletes_it() {
        let store = MemorySecretStore::new();
        store.save("a", b"value").unwrap();
        assert_eq!(store.load("a").unwrap().as_deref(), Some(&b"value"[..]));
        assert_eq!(store.len(), 1);
        store.delete("a").unwrap();
        assert!(store.load("a").unwrap().is_none());
        assert!(store.is_empty());
        store.delete("a").unwrap();
    }

    #[test]
    fn loads_saved_file_secret_with_private_permissions_then_deletes_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets"));
        assert!(store.load("k").unwrap().is_none());
        store.save("k", b"value").unwrap();
        assert_eq!(store.load("k").unwrap().as_deref(), Some(&b"value"[..]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = store.path("k");
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        store.delete("k").unwrap();
        store.delete("k").unwrap();
        assert!(store.load("k").unwrap().is_none());
    }

    #[test]
    fn reports_file_secret_save_and_load_failures() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let store = FileSecretStore::new(blocker.join("secrets"));
        assert!(matches!(
            store.save("k", b"v"),
            Err(SecretError::Unavailable)
        ));
        assert!(matches!(store.load("k"), Err(SecretError::Failed)));
    }

    #[test]
    fn secret_key_names_are_derived_from_device_id() {
        assert_eq!(
            keys::device_private_key("dev"),
            "whalerein:v1:device-key:dev"
        );
        assert_eq!(keys::device_token("dev"), "whalerein:v1:device-token:dev");
        assert_eq!(keys::cache_key("dev"), "whalerein:v1:cache-key:dev");
        assert!(keys::diagnostic_salt("dev").contains("diagnostic-salt"));
    }
}

#[cfg(all(test, feature = "keyring", target_os = "macos"))]
mod keyring_manual {
    use super::*;

    #[test]
    #[ignore = "touches the macOS login keychain; run explicitly for SecretStore verification"]
    fn stores_loads_and_deletes_binary_secrets_in_macos_keychain() {
        let service = format!("whalerein-test-{}", crate::util::random_id(8));
        let store = KeyringSecretStore::new(&service);
        let key = "roundtrip";
        assert_eq!(store.load(key).unwrap(), None);
        store.save(key, b"binary\x00secret").unwrap();
        assert_eq!(store.load(key).unwrap(), Some(b"binary\x00secret".to_vec()));
        store.delete(key).unwrap();
        assert!(matches!(
            store.load(key),
            Err(SecretError::Unavailable) | Ok(None)
        ));
    }
}
