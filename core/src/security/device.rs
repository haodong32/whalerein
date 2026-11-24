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

pub(crate) fn existing_device_id(
    secrets: &Arc<dyn SecretStore>,
    profile: &Path,
) -> Result<Option<String>, SecurityError> {
    match secrets.load(&storage_key(profile)?)? {
        Some(bytes) => {
            let stored: StoredIdentity =
                serde_json::from_slice(&bytes).map_err(|_| SecurityError::InvalidKey)?;
            if stored.device_id.is_empty() {
                return Err(SecurityError::InvalidKey);
            }
            Ok(Some(stored.device_id))
        }
        None => Ok(None),
    }
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

/// Replace the keypair while preserving the client id. The caller must have
/// revoked every remote grant before committing the new key.
pub fn replace_key(
    secrets: &Arc<dyn SecretStore>,
    profile: &Path,
    key: &StaticIdentity,
) -> Result<DeviceIdentity, SecurityError> {
    let slot = storage_key(profile)?;
    let bytes = secrets.load(&slot)?.ok_or(SecurityError::InvalidKey)?;
    let stored: StoredIdentity =
        serde_json::from_slice(&bytes).map_err(|_| SecurityError::InvalidKey)?;
    if stored.device_id.is_empty() {
        return Err(SecurityError::InvalidKey);
    }
    let next = StoredIdentity {
        device_id: stored.device_id.clone(),
        private_key: *key.private_key(),
    };
    let encoded = serde_json::to_vec(&next).map_err(|_| SecurityError::InvalidKey)?;
    secrets.save(&slot, &encoded)?;
    store_device_identity(secrets, &next.device_id, key)?;
    Ok(DeviceIdentity {
        device_id: next.device_id,
        key: StaticIdentity::from_private_key(key.private_key())?,
    })
}

pub fn forget(secrets: &Arc<dyn SecretStore>, profile: &Path) -> Result<(), SecurityError> {
    let slot = storage_key(profile)?;
    if let Some(bytes) = secrets.load(&slot)? {
        if let Ok(stored) = serde_json::from_slice::<StoredIdentity>(&bytes) {
            secrets.delete(&keys::device_private_key(&stored.device_id))?;
            secrets.delete(&keys::device_token(&stored.device_id))?;
            secrets.delete(&keys::pending_pairing(&stored.device_id))?;
            secrets.delete(&keys::pairing_history(&stored.device_id))?;
        }
    }
    secrets.delete(&slot)?;
    Ok(())
}

/// OS-released profile lock: crashes cannot leave a stale exclusive lock.
pub struct ProfileLock {
    _file: std::fs::File,
    directory: std::fs::File,
}
pub(crate) fn retryable_admission_error(error: &std::io::Error) -> bool {
    let busy = error.kind() == std::io::ErrorKind::WouldBlock;
    #[cfg(windows)]
    let busy = busy || error.raw_os_error() == Some(32);
    busy
}

/// A bounded wait for a lock that is briefly held by another operation or by a
/// duplicated descriptor in a forked child. `O_CLOEXEC` closes such descriptors
/// on the far side of `exec`, but not during the fork-to-exec window, so a
/// genuinely retryable `WouldBlock` can outlive the original holder.
const LOCK_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);
const LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);

impl ProfileLock {
    /// Test-only direct acquisition; production callers use the bounded-waiting
    /// variants so transient fork-inherited contention is retried.
    #[cfg(test)]
    pub fn acquire(profile: &Path) -> std::io::Result<Self> {
        Self::acquire_file(profile, "identity.lock", false, false)
    }

    pub(crate) fn acquire_admission(profile: &Path) -> std::io::Result<Self> {
        Self::acquire_file(profile, "admission.lock", false, false)
    }

    #[cfg(all(unix, test))]
    pub(crate) fn acquire_admission_shared(profile: &Path) -> std::io::Result<Self> {
        Self::acquire_file(profile, "admission.lock", false, true)
    }

    /// Identity operation guarded by a bounded wait for transient lock
    /// contention. A persistent holder still fails after the budget.
    pub(crate) fn acquire_identity_waiting(profile: &Path) -> std::io::Result<Self> {
        Self::acquire_file_waiting(profile, "identity.lock", false, LOCK_RETRY_BUDGET)
    }

    pub(crate) fn acquire_cloud_pairing(profile: &Path, host: &str) -> std::io::Result<Self> {
        let name = format!(
            "cloud-pairing-{}.lock",
            crate::util::sha256_hex(host.as_bytes())
        );
        Self::acquire_file_waiting(profile, &name, false, LOCK_RETRY_BUDGET)
    }

    /// Admission operation guarded by a bounded wait for transient contention.
    pub(crate) fn acquire_admission_waiting(profile: &Path) -> std::io::Result<Self> {
        Self::acquire_file_waiting(profile, "admission.lock", false, LOCK_RETRY_BUDGET)
    }

    #[cfg(unix)]
    pub(crate) fn acquire_admission_shared_waiting(profile: &Path) -> std::io::Result<Self> {
        Self::acquire_file_waiting(profile, "admission.lock", true, LOCK_RETRY_BUDGET)
    }

    fn acquire_file_waiting(
        profile: &Path,
        name: &str,
        shared: bool,
        budget: std::time::Duration,
    ) -> std::io::Result<Self> {
        let deadline = std::time::Instant::now() + budget;
        loop {
            match Self::acquire_file(profile, name, false, shared) {
                Ok(guard) => return Ok(guard),
                Err(error)
                    if retryable_admission_error(&error)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(LOCK_RETRY_INTERVAL);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) async fn acquire_admission_async(
        profile: &Path,
        deadline: std::time::Instant,
    ) -> std::io::Result<Self> {
        loop {
            match Self::acquire_admission(profile) {
                Ok(guard) => return Ok(guard),
                Err(error)
                    if retryable_admission_error(&error)
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn directory(&self) -> &std::fs::File {
        &self.directory
    }

    fn acquire_file(
        profile: &Path,
        name: &str,
        _blocking: bool,
        _shared: bool,
    ) -> std::io::Result<Self> {
        #[cfg(unix)]
        let (directory, file) = {
            use std::ffi::CString;
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let directory = options.open(profile)?;
            if !directory.metadata()?.is_dir() {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
            let name = CString::new(name)
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK
                        | libc::O_CLOEXEC,
                    0o600u32,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            (directory, unsafe { std::fs::File::from_raw_fd(fd) })
        };
        #[cfg(windows)]
        let (directory, file) = {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            let mut parent = std::fs::OpenOptions::new();
            parent
                .read(true)
                .share_mode(3)
                .custom_flags(0x02000000 | 0x00200000);
            let directory = parent.open(profile)?;
            let parent_metadata = directory.metadata()?;
            if !parent_metadata.is_dir() || parent_metadata.file_attributes() & 0x400 != 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .custom_flags(0x00200000);
            (directory, options.open(profile.join(name))?)
        };
        #[cfg(not(any(unix, windows)))]
        return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
        }
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
            if unsafe {
                flock(
                    file.as_raw_fd(),
                    (if _shared { 1 } else { 2 }) | (if _blocking { 0 } else { 4 }),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self {
            _file: file,
            directory,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn windows_sharing_violation_is_retryable_admission_contention() {
        assert!(super::retryable_admission_error(
            &std::io::Error::from_raw_os_error(32)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_epipe_is_not_retryable_admission_contention() {
        assert!(!super::retryable_admission_error(
            &std::io::Error::from_raw_os_error(32)
        ));
        assert!(super::retryable_admission_error(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
    }

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
    fn rotation_preserves_the_client_id_and_replaces_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let original = load_or_create(&secrets, dir.path(), None).unwrap();
        let next = StaticIdentity::generate();
        let rotated = replace_key(&secrets, dir.path(), &next).unwrap();
        assert_eq!(rotated.device_id, original.device_id);
        assert_eq!(rotated.key.public_key(), next.public_key());
        assert_ne!(rotated.key.public_key(), original.key.public_key());
        let reopened = load_or_create(&secrets, dir.path(), None).unwrap();
        assert_eq!(reopened.device_id, original.device_id);
        assert_eq!(reopened.key.public_key(), next.public_key());
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

    #[test]
    fn bounded_retry_still_reports_a_persistent_holder() {
        let dir = tempfile::tempdir().unwrap();
        let held = ProfileLock::acquire_admission(dir.path()).unwrap();
        let started = std::time::Instant::now();
        let error = match ProfileLock::acquire_file_waiting(
            dir.path(),
            "admission.lock",
            false,
            std::time::Duration::from_millis(30),
        ) {
            Ok(_) => panic!("a held lock must not be acquired"),
            Err(error) => error,
        };
        assert!(retryable_admission_error(&error));
        assert!(started.elapsed() >= std::time::Duration::from_millis(25));
        drop(held);
    }

    /// A fork child inherits a duplicate of the parent's open file description;
    /// `O_CLOEXEC` releases it only at exec, so the parent must tolerate a
    /// bounded WouldBlock after it dropped its own descriptor. `dup(2)` creates
    /// the same shared-description condition deterministically without forking
    /// (a real fork in the parallel test harness would contaminate sibling
    /// tests by inheriting their lock descriptors, which would be its own bug).
    #[cfg(unix)]
    #[test]
    fn duplicate_file_description_holds_admission_until_last_close() {
        use std::os::fd::{AsRawFd, FromRawFd};
        let dir = tempfile::tempdir().unwrap();
        let held = ProfileLock::acquire_admission(dir.path()).unwrap();
        let duplicate = unsafe { libc::dup(held._file.as_raw_fd()) };
        assert!(duplicate >= 0);
        let duplicate = unsafe { std::fs::File::from_raw_fd(duplicate) };
        drop(held);
        let error = match ProfileLock::acquire_admission(dir.path()) {
            Ok(_) => panic!("the duplicate descriptor must retain the lock"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        drop(duplicate);
        let guard = ProfileLock::acquire_admission_waiting(dir.path()).unwrap();
        drop(guard);
    }
}
