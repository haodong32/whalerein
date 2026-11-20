//! SQLite cache.
//!
//! Only `sync` writes session/event/cursor rows. Only `commands` writes the
//! command projection. Only `drafts` writes draft rows, and it seals their text
//! before storage. Sensitive payload columns are encrypted with a random cache
//! key held by `SecretStore`; the key never enters the database.

mod conversation;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use rusqlite::{params, Connection, OptionalExtension};

use crate::api::{HostInfo, OnlineStatus, PendingCommand, SessionSummary};
use crate::session::SessionKey;

pub(crate) use conversation::HistoryView;

pub const CACHE_NONCE_LEN: usize = 12;
pub const SCHEMA_VERSION: i64 = 3;
const HISTORY_CACHE_BYTES: u64 = 128 * 1024 * 1024;
/// How long an unknown send keeps its idempotency record. It is deliberately
/// longer than one command request lifetime so a retry after reconnecting can
/// still recover instead of submitting duplicate user text.
#[cfg(test)]
const SEND_INTENT_TTL_MS: i64 = 5 * 60 * 1000;

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
    #[error("cache content projection does not match the active projection")]
    ProjectionMismatch,
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

/// Why a cache was quarantined and rebuilt. Pairing identity is never part of
/// the cache, so both variants preserve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRebuild {
    /// The cache key is missing, was replaced, or no longer decrypts payloads.
    KeyMismatch,
    /// The file is not a SQLite database at all (file-level corruption).
    CorruptFile,
}

impl CacheRebuild {
    pub fn reason(self) -> &'static str {
        match self {
            Self::KeyMismatch => "cache_key_invalid",
            Self::CorruptFile => "cache_file_corrupt",
        }
    }
}

/// A missing or empty file is a valid new cache; a non-SQLite header is not.
fn has_sqlite_header(path: &Path) -> Result<bool, CacheError> {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(_) => return Err(CacheError::Database),
    };
    let mut header = [0u8; 16];
    let read = file.read(&mut header).map_err(|_| CacheError::Database)?;
    Ok(read == 0 || (read == header.len() && &header == b"SQLite format 3\0"))
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

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct CacheProvenance {
    profile: windows::Identity,
    database: windows::Identity,
}

#[cfg(unix)]
impl Drop for LockedPendingSnapshot<'_> {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

pub struct Cache {
    conn: Mutex<Connection>,
    #[cfg(any(unix, windows))]
    provenance: Option<CacheProvenance>,
    #[cfg(any(unix, windows))]
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

    #[cfg(any(unix, windows))]
    pub(crate) fn is_revocation_only(&self) -> bool {
        self.revocation_only
    }

    pub(crate) fn has_key(&self, key: &[u8]) -> bool {
        self.key == key
    }

    /// Open the cache, rebuilding a cache that cannot be read with `key`.
    ///
    /// Only two cases are rebuilt automatically: a key that does not match the
    /// database (missing or tampered cache key) and a file whose header is not
    /// SQLite (file-level corruption). A busy or future-schema database is
    /// returned as an error instead of being destroyed. Pairing lives in the
    /// configuration and SecretStore, so rebuilding never changes identity.
    pub fn open_recovering(
        path: &Path,
        key: [u8; 32],
    ) -> Result<(Self, Option<CacheRebuild>), CacheError> {
        if !has_sqlite_header(path)? {
            Self::quarantine(path)?;
            return Ok((Self::open(path, key)?, Some(CacheRebuild::CorruptFile)));
        }
        match Self::open(path, key) {
            Ok(cache) => Ok((cache, None)),
            Err(CacheError::InvalidKey) => {
                Self::quarantine(path)?;
                Ok((Self::open(path, key)?, Some(CacheRebuild::KeyMismatch)))
            }
            Err(error) => Err(error),
        }
    }

    /// Move a corrupt or undecryptable cache aside so a fresh one can be
    /// created without touching pairing configuration or credentials.
    fn quarantine(path: &Path) -> Result<(), CacheError> {
        let target = path.with_file_name("cache.sqlite3.quarantine");
        if target.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            std::fs::remove_file(&target).map_err(|_| CacheError::Database)?;
        }
        let mut moved = false;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let source = PathBuf::from(format!("{}{suffix}", path.display()));
            match std::fs::rename(&source, format!("{}{suffix}", target.display())) {
                Ok(()) => moved = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(CacheError::Database),
            }
        }
        if moved {
            Ok(())
        } else {
            Err(CacheError::Missing)
        }
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
            #[cfg(any(unix, windows))]
            provenance: None,
            #[cfg(any(unix, windows))]
            revocation_only: false,
            key,
            history_budget_bytes: HISTORY_CACHE_BYTES,
        };
        cache.initialize_schema()?;
        cache.verify_key()?;
        #[cfg(any(unix, windows))]
        {
            cache.provenance = cache.capture_provenance(path);
        }
        Ok(cache)
    }

    pub fn open_in_memory(key: [u8; 32]) -> Result<Self, CacheError> {
        let cache = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
            #[cfg(any(unix, windows))]
            provenance: None,
            #[cfg(any(unix, windows))]
            revocation_only: false,
            key,
            history_budget_bytes: HISTORY_CACHE_BYTES,
        };
        cache.initialize_schema()?;
        cache.verify_key()?;
        Ok(cache)
    }

    /// Prove that this database belongs to `self.key`.
    ///
    /// New databases store a public tag derived from the random key. A legacy
    /// database without the tag is verified by decrypting one existing sealed
    /// payload; a failure means the cache key was lost or replaced and the
    /// payloads are unreadable.
    fn verify_key(&self) -> Result<(), CacheError> {
        let tag = hex::encode(&crate::util::sha256(&self.key)[..16]);
        let conn = self.conn.lock().expect("cache mutex");
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key='cache_key_tag'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(stored) = stored {
            return if stored == tag {
                Ok(())
            } else {
                Err(CacheError::InvalidKey)
            };
        }
        let probe: Option<(String, String, Vec<u8>)> = conn
            .query_row(
                "SELECT host_id,session_id,summary FROM sessions LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((host_id, session_id, blob)) = probe {
            let key = SessionKey {
                host_id,
                session_id,
            };
            self.open_blob(&session_aad(&key), &blob)
                .map_err(|_| CacheError::InvalidKey)?;
        } else {
            let probe: Option<(String, String, String, Vec<u8>)> = conn
                .query_row(
                    "SELECT host_id,session_id,message_id,payload FROM conversation_messages LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if let Some((host, session, message_id, blob)) = probe {
                let key = SessionKey {
                    host_id: host,
                    session_id: session,
                };
                self.open_blob(&conversation::aad(&key, &message_id), &blob)
                    .map_err(|_| CacheError::InvalidKey)?;
            }
        }
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('cache_key_tag',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![tag],
        )?;
        Ok(())
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
                "2" | "3" => {}
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
             );
             CREATE TABLE IF NOT EXISTS drafts (
                 host_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 sealed_text BLOB NOT NULL,
                 updated_at_unix_ms INTEGER NOT NULL,
                 PRIMARY KEY(host_id,session_id)
             );",
        )?;
        #[cfg(test)]
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS send_intents (
                 host_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 digest TEXT NOT NULL,
                 request_id TEXT NOT NULL,
                 command_id TEXT NOT NULL,
                 created_at_unix_ms INTEGER NOT NULL,
                 PRIMARY KEY(host_id,session_id,digest)
             );",
        )?;
        // The drafts table is additive: an existing version-2 database gains it
        // in the same migration transaction without a version bump, and older
        // binaries safely ignore the extra table.
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

    #[cfg(test)]
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
        let sealed = self.sealed_sessions(host_id, sessions)?;
        let mut conn = self.conn.lock().expect("cache mutex");
        let tx = conn.transaction()?;
        for (id, body) in sealed {
            tx.execute("INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(host_id,session_id) DO UPDATE SET summary=excluded.summary,complete=excluded.complete,source=excluded.source,updated_at_unix_ms=excluded.updated_at_unix_ms",params![host_id,id,body,complete as i64,source,crate::util::now_unix_ms()])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace the complete cached session list for one Host.
    ///
    /// A completed remote listing is authoritative: sessions the source no
    /// longer reports (deleted, archived or otherwise filtered) must disappear
    /// from the offline view instead of lingering as stale rows.
    pub fn replace_sessions(
        &self,
        host_id: &str,
        sessions: &[SessionSummary],
        source: &str,
        complete: bool,
    ) -> Result<(), CacheError> {
        let sealed = self.sealed_sessions(host_id, sessions)?;
        let mut conn = self.conn.lock().expect("cache mutex");
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sessions WHERE host_id=?1", params![host_id])?;
        for (id, body) in sealed {
            tx.execute(
                "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    host_id,
                    id,
                    body,
                    complete as i64,
                    source,
                    crate::util::now_unix_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn sealed_sessions(
        &self,
        host_id: &str,
        sessions: &[SessionSummary],
    ) -> Result<Vec<(String, Vec<u8>)>, CacheError> {
        sessions
            .iter()
            .map(|session| {
                let key = SessionKey {
                    host_id: host_id.into(),
                    session_id: session.session_id.clone(),
                };
                let bytes = serde_json::to_vec(session).map_err(|_| CacheError::Corrupt)?;
                Ok((
                    session.session_id.clone(),
                    self.seal(&session_aad(&key), &bytes)?,
                ))
            })
            .collect()
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
            let key = SessionKey {
                host_id: host_id.into(),
                session_id: id,
            };
            let bytes = self.open_blob(&session_aad(&key), &body)?;
            sessions.push(serde_json::from_slice(&bytes).map_err(|_| CacheError::Corrupt)?);
        }
        sort_sessions(&mut sessions);
        Ok(sessions)
    }
    pub fn get_session(&self, key: &SessionKey) -> Result<Option<SessionSummary>, CacheError> {
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

    #[cfg(windows)]
    fn capture_provenance(&self, path: &Path) -> Option<CacheProvenance> {
        let conn = self.conn.try_lock().ok()?;
        windows::prove(&conn, path.parent()?).ok()
    }

    #[cfg(windows)]
    pub(crate) fn open_existing_verified(
        path: &Path,
        key: [u8; 32],
        directory: &std::fs::File,
    ) -> Result<Self, CacheError> {
        let conn = Connection::open_with_flags(
            windows::directory_path(directory)?.join("cache.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        let provenance = windows::prove(&conn, path.parent().ok_or(CacheError::Corrupt)?)?;
        if provenance.profile != windows::directory_identity(directory)? {
            return Err(CacheError::Corrupt);
        }
        Ok(Self {
            conn: Mutex::new(conn),
            provenance: Some(provenance),
            revocation_only: true,
            key,
            history_budget_bytes: HISTORY_CACHE_BYTES,
        })
    }

    #[cfg(windows)]
    pub(crate) fn pending_commands_verified(
        &self,
        directory: &std::fs::File,
        profile: &Path,
    ) -> Result<Vec<PendingCommand>, CacheError> {
        let provenance = self.provenance.ok_or(CacheError::Corrupt)?;
        let conn = self.conn.lock().map_err(|_| CacheError::Database)?;
        if windows::prove(&conn, profile)? != provenance
            || windows::directory_identity(directory)? != provenance.profile
        {
            return Err(CacheError::Corrupt);
        }
        windows::no_sidecars(directory)?;
        let transaction =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| match e.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    CacheError::Busy
                }
                _ => CacheError::Database,
            })?;
        let mode: String = transaction.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(CacheError::Corrupt);
        }
        let pending = Self::read_pending_commands(&transaction)?;
        windows::no_sidecars(directory)?;
        if windows::prove(&conn, profile)? != provenance {
            return Err(CacheError::Corrupt);
        }
        Ok(pending)
    }

    #[cfg(windows)]
    pub(crate) fn matches_profile(&self, directory: &std::fs::File, profile: &Path) -> bool {
        let Some(provenance) = self.provenance else {
            return false;
        };
        let Ok(conn) = self.conn.try_lock() else {
            return false;
        };
        matches!(windows::prove(&conn, profile), Ok(actual) if actual == provenance)
            && matches!(windows::directory_identity(directory), Ok(id) if id == provenance.profile)
    }

    /// Release the file handle without discarding the durable pending journal.
    #[cfg(windows)]
    pub(crate) fn release_for_revocation(&self) -> Result<(), CacheError> {
        let replacement = Self::open_in_memory(self.key)?
            .conn
            .into_inner()
            .map_err(|_| CacheError::Database)?;
        *self.conn.lock().map_err(|_| CacheError::Database)? = replacement;
        Ok(())
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

    /// Draft text is sealed by `drafts` before it reaches this storage layer;
    /// the cache only stores the opaque nonce/ciphertext blob.
    pub(crate) fn put_draft(
        &self,
        host_id: &str,
        session_id: &str,
        sealed: &[u8],
    ) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute(
            "INSERT INTO drafts(host_id, session_id, sealed_text, updated_at_unix_ms)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(host_id,session_id) DO UPDATE SET
                 sealed_text = excluded.sealed_text,
                 updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![host_id, session_id, sealed, crate::util::now_unix_ms()],
        )?;
        Ok(())
    }

    pub(crate) fn get_draft(
        &self,
        host_id: &str,
        session_id: &str,
    ) -> Result<Option<Vec<u8>>, CacheError> {
        self.conn
            .lock()
            .expect("cache mutex")
            .query_row(
                "SELECT sealed_text FROM drafts WHERE host_id=?1 AND session_id=?2",
                params![host_id, session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn delete_draft(&self, host_id: &str, session_id: &str) -> Result<(), CacheError> {
        self.conn.lock().expect("cache mutex").execute(
            "DELETE FROM drafts WHERE host_id=?1 AND session_id=?2",
            params![host_id, session_id],
        )?;
        Ok(())
    }

    pub(crate) fn delete_host_drafts(&self, host_id: &str) -> Result<(), CacheError> {
        self.conn
            .lock()
            .expect("cache mutex")
            .execute("DELETE FROM drafts WHERE host_id=?1", params![host_id])?;
        Ok(())
    }

    #[cfg(any(not(unix), test))]
    pub fn clear(&self) -> Result<(), CacheError> {
        #[cfg(windows)]
        if self.revocation_only {
            return self.release_for_revocation();
        }
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute_batch(
            "DELETE FROM sessions; DELETE FROM commands; DELETE FROM hosts; DELETE FROM history_receipts; DELETE FROM conversation_views; DELETE FROM conversation_messages; DELETE FROM conversation_stages; DELETE FROM conversation_stage_messages; DELETE FROM drafts;
             UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='cache_generation';",
        )?;
        #[cfg(test)]
        conn.execute_batch("DELETE FROM send_intents;")?;
        Ok(())
    }

    /// Bind outgoing intent to this identity without storing plaintext.
    pub(crate) fn send_intent_digest(&self, text: &str) -> String {
        let mut material = self.key.to_vec();
        material.extend_from_slice(text.as_bytes());
        crate::util::sha256_hex(&material)
    }

    /// Remember a send whose observation ended unknown so an identical resend
    /// recovers the original command instead of creating a second user message.
    #[cfg(test)]
    pub(crate) fn record_send_intent(
        &self,
        host_id: &str,
        session_id: &str,
        digest: &str,
        request_id: &str,
        command_id: &str,
    ) -> Result<(), CacheError> {
        self.conn
            .lock()
            .expect("cache mutex")
            .execute(
                "INSERT INTO send_intents(host_id,session_id,digest,request_id,command_id,created_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(host_id,session_id,digest) DO UPDATE SET request_id=excluded.request_id,command_id=excluded.command_id,created_at_unix_ms=excluded.created_at_unix_ms",
                params![
                    host_id,
                    session_id,
                    digest,
                    request_id,
                    command_id,
                    crate::util::now_unix_ms()
                ],
            )?;
        Ok(())
    }

    /// The retained request for an identical send that has not been confirmed
    /// yet, if one is still inside the idempotency window.
    #[cfg(test)]
    pub(crate) fn unresolved_send_intent(
        &self,
        host_id: &str,
        session_id: &str,
        digest: &str,
    ) -> Result<Option<String>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let now = crate::util::now_unix_ms();
        conn.execute(
            "DELETE FROM send_intents WHERE created_at_unix_ms < ?1",
            params![now - SEND_INTENT_TTL_MS],
        )?;
        Ok(conn
            .query_row(
                "SELECT request_id FROM send_intents WHERE host_id=?1 AND session_id=?2 AND digest=?3",
                params![host_id, session_id, digest],
                |row| row.get(0),
            )
            .optional()?)
    }

    #[cfg(test)]
    pub(crate) fn resolve_send_intent(
        &self,
        host_id: &str,
        session_id: &str,
        digest: &str,
    ) -> Result<(), CacheError> {
        {
            let conn = self.conn.lock().expect("cache mutex");
            conn.execute(
                "DELETE FROM send_intents WHERE host_id=?1 AND session_id=?2 AND digest=?3",
                params![host_id, session_id, digest],
            )?;
        }
        #[cfg(windows)]
        self.release_for_revocation()?;
        Ok(())
    }
}

fn session_aad(key: &SessionKey) -> Vec<u8> {
    serde_json::to_vec(&["session", key.host_id.as_str(), key.session_id.as_str()])
        .expect("string tuple")
}

/// One order for remote and cached session listings.
///
/// Most recently updated sessions come first, with the session identity as a
/// deterministic tie-break. Without a shared order a cache-only view would sort
/// by identity and the list would visibly jump when the Host reconnects.
pub(crate) fn sort_sessions(sessions: &mut [SessionSummary]) {
    sessions.sort_by(|left, right| {
        right
            .updated_at_unix_ms
            .cmp(&left.updated_at_unix_ms)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
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
        let a = SessionKey {
            host_id: "a".into(),
            session_id: "same".into(),
        };
        let b = SessionKey {
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
    fn adds_opaque_draft_storage_to_an_existing_version_two_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO meta VALUES('schema_version','2');
             CREATE TABLE commands(request_id TEXT PRIMARY KEY, command_id TEXT NOT NULL, status TEXT NOT NULL, error_code TEXT, updated_at_unix_ms INTEGER NOT NULL);
             INSERT INTO commands VALUES('legacy','old-command','unknown',NULL,1);",
        )
        .unwrap();
        drop(conn);
        let cache = Cache::open(&path, [1; 32]).unwrap();
        assert!(cache.get_draft("h", "s").unwrap().is_none());
        cache.put_draft("h", "s", b"sealed").unwrap();
        assert_eq!(
            cache.get_draft("h", "s").unwrap().as_deref(),
            Some(&b"sealed"[..])
        );
        assert!(cache.get_command("legacy").unwrap().is_some());
        cache.delete_host_drafts("h").unwrap();
        assert!(cache.get_draft("h", "s").unwrap().is_none());
        assert_eq!(
            cache
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            SCHEMA_VERSION.to_string()
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

#[cfg(test)]
mod session_list_tests {
    use super::*;

    fn session(session_id: &str, updated_at_unix_ms: i64) -> SessionSummary {
        SessionSummary::from(whalerein_proto::SessionSummary {
            session_id: session_id.into(),
            title: session_id.into(),
            updated_at_unix_ms,
            ..Default::default()
        })
    }

    #[test]
    fn a_complete_session_snapshot_replaces_removed_entries() {
        let cache = Cache::open_in_memory([7; 32]).unwrap();
        cache
            .replace_sessions(
                "host-1",
                &[session("session-b", 20), session("session-a", 10)],
                "remote",
                true,
            )
            .unwrap();
        assert_eq!(cache.load_sessions("host-1").unwrap().len(), 2);

        cache
            .replace_sessions("host-1", &[session("session-a", 10)], "remote", true)
            .unwrap();
        let sessions = cache.load_sessions("host-1").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-a");
        assert!(cache
            .get_session(&SessionKey {
                host_id: "host-1".into(),
                session_id: "session-b".into(),
            })
            .unwrap()
            .is_none());

        // A single-summary refresh (get_session) upserts without deleting the
        // rest of the authoritative snapshot.
        cache
            .put_sessions("host-1", &[session("session-c", 5)], "remote", true)
            .unwrap();
        assert_eq!(cache.load_sessions("host-1").unwrap().len(), 2);
    }

    #[test]
    fn cached_sessions_sort_by_update_time_then_identity() {
        let cache = Cache::open_in_memory([7; 32]).unwrap();
        cache
            .replace_sessions(
                "host-1",
                &[
                    session("session-c", 10),
                    session("session-a", 10),
                    session("session-b", 20),
                ],
                "remote",
                true,
            )
            .unwrap();
        let ids = cache
            .load_sessions("host-1")
            .unwrap()
            .into_iter()
            .map(|session| session.session_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["session-b", "session-a", "session-c"]);
    }

    #[test]
    fn replacement_is_scoped_to_one_host() {
        let cache = Cache::open_in_memory([7; 32]).unwrap();
        cache
            .replace_sessions("host-1", &[session("session-1", 1)], "remote", true)
            .unwrap();
        cache
            .replace_sessions("host-2", &[session("session-2", 1)], "remote", true)
            .unwrap();
        cache
            .replace_sessions("host-1", &[], "remote", true)
            .unwrap();
        assert!(cache.load_sessions("host-1").unwrap().is_empty());
        assert_eq!(cache.load_sessions("host-2").unwrap().len(), 1);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn path(directory: &Path) -> PathBuf {
        directory.join("cache.sqlite3")
    }

    #[test]
    fn a_replaced_cache_key_is_detected_and_recovery_quarantines_the_old_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = path(directory.path());
        {
            let cache = Cache::open(&path, [1u8; 32]).unwrap();
            cache
                .put_sessions(
                    "host-1",
                    &[crate::api::SessionSummary::from(
                        whalerein_proto::SessionSummary {
                            session_id: "s-1".into(),
                            ..Default::default()
                        },
                    )],
                    "remote",
                    true,
                )
                .unwrap();
        }
        assert!(matches!(
            Cache::open(&path, [2u8; 32]),
            Err(CacheError::InvalidKey)
        ));
        let (cache, rebuilt) = Cache::open_recovering(&path, [2u8; 32]).unwrap();
        assert_eq!(rebuilt, Some(CacheRebuild::KeyMismatch));
        assert!(cache.load_sessions("host-1").unwrap().is_empty());
        let quarantine = directory.path().join("cache.sqlite3.quarantine");
        assert!(quarantine.is_file());
        assert!(std::fs::metadata(&quarantine).unwrap().len() > 0);
    }

    #[test]
    fn a_non_sqlite_file_is_quarantined_and_rebuilt_without_erroring() {
        let directory = tempfile::tempdir().unwrap();
        let path = path(directory.path());
        std::fs::write(&path, b"garbage cache file").unwrap();
        let (cache, rebuilt) = Cache::open_recovering(&path, [3u8; 32]).unwrap();
        assert_eq!(rebuilt, Some(CacheRebuild::CorruptFile));
        assert!(cache.load_sessions("host-1").unwrap().is_empty());
        assert_eq!(
            std::fs::read(directory.path().join("cache.sqlite3.quarantine")).unwrap(),
            b"garbage cache file"
        );
    }

    #[test]
    fn a_fresh_cache_stores_a_tag_and_reopens_with_the_same_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = path(directory.path());
        drop(Cache::open(&path, [4u8; 32]).unwrap());
        let (cache, rebuilt) = Cache::open_recovering(&path, [4u8; 32]).unwrap();
        assert_eq!(rebuilt, None);
        assert!(cache.load_sessions("host-1").unwrap().is_empty());
        assert!(!directory.path().join("cache.sqlite3.quarantine").exists());
    }
}
