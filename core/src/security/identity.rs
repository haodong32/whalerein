//! Device identity keys and their SecretStore lifecycle.

use std::sync::Arc;

use x25519_dalek::{PublicKey, StaticSecret};

#[cfg(test)]
use super::SecretError;
use super::{keys, SecretStore, SecurityError};

/// A long-lived X25519 identity keypair.
pub struct StaticIdentity {
    private_key: [u8; 32],
    public_key: [u8; 32],
}

impl std::fmt::Debug for StaticIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render private key material.
        f.write_str("StaticIdentity(..)")
    }
}

impl StaticIdentity {
    pub fn generate() -> Self {
        let bytes: [u8; 32] = crate::util::random_bytes(32)
            .try_into()
            .expect("32 random bytes");
        Self::from_secret(StaticSecret::from(bytes))
    }

    pub fn from_private_key(private_key: &[u8]) -> Result<Self, SecurityError> {
        let bytes: [u8; 32] = private_key
            .try_into()
            .map_err(|_| SecurityError::InvalidKey)?;
        Ok(Self::from_secret(StaticSecret::from(bytes)))
    }

    fn from_secret(secret: StaticSecret) -> Self {
        let public = PublicKey::from(&secret);
        Self {
            private_key: secret.to_bytes(),
            public_key: public.to_bytes(),
        }
    }

    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    #[cfg(test)]
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key)
    }

    pub fn private_key(&self) -> &[u8; 32] {
        &self.private_key
    }
}

/// Load the device identity for `device_id`, if paired.
pub fn load_device_identity(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
) -> Result<Option<StaticIdentity>, SecurityError> {
    match secrets.load(&keys::device_private_key(device_id))? {
        Some(bytes) => StaticIdentity::from_private_key(&bytes).map(Some),
        None => Ok(None),
    }
}

pub fn store_device_identity(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
    identity: &StaticIdentity,
) -> Result<(), SecurityError> {
    secrets.save(&keys::device_private_key(device_id), identity.private_key())?;
    Ok(())
}

pub fn store_device_token(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
    token: &str,
) -> Result<(), SecurityError> {
    if token.is_empty() {
        return Err(SecurityError::InvalidKey);
    }
    secrets.save(&keys::device_token(device_id), token.as_bytes())?;
    Ok(())
}

pub fn load_device_token(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
) -> Result<Option<String>, SecurityError> {
    match secrets.load(&keys::device_token(device_id))? {
        Some(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| SecurityError::InvalidKey),
        None => Ok(None),
    }
}

/// Remove the private key and transport token for one device.
#[cfg(test)]
pub fn delete_device_material(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
) -> Result<(), SecurityError> {
    secrets.delete(&keys::device_private_key(device_id))?;
    secrets.delete(&keys::device_token(device_id))?;
    Ok(())
}

/// Load or create the random key that encrypts SQLite payload columns.
pub fn load_or_create_cache_key(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
) -> Result<[u8; 32], SecurityError> {
    let key_name = keys::cache_key(device_id);
    if let Some(bytes) = secrets.load(&key_name)? {
        return bytes.try_into().map_err(|_| SecurityError::InvalidKey);
    }
    let key: [u8; 32] = crate::util::random_bytes(32)
        .try_into()
        .map_err(|_| SecurityError::InvalidKey)?;
    secrets.save(&key_name, &key)?;
    Ok(key)
}

/// Load or create a local random salt for salted diagnostic identifiers.
#[cfg(test)]
pub fn load_or_create_diagnostic_salt(
    secrets: &Arc<dyn SecretStore>,
    device_id: &str,
) -> Result<[u8; 32], SecretError> {
    let key_name = keys::diagnostic_salt(device_id);
    if let Some(bytes) = secrets.load(&key_name)? {
        if let Ok(salt) = bytes.try_into() {
            return Ok(salt);
        }
    }
    let salt: [u8; 32] = crate::util::random_bytes(32)
        .try_into()
        .expect("32 random bytes");
    secrets.save(&key_name, &salt)?;
    Ok(salt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::MemorySecretStore;

    #[test]
    fn restores_identity_from_saved_private_key() {
        let identity = StaticIdentity::generate();
        let clone = StaticIdentity::from_private_key(identity.private_key()).unwrap();
        assert_eq!(identity.public_key(), clone.public_key());
        assert_eq!(identity.public_key_hex().len(), 64);
        assert_eq!(format!("{:?}", identity), "StaticIdentity(..)");
    }

    #[test]
    fn rejects_wrong_private_key_length() {
        assert!(matches!(
            StaticIdentity::from_private_key(&[1, 2, 3]),
            Err(SecurityError::InvalidKey)
        ));
    }

    #[test]
    fn stores_and_loads_device_key_and_token() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let identity = StaticIdentity::generate();
        store_device_identity(&secrets, "dev", &identity).unwrap();
        store_device_token(&secrets, "dev", "token-value").unwrap();
        let loaded = load_device_identity(&secrets, "dev").unwrap().unwrap();
        assert_eq!(loaded.public_key(), identity.public_key());
        assert_eq!(
            load_device_token(&secrets, "dev").unwrap().as_deref(),
            Some("token-value")
        );
        delete_device_material(&secrets, "dev").unwrap();
        assert!(load_device_identity(&secrets, "dev").unwrap().is_none());
        assert!(load_device_token(&secrets, "dev").unwrap().is_none());
    }

    #[test]
    fn rejects_empty_token() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        assert!(matches!(
            store_device_token(&secrets, "dev", ""),
            Err(SecurityError::InvalidKey)
        ));
    }

    #[test]
    fn cache_key_is_stable_and_32_bytes() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let first = load_or_create_cache_key(&secrets, "dev").unwrap();
        let second = load_or_create_cache_key(&secrets, "dev").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn invalid_stored_token_is_rejected() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        secrets
            .save(&keys::device_token("dev"), &[0xff, 0xfe])
            .unwrap();
        assert!(matches!(
            load_device_token(&secrets, "dev"),
            Err(SecurityError::InvalidKey)
        ));
    }

    #[test]
    fn diagnostic_salt_is_stable_and_repairs_invalid_values() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let first = load_or_create_diagnostic_salt(&secrets, "dev").unwrap();
        let second = load_or_create_diagnostic_salt(&secrets, "dev").unwrap();
        assert_eq!(first, second);
        secrets
            .save(&keys::diagnostic_salt("other"), &[1, 2])
            .unwrap();
        let repaired = load_or_create_diagnostic_salt(&secrets, "other").unwrap();
        assert_eq!(repaired.len(), 32);
    }

    #[test]
    fn invalid_stored_cache_key_is_rejected() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        secrets.save(&keys::cache_key("dev"), &[1, 2, 3]).unwrap();
        assert!(matches!(
            load_or_create_cache_key(&secrets, "dev"),
            Err(SecurityError::InvalidKey)
        ));
    }
}
