//! SQLite cache.
//!
//! Only `sync` writes session/event/cursor rows. Only `commands` writes the
//! command projection. Sensitive payload columns are encrypted with a random
//! cache key held by `SecretStore`; the key never enters the database.

mod conversation;

use std::path::Path;
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use rusqlite::{params, Connection, OptionalExtension};

use crate::api::{HostInfo, OnlineStatus, PendingCommand, SessionSummary};

pub const CACHE_NONCE_LEN: usize = 12;
pub const SCHEMA_VERSION: i64 = 2;
const HISTORY_CACHE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache database error")]
    Database,
    #[error("cache encryption failed")]
    Encryption,
    #[error("cache key is invalid")]
    InvalidKey,
    #[error("history cursor does not continue the applied view")]
    HistoryGap,
    #[error("conversation history exceeds the cache budget")]
    HistoryBudget,
    #[error("cache payload is corrupt")]
    Corrupt,
    #[error("cache database is missing")]
    Missing,
    #[error("cache database is temporarily busy")]
    Busy,
}

impl From<rusqlite::Error> for CacheError {
    fn from(_: rusqlite::Error) -> Self {
        CacheError::Database
    }
}

pub(crate) struct CommandIntent<'a> {
    pub request_id: &'a str,
    pub command_id: &'a str,
    pub host_id: &'a str,
    pub device_id: &'a str,
    pub access_url: &'a str,
    pub host_fingerprint: &'a str,
    pub command_kind: &'a str,
    pub command_parent_session_id: &'a str,
}

/// Holds SQLite's transaction until revocation has removed the cache.
#[cfg(unix)]
pub(crate) struct LockedPendingSnapshot<'a> {
    pub pending: Vec<PendingCommand>,
    conn: std::sync::MutexGuard<'a, Connection>,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn profile_entry_stat(directory: &std::fs::File, name: &str) -> std::io::Result<libc::stat> {
    use std::os::fd::AsRawFd;
    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(unix)]
fn sqlite_has_moved(conn: &Connection) -> Result<bool, CacheError> {
    let mut moved = 0i32;
    let status = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            std::ptr::null(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut i32).cast(),
        )
    };
    if status != rusqlite::ffi::SQLITE_OK {
        return Err(CacheError::Database);
    }
    Ok(moved != 0)
}

#[cfg(unix)]
impl LockedPendingSnapshot<'_> {
    #[allow(clippy::unnecessary_cast)] // libc stat field widths differ by platform.
    pub(crate) fn matches_name(&self, directory: &std::fs::File) -> bool {
        match profile_entry_stat(directory, "cache.sqlite3") {
            Ok(actual) => {
                (actual.st_mode & libc::S_IFMT) == libc::S_IFREG
                    && actual.st_nlink == 1
                    && self.device == actual.st_dev as u64
                    && self.inode == actual.st_ino as u64
                    && matches!(sqlite_has_moved(&self.conn), Ok(false))
            }
            Err(_) => false,
        }
    }
    pub(crate) fn removed_from_profile(&self, directory: &std::fs::File) -> bool {
        matches!(profile_entry_stat(directory,"cache.sqlite3"),Err(error) if error.kind()==std::io::ErrorKind::NotFound)
            && matches!(sqlite_has_moved(&self.conn), Ok(true))
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct CacheProvenance {
    profile_device: u64,
    profile_inode: u64,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl Drop for LockedPendingSnapshot<'_> {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

pub struct Cache {
    conn: Mutex<Connection>,
    #[cfg(unix)]
    provenance: Option<CacheProvenance>,
    #[cfg(unix)]
    revocation_only: bool,
    key: [u8; 32],
    history_budget_bytes: u64,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cache(..)")
    }
}

impl Cache {
    #[cfg(all(test, unix))]
    pub(crate) fn sqlite_connection_identity(&self) -> usize {
        let conn = self.conn.lock().expect("cache lock");
        unsafe { conn.handle() as usize }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn has_revocation_provenance(&self) -> bool {
        self.provenance.is_some()
    }

    #[cfg(unix)]
    pub(crate) fn is_revocation_only(&self) -> bool {
        self.revocation_only
    }

    pub(crate) fn has_key(&self, key: &[u8]) -> bool {
        self.key == key
    }

    pub fn open(path: &Path, key: [u8; 32]) -> Result<Self, CacheError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| CacheError::Database)?;
        }
        let conn = Connection::open(path)?;
        // A revoked profile may have been replaced after this connection was
        // opened. Closing it must not checkpoint a potentially foreign WAL.
        conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )?;
        #[allow(unused_mut)] // Unix records provenance after initialization.
        let mut cache = Self {
            conn: Mutex::new(conn),
            #[cfg(unix)]
            provenance: None,
            #[cfg(unix)]
            revocation_only: false,
            key,
            history_budget_bytes: HISTORY_CACHE_BYTES,
        };
        cache.initialize_schema()?;
        #[cfg(unix)]
        {
            cache.provenance = cache.capture_provenance(path);
        }
        Ok(cache)
    }

    pub fn open_in_memory(key: [u8; 32]) -> Result<Self, CacheError> {
        let cache = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
            #[cfg(unix)]
            provenance: None,
            #[cfg(unix)]
            revocation_only: false,
            key,
            history_budget_bytes: HISTORY_CACHE_BYTES,
        };
        cache.initialize_schema()?;
        Ok(cache)
    }

    fn initialize_schema(&self) -> Result<(), CacheError> {
        {
            let conn = self.conn.lock().expect("cache mutex");
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            conn.execute_batch(
                "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; BEGIN IMMEDIATE;",
            )?;
        }
        // One SQLite write transaction covers both schema inspection and ALTERs.
        // Separate Core instances must not race after observing the same missing column.
        let result = self
            .initialize()
            .and_then(|_| self.initialize_conversations())
            .and_then(|_| self.trim_history_cache(None));
        let conn = self.conn.lock().expect("cache mutex");
        if result.is_ok() {
            conn.execute_batch("COMMIT")?;
        } else {
            let _ = conn.execute_batch("ROLLBACK");
        }
        result
    }

    fn initialize(&self) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)",
        )?;
        let previous: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(version) = previous.as_deref() {
            match version {
                "1" => conn
                    .execute_batch("DROP TABLE IF EXISTS events; DROP TABLE IF EXISTS sessions;")?,
                "2" => {}
                _ => return Err(CacheError::Corrupt),
            }
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS hosts (
                 host_id TEXT PRIMARY KEY,
                 protocol_version INTEGER NOT NULL,
                 fingerprint TEXT NOT NULL,
                 updated_at_unix_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 host_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 summary BLOB NOT NULL,
                 complete INTEGER NOT NULL,
                 source TEXT NOT NULL,
                 updated_at_unix_ms INTEGER NOT NULL,
                 PRIMARY KEY(host_id,session_id)
             );
             CREATE TABLE IF NOT EXISTS commands (
                 request_id TEXT PRIMARY KEY,
                 command_id TEXT NOT NULL,
                 status TEXT NOT NULL,
                 error_code TEXT,
                 updated_at_unix_ms INTEGER NOT NULL
             );",
        )?;
        // Add recovery routing without rebuilding the durable command journal.
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(commands)")?
            .query_map([], |row| row.get(1))?
            .collect::<Result<_, _>>()?;
        for column in [
            "host_id",
            "device_id",
            "access_url",
            "host_fingerprint",
            "command_kind",
            "command_parent_session_id",
        ] {
            if !columns.iter().any(|name| name == column) {
                conn.execute_batch(&format!(
                    "ALTER TABLE commands ADD COLUMN {column} TEXT NOT NULL DEFAULT '';"
                ))?;
            }
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CacheError> {
        let cipher =
            ChaCha20Poly1305::new_from_slice(&self.key).map_err(|_| CacheError::InvalidKey)?;
        let nonce_bytes = crate::util::random_bytes(CACHE_NONCE_LEN);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CacheError::Encryption)?;
        let mut sealed = Vec::with_capacity(CACHE_NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    fn open_blob(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, CacheError> {
        if blob.len() <= CACHE_NONCE_LEN {
            return Err(CacheError::Corrupt);
        }
        let cipher =
            ChaCha20Poly1305::new_from_slice(&self.key).map_err(|_| CacheError::InvalidKey)?;
        let (nonce, ciphertext) = blob.split_at(CACHE_NONCE_LEN);
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| CacheError::Corrupt)
    }

    pub fn put_hosts(&self, hosts: &[HostInfo]) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().expect("cache mutex");
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM hosts", [])?;
        for host in hosts {
            // `online` is connection state and lives only in runtime memory.
            tx.execute(
                "INSERT INTO hosts(host_id, protocol_version, fingerprint, updated_at_unix_ms)
                 VALUES(?1, ?2, ?3, ?4)",
                params![
                    host.host_id,
                    host.protocol_version,
                    host.fingerprint,
                    crate::util::now_unix_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_hosts(&self) -> Result<Vec<HostInfo>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let mut statement = conn
            .prepare("SELECT host_id, protocol_version, fingerprint FROM hosts ORDER BY host_id")?;
        let rows = statement.query_map([], |row| {
            Ok(HostInfo {
                host_id: row.get(0)?,
                // Online is deliberately not cached; callers must ask Access.
                online: false,
                protocol_version: row.get(1)?,
                fingerprint: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Upsert session summaries produced by a completed host query.
    /// Refresh remote summaries without advancing the locally applied cursor.
    pub fn put_sessions(
        &self,
        host_id: &str,
        sessions: &[SessionSummary],
        source: &str,
        complete: bool,
    ) -> Result<(), CacheError> {
        let sealed = sessions
            .iter()
            .map(|session| {
                let key = whalerein_proto::SessionKey {
                    host_id: host_id.into(),
                    session_id: session.session_id.clone(),
                };
                let bytes = serde_json::to_vec(session).map_err(|_| CacheError::Corrupt)?;
                Ok((
                    session.session_id.clone(),
                    self.seal(&session_aad(&key), &bytes)?,
                ))
            })
            .collect::<Result<Vec<_>, CacheError>>()?;
        let mut conn = self.conn.lock().expect("cache mutex");
        let tx = conn.transaction()?;
        for (id, body) in sealed {
            tx.execute("INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(host_id,session_id) DO UPDATE SET summary=excluded.summary,complete=excluded.complete,source=excluded.source,updated_at_unix_ms=excluded.updated_at_unix_ms",params![host_id,id,body,complete as i64,source,crate::util::now_unix_ms()])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn load_sessions(&self, host_id: &str) -> Result<Vec<SessionSummary>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let mut statement = conn.prepare(
            "SELECT session_id,summary FROM sessions WHERE host_id=?1 ORDER BY session_id",
        )?;
        let rows = statement.query_map(params![host_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            let (id, body) = row?;
            let key = whalerein_proto::SessionKey {
                host_id: host_id.into(),
                session_id: id,
            };
            let bytes = self.open_blob(&session_aad(&key), &body)?;
            sessions.push(serde_json::from_slice(&bytes).map_err(|_| CacheError::Corrupt)?);
        }
        Ok(sessions)
    }
    pub fn get_session(
        &self,
        key: &whalerein_proto::SessionKey,
    ) -> Result<Option<SessionSummary>, CacheError> {
        let body: Option<Vec<u8>> = self
            .conn
            .lock()
            .expect("cache mutex")
            .query_row(
                "SELECT summary FROM sessions WHERE host_id=?1 AND session_id=?2",
                params![key.host_id, key.session_id],
                |row| row.get(0),
            )
            .optional()?;
        body.map(|blob| {
            let bytes = self.open_blob(&session_aad(key), &blob)?;
            serde_json::from_slice(&bytes).map_err(|_| CacheError::Corrupt)
        })
        .transpose()
    }

    #[cfg(test)]
    pub(crate) fn record_command_intent(
        &self,
        request_id: &str,
        command_id: &str,
        host_id: &str,
        device_id: &str,
        access_url: &str,
        host_fingerprint: &str,
    ) -> Result<(), CacheError> {
        self.record_typed_command_intent(CommandIntent {
            request_id,
            command_id,
            host_id,
            device_id,
            access_url,
            host_fingerprint,
            command_kind: "",
            command_parent_session_id: "",
        })
    }

    pub(crate) fn record_typed_command_intent(
        &self,
        intent: CommandIntent<'_>,
    ) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute("INSERT INTO commands(request_id, command_id, host_id, device_id, access_url, host_fingerprint, command_kind, command_parent_session_id, status, updated_at_unix_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'unknown', ?9)", params![intent.request_id, intent.command_id, intent.host_id, intent.device_id, intent.access_url, intent.host_fingerprint, intent.command_kind, intent.command_parent_session_id, crate::util::now_unix_ms()])?;
        Ok(())
    }

    pub(crate) fn command_identity(
        &self,
        request_id: &str,
    ) -> Result<Option<(String, String)>, CacheError> {
        self.conn
            .lock()
            .expect("cache mutex")
            .query_row(
                "SELECT command_kind, command_parent_session_id FROM commands WHERE request_id=?1",
                [request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn remove_unsent_command(&self, request_id: &str) -> Result<(), CacheError> {
        self.conn.lock().expect("cache mutex").execute(
            "DELETE FROM commands WHERE request_id=?1 AND status='unknown'",
            [request_id],
        )?;
        Ok(())
    }

    pub(crate) fn pending_commands(&self) -> Result<Vec<PendingCommand>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        Self::read_pending_commands(&conn)
    }

    fn read_pending_commands(conn: &Connection) -> Result<Vec<PendingCommand>, CacheError> {
        let mut statement = conn.prepare("SELECT request_id, command_id, host_id, device_id, status, access_url, host_fingerprint FROM commands WHERE status IN ('unknown', 'received', 'host_accepted') ORDER BY updated_at_unix_ms, request_id")?;
        let rows = statement
            .query_map([], |row| {
                let status: String = row.get(4)?;
                Ok(PendingCommand {
                    access_url: row.get(5)?,
                    host_fingerprint: row.get(6)?,
                    request_id: row.get(0)?,
                    command_id: row.get(1)?,
                    host_id: row.get(2)?,
                    device_id: row.get(3)?,
                    status: match status.as_str() {
                        "received" => OnlineStatus::Received,
                        "host_accepted" => OnlineStatus::HostAccepted,
                        _ => OnlineStatus::Unknown,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)]
    fn capture_provenance(&self, path: &Path) -> Option<CacheProvenance> {
        use std::os::unix::fs::MetadataExt;
        let parent = std::fs::symlink_metadata(path.parent()?).ok()?;
        let db = std::fs::symlink_metadata(path).ok()?;
        if !parent.is_dir()
            || parent.mode() & 0o022 != 0
            || parent.uid() != unsafe { libc::geteuid() }
            || !db.is_file()
            || db.nlink() != 1
        {
            return None;
        }
        let conn = self.conn.try_lock().ok()?;
        if !matches!(sqlite_has_moved(&conn), Ok(false)) {
            return None;
        }
        Some(CacheProvenance {
            profile_device: parent.dev(),
            profile_inode: parent.ino(),
            device: db.dev(),
            inode: db.ino(),
        })
    }

    /// Initialize a Cache around an already existing database without creating
    /// or migrating it; business rows are not read until provenance is checked.
    #[cfg(unix)]
    pub(crate) fn open_existing_verified(
        path: &Path,
        key: [u8; 32],
        directory: &std::fs::File,
    ) -> Result<Self, CacheError> {
        use std::os::unix::fs::MetadataExt;
        let parent = std::fs::symlink_metadata(path.parent().ok_or(CacheError::Database)?)
            .map_err(|_| CacheError::Database)?;
        let held = directory.metadata().map_err(|_| CacheError::Database)?;
        if !parent.is_dir()
            || parent.mode() & 0o022 != 0
            || parent.uid() != unsafe { libc::geteuid() }
            || parent.dev() != held.dev()
            || parent.ino() != held.ino()
        {
            return Err(CacheError::Corrupt);
        }
        let initial = profile_entry_stat(directory, "cache.sqlite3").map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CacheError::Missing
            } else {
                CacheError::Database
            }
        })?;
        if (initial.st_mode & libc::S_IFMT) != libc::S_IFREG || initial.st_nlink != 1 {
            return Err(CacheError::Corrupt);
        }
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        let mut cache = Self {
            conn: Mutex::new(conn),
            provenance: None,
            revocation_only: true,
            key,
            history_budget_bytes: HISTORY_CACHE_BYTES,
        };
        let provenance = cache.capture_provenance(path).ok_or(CacheError::Corrupt)?;
        #[allow(clippy::unnecessary_cast)]
        if provenance.profile_device != held.dev()
            || provenance.profile_inode != held.ino()
            || provenance.device != initial.st_dev as u64
            || provenance.inode != initial.st_ino as u64
        {
            return Err(CacheError::Corrupt);
        }
        cache.provenance = Some(provenance);
        Ok(cache)
    }

    /// Snapshot through this Cache's existing SQLite connection. The caller
    /// holds the profile guard and keeps this transaction until cleanup ends.
    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)]
    pub(crate) fn pending_commands_from_existing<'a>(
        &'a self,
        directory: &std::fs::File,
        profile: &Path,
    ) -> Result<LockedPendingSnapshot<'a>, CacheError> {
        use std::os::unix::fs::MetadataExt;
        fn no_sidecars(directory: &std::fs::File) -> Result<(), CacheError> {
            for name in [
                "cache.sqlite3-journal",
                "cache.sqlite3-wal",
                "cache.sqlite3-shm",
            ] {
                match profile_entry_stat(directory, name) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) if name == "cache.sqlite3-journal" => return Err(CacheError::Busy),
                    _ => return Err(CacheError::Corrupt),
                }
            }
            Ok(())
        }
        let provenance = self.provenance.ok_or(CacheError::Corrupt)?;
        let held = directory.metadata().map_err(|_| CacheError::Database)?;
        let current = std::fs::symlink_metadata(profile).map_err(|_| CacheError::Database)?;
        if !current.is_dir()
            || current.mode() & 0o022 != 0
            || current.uid() != unsafe { libc::geteuid() }
            || held.dev() != provenance.profile_device
            || held.ino() != provenance.profile_inode
            || current.dev() != provenance.profile_device
            || current.ino() != provenance.profile_inode
        {
            return Err(CacheError::Corrupt);
        }
        no_sidecars(directory)?;
        let conn = self.conn.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => CacheError::Busy,
            std::sync::TryLockError::Poisoned(_) => CacheError::Database,
        })?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| match error.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    CacheError::Busy
                }
                _ => CacheError::Database,
            })?;
        let mut snapshot = LockedPendingSnapshot {
            pending: Vec::new(),
            conn,
            device: provenance.device,
            inode: provenance.inode,
        };
        if !snapshot.matches_name(directory) {
            return Err(CacheError::Corrupt);
        }
        no_sidecars(directory)?;
        let mode: String = snapshot
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(CacheError::Corrupt);
        }
        snapshot.pending = Self::read_pending_commands(&snapshot.conn)?;
        no_sidecars(directory)?;
        let current = std::fs::symlink_metadata(profile).map_err(|_| CacheError::Database)?;
        if current.dev() != provenance.profile_device
            || current.ino() != provenance.profile_inode
            || !snapshot.matches_name(directory)
        {
            return Err(CacheError::Corrupt);
        }
        Ok(snapshot)
    }

    pub fn put_command(
        &self,
        request_id: &str,
        command_id: &str,
        status: OnlineStatus,
        error_code: Option<&str>,
    ) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute(
            "INSERT INTO commands(request_id, command_id, status, error_code, updated_at_unix_ms)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(request_id) DO UPDATE SET
                 status = excluded.status,
                 error_code = excluded.error_code,
                 updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![
                request_id,
                command_id,
                status.as_str(),
                error_code,
                crate::util::now_unix_ms()
            ],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn get_command(
        &self,
        request_id: &str,
    ) -> Result<Option<(String, String, Option<String>)>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.query_row(
            "SELECT command_id, status, error_code FROM commands WHERE request_id = ?1",
            params![request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Non-terminal and unknown command counts used by doctor.
    pub fn command_counts(&self) -> Result<(u64, u64), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let non_terminal: i64 = conn.query_row(
            "SELECT COUNT(*) FROM commands WHERE status IN ('received', 'host_accepted')",
            [],
            |row| row.get(0),
        )?;
        let unknown: i64 = conn.query_row(
            "SELECT COUNT(*) FROM commands WHERE status = 'unknown'",
            [],
            |row| row.get(0),
        )?;
        Ok((non_terminal as u64, unknown as u64))
    }

    #[cfg(any(not(unix), test))]
    pub fn clear(&self) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute_batch(
            "DELETE FROM sessions; DELETE FROM commands; DELETE FROM hosts; DELETE FROM history_receipts; DELETE FROM conversation_views; DELETE FROM conversation_messages; DELETE FROM conversation_stages; DELETE FROM conversation_stage_messages;",
        )?;
        Ok(())
    }
}

fn session_aad(key: &whalerein_proto::SessionKey) -> Vec<u8> {
    serde_json::to_vec(&["session", key.host_id.as_str(), key.session_id.as_str()])
        .expect("string tuple")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn summary(title: &str) -> SessionSummary {
        SessionSummary::from(whalerein_proto::SessionSummary {
            session_id: "same".into(),
            title: title.into(),
            ..Default::default()
        })
    }
    #[test]
    fn session_keys_and_encryption_bind_host_and_session_identity() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        cache
            .put_sessions("a", &[summary("x")], "source", true)
            .unwrap();
        cache
            .put_sessions("b", &[summary("y")], "source", true)
            .unwrap();
        let a = whalerein_proto::SessionKey {
            host_id: "a".into(),
            session_id: "same".into(),
        };
        let b = whalerein_proto::SessionKey {
            host_id: "b".into(),
            session_id: "same".into(),
        };
        assert_ne!(
            cache.get_session(&a).unwrap().unwrap().title,
            cache.get_session(&b).unwrap().unwrap().title
        );
        assert_eq!(cache.load_sessions("a").unwrap().len(), 1);
        cache.conn.lock().unwrap().execute("UPDATE sessions SET summary=(SELECT summary FROM sessions WHERE host_id='a') WHERE host_id='b'",[]).unwrap();
        assert!(matches!(cache.get_session(&b), Err(CacheError::Corrupt)));
    }
    #[test]
    fn host_presence_is_not_restored_and_command_projection_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = Cache::open(&path, [1; 32]).unwrap();
        cache
            .put_hosts(&[HostInfo {
                host_id: "a".into(),
                online: true,
                protocol_version: 1,
                fingerprint: "fixture".into(),
            }])
            .unwrap();
        assert!(!cache.load_hosts().unwrap()[0].online);
        cache
            .put_command("r", "c", OnlineStatus::Unknown, Some("unavailable"))
            .unwrap();
        assert_eq!(cache.command_counts().unwrap(), (0, 1));
        drop(cache);
        let cache = Cache::open(&path, [1; 32]).unwrap();
        assert!(cache.get_command("r").unwrap().is_some());
        cache.clear().unwrap();
        assert!(cache.get_command("r").unwrap().is_none());
        assert!(cache.load_hosts().unwrap().is_empty());
    }
    #[test]
    fn cache_rejects_tampered_ciphertext_and_future_schemas() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        assert!(cache.open_blob(b"a", &[0; 2]).is_err());
        let sealed = cache.seal(b"a", b"x").unwrap();
        assert!(cache.open_blob(b"b", &sealed).is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let cache = Cache::open(&path, [1; 32]).unwrap();
        cache
            .conn
            .lock()
            .unwrap()
            .execute("UPDATE meta SET value='999' WHERE key='schema_version'", [])
            .unwrap();
        drop(cache);
        assert!(Cache::open(&path, [1; 32]).is_err());
    }
    #[test]
    fn recovery_routing_migration_preserves_legacy_rows_and_new_intents_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO meta VALUES('schema_version','2'); CREATE TABLE commands(request_id TEXT PRIMARY KEY, command_id TEXT NOT NULL, status TEXT NOT NULL, error_code TEXT, updated_at_unix_ms INTEGER NOT NULL); INSERT INTO commands VALUES('legacy','old-command','unknown',NULL,1);").unwrap();
        drop(conn);
        let cache = Cache::open(&path, [1; 32]).unwrap();
        assert_eq!(
            cache.get_command("legacy").unwrap().unwrap().0,
            "old-command"
        );
        cache
            .record_command_intent("r", "c", "h", "d", "https://access.test/", "")
            .unwrap();
        cache
            .put_command("r", "c", OnlineStatus::HostAccepted, None)
            .unwrap();
        let before = cache.pending_commands().unwrap();
        assert_eq!(before.len(), 2);
        let current = before
            .iter()
            .find(|command| command.request_id == "r")
            .unwrap();
        assert_eq!(current.host_id, "h");
        assert_eq!(current.device_id, "d");
        assert_eq!(current.status, OnlineStatus::HostAccepted);
        drop(cache);
        assert_eq!(
            Cache::open(&path, [1; 32])
                .unwrap()
                .pending_commands()
                .unwrap(),
            before
        );
    }

    #[test]
    fn replacing_the_development_cache_preserves_command_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let cache = Cache::open(&path, [1; 32]).unwrap();
        cache
            .put_command("r", "c", OnlineStatus::HostAccepted, None)
            .unwrap();
        cache
            .conn
            .lock()
            .unwrap()
            .execute("UPDATE meta SET value='1' WHERE key='schema_version'", [])
            .unwrap();
        drop(cache);
        let cache = Cache::open(&path, [1; 32]).unwrap();
        assert!(cache.get_command("r").unwrap().is_some());
        assert_eq!(cache.command_counts().unwrap(), (1, 0));
    }
}
