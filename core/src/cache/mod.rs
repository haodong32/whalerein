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
    #[error("cache payload is corrupt")]
    Corrupt,
}

impl From<rusqlite::Error> for CacheError {
    fn from(_: rusqlite::Error) -> Self {
        CacheError::Database
    }
}

pub struct Cache {
    conn: Mutex<Connection>,
    key: [u8; 32],
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cache(..)")
    }
}

impl Cache {
    pub fn open(path: &Path, key: [u8; 32]) -> Result<Self, CacheError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| CacheError::Database)?;
        }
        let conn = Connection::open(path)?;
        let cache = Self {
            conn: Mutex::new(conn),
            key,
        };
        cache.initialize_schema()?;
        Ok(cache)
    }

    pub fn open_in_memory(key: [u8; 32]) -> Result<Self, CacheError> {
        let cache = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
            key,
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
            .and_then(|_| self.initialize_conversations());
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
        for column in ["host_id", "device_id", "access_url", "host_fingerprint"] {
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

    pub(crate) fn record_command_intent(
        &self,
        request_id: &str,
        command_id: &str,
        host_id: &str,
        device_id: &str,
        access_url: &str,
        host_fingerprint: &str,
    ) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute("INSERT INTO commands(request_id, command_id, host_id, device_id, access_url, host_fingerprint, status, updated_at_unix_ms) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'unknown', ?7)", params![request_id, command_id, host_id, device_id, access_url, host_fingerprint, crate::util::now_unix_ms()])?;
        Ok(())
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
