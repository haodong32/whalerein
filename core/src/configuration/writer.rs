//! Durable, serialized configuration replacement.
//!
//! Cooperating Core/Host writers serialize through the shared SQLite sidecar
//! `<config>.lock.sqlite`, table `whalerein_config_lock`. SQLite releases the
//! `BEGIN IMMEDIATE` transaction when the process exits, including on crash.

use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::ConfigurationError;

pub(crate) const LOCK_TABLE: &str = "whalerein_config_lock";

/// Revision marker for one exact file content.
pub(crate) fn content_revision(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub(crate) fn lock_path(config: &Path) -> PathBuf {
    let mut path = config.as_os_str().to_os_string();
    path.push(".lock.sqlite");
    PathBuf::from(path)
}

/// Held SQLite write transaction. Dropping it rolls back and lets the OS
/// release the lock even when the caller never commits.
pub(crate) struct ConfigLock {
    connection: Connection,
    finished: bool,
}

impl ConfigLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, ConfigurationError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| ConfigurationError::Storage)?;
        }
        let connection = Connection::open(path).map_err(|_| ConfigurationError::Storage)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(30))
            .map_err(|_| ConfigurationError::Storage)?;
        connection
            .execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {LOCK_TABLE} (id INTEGER PRIMARY KEY);"
            ))
            .map_err(|_| ConfigurationError::Storage)?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| ConfigurationError::Storage)?;
        Ok(Self {
            connection,
            finished: false,
        })
    }

    pub(crate) fn commit(mut self) {
        let _ = self.connection.execute_batch("COMMIT");
        self.finished = true;
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
    }
}

/// Durable atomic replace: temp in the same directory, fsync file, rename, fsync
/// directory. A failure never removes the previous complete document.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), ConfigurationError> {
    write_with_fault(path, bytes)
}

#[cfg(test)]
pub(crate) static FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
fn fault_at(point: u8) -> bool {
    FAULT.load(std::sync::atomic::Ordering::SeqCst) == point
}

fn write_with_fault(path: &Path, bytes: &[u8]) -> Result<(), ConfigurationError> {
    #[cfg(test)]
    if fault_at(1) {
        return Err(ConfigurationError::Storage);
    }
    let parent = path.parent().ok_or(ConfigurationError::Storage)?;
    std::fs::create_dir_all(parent).map_err(|_| ConfigurationError::Storage)?;
    let name = path.file_name().ok_or(ConfigurationError::Storage)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        name.to_string_lossy(),
        crate::util::random_id(16)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| ConfigurationError::Storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).map_err(
            |_| {
                let _ = std::fs::remove_file(&temporary);
                ConfigurationError::Storage
            },
        )?;
    }
    let result = (|| {
        file.write_all(bytes)
            .map_err(|_| ConfigurationError::Storage)?;
        file.sync_all().map_err(|_| ConfigurationError::Storage)?;
        #[cfg(test)]
        if fault_at(2) {
            return Err(ConfigurationError::Storage);
        }
        drop(file);
        std::fs::rename(&temporary, path).map_err(|_| ConfigurationError::Storage)?;
        #[cfg(test)]
        if fault_at(3) {
            return Err(ConfigurationError::Storage);
        }
        #[cfg(unix)]
        {
            let directory = std::fs::File::open(parent).map_err(|_| ConfigurationError::Storage)?;
            directory
                .sync_all()
                .map_err(|_| ConfigurationError::Storage)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
