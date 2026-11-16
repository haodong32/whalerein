//! The mobile secret-storage boundary.
//!
//! Swift implements [`SecretStore`] with the iOS Keychain. Core keeps the same
//! trait as its only injected capability, so private keys, device transports
//! tokens and cache keys never enter app storage, files or logs. The Rust
//! [`MemorySecretStore`] object exists for isolated Simulator and Rust tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use whalerein_core::{SecretError, SecretStore as CoreSecretStore};

/// Failure returned by a foreign secret store. Unit variants keep the Swift
/// protocol implementation trivial: `throw SecretStoreError.Unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum SecretStoreError {
    #[error("secret backend unavailable")]
    Unavailable,
    #[error("secret operation failed")]
    Failed,
}

/// Unexpected Swift errors (for example a thrown NSError) become a typed
/// `Failed` result instead of propagating across the boundary.
impl From<uniffi::UnexpectedUniFFICallbackError> for SecretStoreError {
    fn from(_: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Failed
    }
}

impl From<SecretStoreError> for SecretError {
    fn from(value: SecretStoreError) -> Self {
        match value {
            SecretStoreError::Unavailable => SecretError::Unavailable,
            SecretStoreError::Failed => SecretError::Failed,
        }
    }
}

/// Foreign-implemented secret storage (Keychain in the app).
///
/// Implementations must be thread-safe and must not log values. The binding
/// never inspects secret bytes.
#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    fn load(&self, key: String) -> Result<Option<Vec<u8>>, SecretStoreError>;
    fn save(&self, key: String, value: Vec<u8>) -> Result<(), SecretStoreError>;
    fn delete(&self, key: String) -> Result<(), SecretStoreError>;
}

/// Adapts the foreign boundary to Core's `SecretStore` trait.
pub(crate) struct SecretStoreAdapter {
    store: Arc<dyn SecretStore>,
}

impl SecretStoreAdapter {
    pub(crate) fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
}

impl CoreSecretStore for SecretStoreAdapter {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
        self.store.load(key.to_owned()).map_err(Into::into)
    }

    fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
        self.store
            .save(key.to_owned(), value.to_vec())
            .map_err(Into::into)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.store.delete(key.to_owned()).map_err(Into::into)
    }
}

/// Process-local secret store for tests and Simulator isolation.
///
/// It is a UniFFI object so Swift tests can either use it directly or wrap it
/// in a `SecretStore` conformance. Values disappear when the process exits and
/// are never written to disk.
#[derive(Debug, Default, uniffi::Object)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

#[uniffi::export]
impl MemorySecretStore {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Number of stored entries, for isolated lifecycle assertions.
    pub fn count(&self) -> u64 {
        Self::len(self) as u64
    }

    pub fn load(&self, key: String) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(self
            .lock()
            .map_err(|_| SecretStoreError::Failed)?
            .get(&key)
            .cloned())
    }

    pub fn save(&self, key: String, value: Vec<u8>) -> Result<(), SecretStoreError> {
        self.lock()?.insert(key, value);
        Ok(())
    }

    pub fn delete(&self, key: String) -> Result<(), SecretStoreError> {
        self.lock()?.remove(&key);
        Ok(())
    }
}

impl MemorySecretStore {
    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>>, SecretStoreError> {
        self.values.lock().map_err(|_| SecretStoreError::Failed)
    }

    fn len(&self) -> usize {
        self.values
            .lock()
            .map(|values| values.len())
            .unwrap_or_default()
    }
}

impl SecretStore for MemorySecretStore {
    fn load(&self, key: String) -> Result<Option<Vec<u8>>, SecretStoreError> {
        MemorySecretStore::load(self, key)
    }

    fn save(&self, key: String, value: Vec<u8>) -> Result<(), SecretStoreError> {
        MemorySecretStore::save(self, key, value)
    }

    fn delete(&self, key: String) -> Result<(), SecretStoreError> {
        MemorySecretStore::delete(self, key)
    }
}

/// A deliberately failing store used to check adapter error mapping.
#[cfg(test)]
pub(crate) struct FailingSecretStore;

#[cfg(test)]
impl SecretStore for FailingSecretStore {
    fn load(&self, _key: String) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }

    fn save(&self, _key: String, _value: Vec<u8>) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Failed)
    }

    fn delete(&self, _key: String) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_round_trips_and_maps_errors() {
        let store = MemorySecretStore::new();
        let adapter = SecretStoreAdapter::new(store.clone());
        assert!(CoreSecretStore::load(&adapter, "k").unwrap().is_none());
        CoreSecretStore::save(&adapter, "k", b"value").unwrap();
        assert_eq!(
            CoreSecretStore::load(&adapter, "k").unwrap().as_deref(),
            Some(&b"value"[..])
        );
        CoreSecretStore::delete(&adapter, "k").unwrap();
        assert_eq!(MemorySecretStore::count(&store), 0);

        let failing = SecretStoreAdapter::new(Arc::new(FailingSecretStore));
        assert!(matches!(
            CoreSecretStore::load(&failing, "k"),
            Err(SecretError::Unavailable)
        ));
        assert!(matches!(
            CoreSecretStore::save(&failing, "k", b"v"),
            Err(SecretError::Failed)
        ));
        assert!(matches!(
            CoreSecretStore::delete(&failing, "k"),
            Err(SecretError::Unavailable)
        ));
    }

    #[test]
    fn rust_memory_object_survives_a_poisoned_lock_as_failed() {
        let store = MemorySecretStore::new();
        let cloned = store.clone();
        let _ = std::thread::spawn(move || {
            let _guard = cloned.values.lock().unwrap();
            panic!("poison the store lock");
        })
        .join();
        assert!(matches!(
            SecretStore::load(&*store, "k".into()),
            Err(SecretStoreError::Failed)
        ));
        assert!(matches!(
            SecretStore::save(&*store, "k".into(), b"v".to_vec()),
            Err(SecretStoreError::Failed)
        ));
        assert!(matches!(
            SecretStore::delete(&*store, "k".into()),
            Err(SecretStoreError::Failed)
        ));
    }

    #[test]
    fn foreign_errors_map_to_core_secret_errors() {
        assert_eq!(
            SecretError::from(SecretStoreError::Unavailable),
            SecretError::Unavailable
        );
        assert_eq!(
            SecretError::from(SecretStoreError::Failed),
            SecretError::Failed
        );
        assert_eq!(
            SecretStoreError::from(uniffi::UnexpectedUniFFICallbackError::new("nserror")),
            SecretStoreError::Failed
        );
    }
}
