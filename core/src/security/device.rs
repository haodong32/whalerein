//! Profile identity survives grants, transport credentials and ordinary config.
use std::path::Path;
use std::sync::Arc;

use super::{
    keys, load_device_identity, store_device_identity, SecretStore, SecurityError, StaticIdentity,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    device_id: String,
    private_key: [u8; 32],
}

pub struct DeviceIdentity {
    pub device_id: String,
    pub key: StaticIdentity,
}

fn storage_key(profile: &Path) -> Result<String, SecurityError> {
    let canonical = std::fs::canonicalize(profile).map_err(|_| SecurityError::InvalidState)?;
    Ok(format!(
        "whalerein:v2:identity:{}",
        crate::util::sha256_hex(canonical.to_string_lossy().as_bytes())
    ))
}

pub fn load_or_create(
    secrets: &Arc<dyn SecretStore>,
    profile: &Path,
    legacy_id: Option<&str>,
) -> Result<DeviceIdentity, SecurityError> {
    let slot = storage_key(profile)?;
    if let Some(bytes) = secrets.load(&slot)? {
        let stored: StoredIdentity =
            serde_json::from_slice(&bytes).map_err(|_| SecurityError::InvalidKey)?;
        if stored.device_id.is_empty() || legacy_id.is_some_and(|id| id != stored.device_id) {
            return Err(SecurityError::InvalidKey);
        }
        let identity = DeviceIdentity {
            device_id: stored.device_id,
            key: StaticIdentity::from_private_key(&stored.private_key)?,
        };
        return Ok(identity);
    }
    let (device_id, key) = match legacy_id {
        Some(id) => (
            id.to_owned(),
            load_device_identity(secrets, id)?.ok_or(SecurityError::InvalidKey)?,
        ),
        None => (crate::util::random_id(16), StaticIdentity::generate()),
    };
    let stored = StoredIdentity {
        device_id: device_id.clone(),
        private_key: *key.private_key(),
    };
    let bytes = serde_json::to_vec(&stored).map_err(|_| SecurityError::InvalidKey)?;
    // This one durable record is committed before any pairing request is sent.
    secrets.save(&slot, &bytes)?;
    store_device_identity(secrets, &device_id, &key)?;
    Ok(DeviceIdentity { device_id, key })
}

pub fn forget(secrets: &Arc<dyn SecretStore>, profile: &Path) -> Result<(), SecurityError> {
    let slot = storage_key(profile)?;
    if let Some(bytes) = secrets.load(&slot)? {
        if let Ok(stored) = serde_json::from_slice::<StoredIdentity>(&bytes) {
            secrets.delete(&keys::device_private_key(&stored.device_id))?;
            secrets.delete(&keys::device_token(&stored.device_id))?;
        }
    }
    secrets.delete(&slot)?;
    Ok(())
}

/// OS-released profile lock: crashes cannot leave a stale exclusive lock.
pub struct ProfileLock {
    _file: std::fs::File,
}
impl ProfileLock {
    pub fn acquire(profile: &Path) -> std::io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(profile.join("identity.lock"))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            extern "C" {
                fn flock(
                    fd: std::os::raw::c_int,
                    operation: std::os::raw::c_int,
                ) -> std::os::raw::c_int;
            }
            // LOCK_EX | LOCK_NB. The file descriptor owns the lock lifetime.
            if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::MemorySecretStore;

    #[test]
    fn identity_reopens_migrates_and_only_changes_on_explicit_forget() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let old = StaticIdentity::generate();
        store_device_identity(&secrets, "existing", &old).unwrap();
        let identity = load_or_create(&secrets, dir.path(), Some("existing")).unwrap();
        assert_eq!(identity.device_id, "existing");
        assert_eq!(identity.key.public_key(), old.public_key());
        let reopened = load_or_create(&secrets, dir.path(), None).unwrap();
        assert_eq!(reopened.device_id, identity.device_id);
        assert!(load_or_create(&secrets, dir.path(), Some("other")).is_err());
        forget(&secrets, dir.path()).unwrap();
        forget(&secrets, dir.path()).unwrap();
        assert_ne!(
            load_or_create(&secrets, dir.path(), None)
                .unwrap()
                .device_id,
            "existing"
        );
    }

    #[test]
    fn corrupt_or_missing_identity_is_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        assert!(load_or_create(&secrets, dir.path(), Some("missing")).is_err());
        let slot = storage_key(dir.path()).unwrap();
        secrets.save(&slot, b"invalid").unwrap();
        assert!(load_or_create(&secrets, dir.path(), None).is_err());
        forget(&secrets, dir.path()).unwrap();
        assert!(load_or_create(&secrets, &dir.path().join("missing"), None).is_err());
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    #[test]
    fn profile_mutations_are_exclusive_and_release_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = ProfileLock::acquire(dir.path()).unwrap();
        assert!(ProfileLock::acquire(dir.path()).is_err());
        drop(first);
        assert!(ProfileLock::acquire(dir.path()).is_ok());
        assert!(ProfileLock::acquire(&dir.path().join("absent")).is_err());
    }
}
