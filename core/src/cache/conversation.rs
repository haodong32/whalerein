//! Host-scoped, encrypted conversation views. Snapshot staging never replaces
//! a complete view until its final page commits.
use super::*;
use crate::conversation::ConversationMessage;
use crate::session::SessionKey;
use prost::Message;
use whalerein_proto::{HistoryPage, MessageUpsert};

pub(super) fn aad(key: &SessionKey, id: &str) -> Vec<u8> {
    serde_json::to_vec(&[
        "conversation",
        key.host_id.as_str(),
        key.session_id.as_str(),
        id,
    ])
    .expect("string tuple")
}

// Evict complete source views, never individual messages beneath a retained
// cursor. Command recovery records and endpoint credentials are not cache data.
fn trim_history(
    conn: &Connection,
    protected: Option<&SessionKey>,
    budget: u64,
) -> Result<(), CacheError> {
    loop {
        let bytes: i64=conn.query_row("SELECT COALESCE((SELECT sum(length(payload)) FROM conversation_messages),0) + COALESCE((SELECT sum(length(payload)) FROM conversation_stage_messages),0)",[],|row|row.get(0))?;
        if bytes as u64 <= budget {
            return Ok(());
        }
        let excluded: whalerein_proto::SessionKey = protected.map(Into::into).unwrap_or_default();
        let candidate:Option<(String,String)>=conn.query_row("SELECT host_id,session_id FROM (SELECT host_id,session_id,rowid AS position FROM conversation_views UNION ALL SELECT host_id,session_id,rowid AS position FROM conversation_stages) WHERE NOT(host_id=?1 AND session_id=?2) ORDER BY position,host_id,session_id LIMIT 1",params![excluded.host_id,excluded.session_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        let Some((host, session)) = candidate else {
            return Err(CacheError::HistoryBudget);
        };
        for table in [
            "conversation_messages",
            "conversation_views",
            "history_receipts",
            "conversation_stage_messages",
            "conversation_stages",
        ] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE host_id=?1 AND session_id=?2"),
                params![host, session],
            )?;
        }
        bump_cache_generation(conn)?;
    }
}

/// The durable identity of one complete cached conversation view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryView {
    pub cursor: String,
    pub projection: String,
    pub generation: i64,
}

impl Cache {
    pub(super) fn trim_history_cache(
        &self,
        protected: Option<&SessionKey>,
    ) -> Result<(), CacheError> {
        trim_history(
            &self.conn.lock().expect("cache mutex"),
            protected,
            self.history_budget_bytes,
        )
    }

    /// Additive version-3 schema. Existing version-2 rows stay in place and
    /// carry an empty projection until a complete replacement installs the
    /// active projection.
    pub fn initialize_conversations(&self) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversation_views (
               host_id TEXT NOT NULL, session_id TEXT NOT NULL, cursor TEXT NOT NULL,
               projection TEXT NOT NULL DEFAULT '', generation INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(host_id,session_id));
             CREATE TABLE IF NOT EXISTS conversation_messages (
               host_id TEXT NOT NULL, session_id TEXT NOT NULL, message_id TEXT NOT NULL,
               ordinal INTEGER NOT NULL, payload BLOB NOT NULL,
               projection TEXT NOT NULL DEFAULT '',
               PRIMARY KEY(host_id,session_id,message_id));
             CREATE TABLE IF NOT EXISTS history_receipts (
               host_id TEXT NOT NULL, session_id TEXT NOT NULL, batch_id TEXT NOT NULL,
               digest BLOB NOT NULL, applied_at INTEGER NOT NULL,
               projection TEXT NOT NULL DEFAULT '',
               PRIMARY KEY(host_id,session_id,batch_id));
             CREATE TABLE IF NOT EXISTS conversation_stages (
               host_id TEXT NOT NULL, session_id TEXT NOT NULL, cut TEXT NOT NULL,
               cursor TEXT NOT NULL, projection TEXT NOT NULL DEFAULT '',
               generation INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(host_id,session_id));
             CREATE TABLE IF NOT EXISTS conversation_stage_messages (
               host_id TEXT NOT NULL, session_id TEXT NOT NULL, message_id TEXT NOT NULL,
               ordinal INTEGER NOT NULL, payload BLOB NOT NULL,
               projection TEXT NOT NULL DEFAULT '',
               PRIMARY KEY(host_id,session_id,message_id));",
        )?;
        for (table, column, definition) in [
            (
                "conversation_views",
                "projection",
                "TEXT NOT NULL DEFAULT ''",
            ),
            (
                "conversation_views",
                "generation",
                "INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "conversation_stages",
                "projection",
                "TEXT NOT NULL DEFAULT ''",
            ),
            (
                "conversation_stages",
                "generation",
                "INTEGER NOT NULL DEFAULT 0",
            ),
            ("history_receipts", "projection", "TEXT NOT NULL DEFAULT ''"),
            (
                "conversation_messages",
                "projection",
                "TEXT NOT NULL DEFAULT ''",
            ),
            (
                "conversation_stage_messages",
                "projection",
                "TEXT NOT NULL DEFAULT ''",
            ),
        ] {
            let columns: Vec<String> = conn
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |row| row.get(1))?
                .collect::<Result<_, _>>()?;
            if !columns.iter().any(|name| name == column) {
                conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition};"
                ))?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS conversation_messages_ordinal
               ON conversation_messages(host_id,session_id,ordinal);
             CREATE INDEX IF NOT EXISTS conversation_stage_messages_ordinal
               ON conversation_stage_messages(host_id,session_id,ordinal);",
        )?;
        if !meta_value(&conn, "cache_install_id")?.is_some_and(|value| !value.is_empty()) {
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('cache_install_id',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![crate::util::random_id(16)],
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('projection_version',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('cache_generation','0')
             ON CONFLICT(key) DO NOTHING",
            [],
        )?;
        Ok(())
    }

    pub(crate) fn cache_install_id(&self) -> Result<String, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        meta_value(&conn, "cache_install_id")?.ok_or(CacheError::Corrupt)
    }

    #[cfg(test)]
    pub(crate) fn clear_projection_for_test(&self, key: &SessionKey) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        conn.execute(
            "UPDATE conversation_views SET projection='' WHERE host_id=?1 AND session_id=?2",
            params![key.host_id, key.session_id],
        )?;
        conn.execute(
            "UPDATE conversation_messages SET projection='' WHERE host_id=?1 AND session_id=?2",
            params![key.host_id, key.session_id],
        )?;
        Ok(())
    }

    pub(crate) fn cache_generation(&self) -> Result<u64, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        meta_generation(&conn, "cache_generation")
    }

    pub fn history_cursor(&self, key: &SessionKey) -> Result<Option<String>, CacheError> {
        Ok(self
            .conn
            .lock()
            .expect("cache mutex")
            .query_row(
                "SELECT cursor FROM conversation_views WHERE host_id=?1 AND session_id=?2",
                params![key.host_id, key.session_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn history_view(&self, key: &SessionKey) -> Result<Option<HistoryView>, CacheError> {
        self.conn
            .lock()
            .expect("cache mutex")
            .query_row(
                "SELECT cursor,projection,generation FROM conversation_views
                 WHERE host_id=?1 AND session_id=?2",
                params![key.host_id, key.session_id],
                |row| {
                    Ok(HistoryView {
                        cursor: row.get(0)?,
                        projection: row.get(1)?,
                        generation: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Committed message identities in ordinal order, without decrypting
    /// payloads. A watch attach uses this to mark history that already exists
    /// locally as seen, so a later gap recovery cannot replay it.
    pub fn history_message_ids(&self, key: &SessionKey) -> Result<Vec<String>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let mut statement = conn.prepare(
            "SELECT message_id FROM conversation_messages
             WHERE host_id=?1 AND session_id=?2
               AND projection=(SELECT projection FROM conversation_views
                               WHERE host_id=?1 AND session_id=?2)
             ORDER BY ordinal",
        )?;
        let rows = statement.query_map(params![key.host_id, key.session_id], |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn history_messages(
        &self,
        key: &SessionKey,
    ) -> Result<Vec<ConversationMessage>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let mut statement = conn.prepare(
            "SELECT message_id,payload FROM conversation_messages
             WHERE host_id=?1 AND session_id=?2
               AND projection=(SELECT projection FROM conversation_views
                               WHERE host_id=?1 AND session_id=?2)
             ORDER BY ordinal",
        )?;
        let rows = statement.query_map(params![key.host_id, key.session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (id, blob) = row?;
            let bytes = self.open_blob(&aad(key, &id), &blob)?;
            let message = MessageUpsert::decode(bytes.as_slice())
                .map_err(|_| CacheError::Corrupt)?
                .message
                .ok_or(CacheError::Corrupt)?;
            messages.push(message.into());
        }
        Ok(messages)
    }

    pub fn next_history_message(
        &self,
        key: &SessionKey,
        after: i64,
    ) -> Result<Option<(i64, MessageUpsert)>, CacheError> {
        let row: Option<(i64, String, Vec<u8>)> = self
            .conn
            .lock()
            .expect("cache mutex")
            .query_row(
                "SELECT ordinal,message_id,payload FROM conversation_messages
             WHERE host_id=?1 AND session_id=?2 AND ordinal>?3
               AND projection=(SELECT projection FROM conversation_views
                               WHERE host_id=?1 AND session_id=?2)
             ORDER BY ordinal LIMIT 1",
                params![key.host_id, key.session_id, after],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(ordinal, id, blob)| {
            let bytes = self.open_blob(&aad(key, &id), &blob)?;
            Ok((
                ordinal,
                MessageUpsert::decode(bytes.as_slice()).map_err(|_| CacheError::Corrupt)?,
            ))
        })
        .transpose()
    }

    pub fn discard_history_stage(&self, key: &SessionKey) -> Result<(), CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        for table in ["conversation_stages", "conversation_stage_messages"] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE host_id=?1 AND session_id=?2"),
                params![key.host_id, key.session_id],
            )?;
        }
        Ok(())
    }

    /// Apply one validated page at a fixed cut.
    ///
    /// A complete snapshot is installed in one transaction. Until that commit
    /// the previous view and receipts remain readable under their own
    /// projection; the new projection is staged separately. A non-snapshot
    /// page may only continue a view with the same projection.
    pub fn apply_history_page(&self, page: &HistoryPage) -> Result<bool, CacheError> {
        let key = SessionKey::from(page.session.clone().ok_or(CacheError::Corrupt)?);
        let batch = page.batch.as_ref().ok_or(CacheError::Corrupt)?;
        if key.host_id.is_empty()
            || key.session_id.is_empty()
            || batch.to_cursor.is_empty()
            || page.cut_cursor.is_empty()
            || page.projection_id.is_empty()
            || page.projection_id.len() > 128
        {
            return Err(CacheError::Corrupt);
        }
        if !batch.projection_id.is_empty() && batch.projection_id != page.projection_id {
            return Err(CacheError::ProjectionMismatch);
        }
        let mut canonical = batch.clone();
        for change in &mut canonical.changes {
            change.origin = None;
        }
        let digest = crate::util::sha256(&canonical.encode_to_vec());
        let projection = page.projection_id.clone();
        let receipt_aad = serde_json::to_vec(&[
            "history-receipt",
            key.host_id.as_str(),
            key.session_id.as_str(),
            projection.as_str(),
            batch.batch_id.as_str(),
        ])
        .expect("string tuple");
        let protected_digest = self.seal(&receipt_aad, &digest)?;
        let sealed: Vec<_> = batch
            .changes
            .iter()
            .map(|change| {
                let message = change.message.as_ref().ok_or(CacheError::Corrupt)?;
                if message.message_id.is_empty() {
                    return Err(CacheError::Corrupt);
                }
                Ok((
                    message.message_id.clone(),
                    self.seal(&aad(&key, &message.message_id), &change.encode_to_vec())?,
                ))
            })
            .collect::<Result<_, CacheError>>()?;
        let mut conn = self.conn.lock().expect("cache mutex");
        let tx = conn.transaction()?;
        let known: Option<(Vec<u8>, String)> = tx
            .query_row(
                "SELECT digest,projection FROM history_receipts
                 WHERE host_id=?1 AND session_id=?2 AND batch_id=?3",
                params![key.host_id, key.session_id, batch.batch_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((known, known_projection)) = known {
            if known_projection != projection {
                // A replacement snapshot starts a fresh receipt namespace;
                // old receipts stay only until that replacement commits. A
                // delta or duplicate in the old namespace is still fenced.
                if !page.snapshot {
                    return Err(CacheError::ProjectionMismatch);
                }
            } else if self.open_blob(&receipt_aad, &known)? != digest {
                return Err(CacheError::Corrupt);
            }
            if known_projection == projection && page.snapshot && page.complete {
                let current: Option<(String, String)> = tx
                    .query_row(
                        "SELECT cursor,projection FROM conversation_views
                         WHERE host_id=?1 AND session_id=?2",
                        params![key.host_id, key.session_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if current == Some((batch.to_cursor.clone(), projection.clone())) {
                    return Ok(false);
                }
            }
        }
        let previous_generation: Option<i64> = tx
            .query_row(
                "SELECT generation FROM conversation_views WHERE host_id=?1 AND session_id=?2",
                params![key.host_id, key.session_id],
                |row| row.get(0),
            )
            .optional()?;
        let table = if page.snapshot {
            "conversation_stage_messages"
        } else {
            "conversation_messages"
        };
        if page.snapshot {
            let stage: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT cut,cursor,projection FROM conversation_stages
                     WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((cut, cursor, staged_projection)) = stage {
                if cut != page.cut_cursor
                    || cursor != batch.from_cursor
                    || staged_projection != projection
                {
                    return Err(CacheError::ProjectionMismatch);
                }
            } else {
                tx.execute(
                    "DELETE FROM conversation_stage_messages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
            }
        } else {
            let view: Option<(String, String, i64)> = tx
                .query_row(
                    "SELECT cursor,projection,generation FROM conversation_views
                     WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((cursor, current_projection, _)) = view else {
                return Err(CacheError::HistoryGap);
            };
            if current_projection != projection {
                return Err(CacheError::ProjectionMismatch);
            }
            if cursor == batch.to_cursor {
                return Ok(false);
            }
            if cursor != batch.from_cursor {
                return Err(CacheError::HistoryGap);
            }
        }
        let ordinal_start:i64=tx.query_row(&format!("SELECT COALESCE(MAX(ordinal),-1)+1 FROM {table} WHERE host_id=?1 AND session_id=?2"),params![key.host_id,key.session_id],|row|row.get(0))?;
        for (index, (id, payload)) in sealed.into_iter().enumerate() {
            let ordinal = ordinal_start + index as i64;
            tx.execute(&format!("INSERT INTO {table}(host_id,session_id,message_id,ordinal,payload,projection) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(host_id,session_id,message_id) DO UPDATE SET payload=excluded.payload,projection=excluded.projection"),params![key.host_id,key.session_id,id,ordinal,payload,projection])?;
        }
        if page.snapshot && !page.complete {
            tx.execute("INSERT INTO conversation_stages(host_id,session_id,cut,cursor,projection,generation) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(host_id,session_id) DO UPDATE SET cut=excluded.cut,cursor=excluded.cursor,projection=excluded.projection,generation=excluded.generation",params![key.host_id,key.session_id,page.cut_cursor,batch.to_cursor,projection,previous_generation.unwrap_or(0)])?;
        } else {
            if page.snapshot {
                tx.execute(
                    "DELETE FROM conversation_messages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
                tx.execute("INSERT INTO conversation_messages(host_id,session_id,message_id,ordinal,payload,projection) SELECT host_id,session_id,message_id,ordinal,payload,projection FROM conversation_stage_messages WHERE host_id=?1 AND session_id=?2",params![key.host_id,key.session_id])?;
                tx.execute(
                    "DELETE FROM conversation_stage_messages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
                tx.execute(
                    "DELETE FROM conversation_stages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
                // Old receipts belong to the fenced projection and must not
                // authenticate a batch in the replacement namespace.
                tx.execute(
                    "DELETE FROM history_receipts WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
                let generation = previous_generation.unwrap_or(0).saturating_add(1);
                tx.execute("INSERT INTO conversation_views(host_id,session_id,cursor,projection,generation) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(host_id,session_id) DO UPDATE SET cursor=excluded.cursor,projection=excluded.projection,generation=excluded.generation",params![key.host_id,key.session_id,batch.to_cursor,projection,generation])?;
                bump_cache_generation(&tx)?;
            } else {
                tx.execute(
                    "UPDATE conversation_views SET cursor=?3 WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id, batch.to_cursor],
                )?;
            }
        }
        tx.execute("INSERT INTO history_receipts(host_id,session_id,batch_id,digest,applied_at,projection) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(host_id,session_id,batch_id) DO NOTHING",params![key.host_id,key.session_id,batch.batch_id,protected_digest,crate::util::now_unix_ms(),projection])?;
        tx.execute("DELETE FROM history_receipts WHERE host_id=?1 AND session_id=?2 AND batch_id NOT IN (SELECT batch_id FROM history_receipts WHERE host_id=?1 AND session_id=?2 ORDER BY applied_at DESC,rowid DESC LIMIT 512)",params![key.host_id,key.session_id])?;
        if let Err(error) = trim_history(&tx, Some(&key), self.history_budget_bytes) {
            tx.rollback()?;
            // Earlier pages of an abandoned replacement must not pin the budget.
            // Keep the last committed view and its recovery metadata intact.
            if matches!(error, CacheError::HistoryBudget) {
                let cleanup = conn.transaction()?;
                for table in ["conversation_stage_messages", "conversation_stages"] {
                    cleanup.execute(
                        &format!("DELETE FROM {table} WHERE host_id=?1 AND session_id=?2"),
                        params![key.host_id, key.session_id],
                    )?;
                }
                cleanup.commit()?;
            }

            return Err(error);
        }
        tx.commit()?;
        Ok(!page.snapshot || page.complete)
    }
    pub fn apply_history_batch(
        &self,
        key: &SessionKey,
        batch: &whalerein_proto::HistoryBatch,
    ) -> Result<bool, CacheError> {
        self.apply_history_page(&HistoryPage {
            session: Some(key.into()),
            batch: Some(batch.clone()),
            cut_cursor: batch.to_cursor.clone(),
            complete: true,
            snapshot: false,
            continuation: String::new(),
            projection_id: batch.projection_id.clone(),
            source: whalerein_proto::HistorySource::Online as i32,
        })
    }
}

fn meta_value(conn: &Connection, key: &str) -> Result<Option<String>, CacheError> {
    conn.query_row("SELECT value FROM meta WHERE key=?1", params![key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(Into::into)
}

fn meta_generation(conn: &Connection, key: &str) -> Result<u64, CacheError> {
    let value = meta_value(conn, key)?.unwrap_or_else(|| "0".into());
    value.parse::<u64>().map_err(|_| CacheError::Corrupt)
}

fn bump_cache_generation(conn: &Connection) -> Result<(), CacheError> {
    let generation = meta_generation(conn, "cache_generation")?.saturating_add(1);
    conn.execute(
        "INSERT INTO meta(key,value) VALUES('cache_generation',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![generation.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_proto::{
        content_block, ContentBlock, ConversationMessage as WireConversationMessage, HistoryBatch,
        MessageRole, TextContent,
    };
    fn key(host: &str) -> SessionKey {
        SessionKey {
            host_id: host.into(),
            session_id: "same".into(),
        }
    }
    fn page(
        host: &str,
        from: &str,
        to: &str,
        cut: &str,
        complete: bool,
        snapshot: bool,
        id: &str,
    ) -> HistoryPage {
        HistoryPage {
            session: Some((&key(host)).into()),
            batch: Some(HistoryBatch {
                from_cursor: from.into(),
                to_cursor: to.into(),
                batch_id: format!("{from}:{to}"),
                projection_id: "projection-1".into(),
                changes: if id.is_empty() {
                    vec![]
                } else {
                    vec![MessageUpsert {
                        message: Some(WireConversationMessage {
                            message_id: id.into(),
                            role: MessageRole::Assistant as i32,
                            blocks: vec![ContentBlock {
                                block_id: "t".into(),
                                content: Some(content_block::Content::Text(TextContent {
                                    text: "x".into(),
                                })),
                            }],
                            ..Default::default()
                        }),
                        origin: None,
                    }]
                },
            }),
            cut_cursor: cut.into(),
            complete,
            snapshot,
            continuation: String::new(),
            projection_id: "projection-1".into(),
            source: whalerein_proto::HistorySource::Online as i32,
        }
    }
    #[test]
    fn snapshot_staging_is_atomic_and_host_scoped() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        cache
            .apply_history_page(&page("a", "start", "old", "old", true, true, "old"))
            .unwrap();
        assert!(!cache
            .apply_history_page(&page("a", "start", "middle", "new", false, true, "one"))
            .unwrap());
        assert_eq!(
            cache.history_cursor(&key("a")).unwrap().as_deref(),
            Some("old")
        );
        assert_eq!(
            cache.history_messages(&key("a")).unwrap()[0].message_id,
            "old"
        );
        let mismatch = page("a", "wrong", "new", "different", true, true, "two");
        assert!(cache.apply_history_page(&mismatch).is_err());
        assert!(cache
            .apply_history_page(&page("a", "middle", "new", "new", true, true, "two"))
            .unwrap());
        assert_eq!(cache.history_messages(&key("a")).unwrap().len(), 2);
        cache
            .apply_history_page(&page("b", "start", "other", "other", true, true, "b"))
            .unwrap();
        assert_eq!(cache.history_messages(&key("b")).unwrap().len(), 1);
        assert!(cache.next_history_message(&key("a"), -1).unwrap().is_some());
        assert!(cache
            .next_history_message(&key("a"), 100)
            .unwrap()
            .is_none());
    }
    #[test]
    fn parent_and_child_with_same_message_id_and_batch_id_keep_independent_views() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        let parent = SessionKey {
            host_id: "host".into(),
            session_id: "parent".into(),
        };
        let child = SessionKey {
            host_id: "host".into(),
            session_id: "child".into(),
        };
        let mut original = page("host", "start", "cut", "cut", true, true, "same-id");
        original.session = Some((&parent).into());
        cache.apply_history_page(&original).unwrap();
        let mut inherited = original.clone();
        inherited.session = Some((&child).into());
        cache.apply_history_page(&inherited).unwrap();
        let mut child_delta = page(
            "host",
            "cut",
            "child-next",
            "child-next",
            true,
            false,
            "same-id",
        );
        child_delta.session = Some((&child).into());
        child_delta.batch.as_mut().unwrap().changes[0]
            .message
            .as_mut()
            .unwrap()
            .blocks[0]
            .content = Some(content_block::Content::Text(TextContent {
            text: "child".into(),
        }));
        cache.apply_history_page(&child_delta).unwrap();
        assert_eq!(
            cache.history_cursor(&parent).unwrap().as_deref(),
            Some("cut")
        );
        assert_eq!(
            cache.history_cursor(&child).unwrap().as_deref(),
            Some("child-next")
        );
        let parent_message = &cache.history_messages(&parent).unwrap()[0];
        let child_message = &cache.history_messages(&child).unwrap()[0];
        assert_eq!(parent_message.message_id, child_message.message_id);
        assert_ne!(parent_message.blocks, child_message.blocks);
    }

    #[test]
    fn duplicate_batches_are_idempotent_and_conflicting_receipts_are_rejected() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        cache
            .apply_history_page(&page("a", "start", "one", "one", true, true, "a"))
            .unwrap();
        let delta = page("a", "one", "two", "two", true, false, "b");
        cache.apply_history_page(&delta).unwrap();
        assert!(!cache.apply_history_page(&delta).unwrap());
        let mut conflict = delta.clone();
        conflict.batch.as_mut().unwrap().changes[0]
            .message
            .as_mut()
            .unwrap()
            .message_id = "changed".into();
        assert!(matches!(
            cache.apply_history_page(&conflict),
            Err(CacheError::Corrupt)
        ));
        assert_eq!(cache.history_messages(&key("a")).unwrap().len(), 2);
        assert!(matches!(
            cache.apply_history_page(&page("a", "missing", "three", "three", true, false, "c")),
            Err(CacheError::HistoryGap)
        ));
    }
    #[test]
    fn discarded_partial_views_and_empty_source_positions_do_not_erase_committed_history() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        cache
            .apply_history_page(&page("a", "start", "one", "one", true, true, "a"))
            .unwrap();
        cache
            .apply_history_page(&page("a", "start", "part", "end", false, true, "b"))
            .unwrap();
        cache.discard_history_stage(&key("a")).unwrap();
        assert_eq!(
            cache.history_messages(&key("a")).unwrap()[0].message_id,
            "a"
        );
        cache
            .apply_history_page(&page(
                "a",
                "one",
                "internal-only",
                "internal-only",
                true,
                false,
                "",
            ))
            .unwrap();
        assert_eq!(cache.history_messages(&key("a")).unwrap().len(), 1);
        assert_eq!(
            cache.history_cursor(&key("a")).unwrap().as_deref(),
            Some("internal-only")
        );
        let mut invalid = page("a", "x", "y", "y", true, false, "a");
        invalid.session = None;
        assert!(cache.apply_history_page(&invalid).is_err());
        cache.clear().unwrap();
        assert!(cache.history_cursor(&key("a")).unwrap().is_none());
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use whalerein_proto::{
        content_block, ContentBlock, ConversationMessage as WireConversationMessage, HistoryBatch,
        MessageRole, TextContent,
    };

    fn page(host: &str, from: &str, to: &str, text: &str, complete: bool) -> HistoryPage {
        HistoryPage {
            session: Some(whalerein_proto::SessionKey {
                host_id: host.into(),
                session_id: "s".into(),
            }),
            batch: Some(HistoryBatch {
                from_cursor: from.into(),
                to_cursor: to.into(),
                batch_id: format!("{from}:{to}"),
                projection_id: "projection-1".into(),
                changes: vec![MessageUpsert {
                    message: Some(WireConversationMessage {
                        message_id: to.into(),
                        role: MessageRole::Assistant as i32,
                        blocks: vec![ContentBlock {
                            block_id: "text".into(),
                            content: Some(content_block::Content::Text(TextContent {
                                text: text.into(),
                            })),
                        }],
                        ..Default::default()
                    }),
                    origin: None,
                }],
            }),
            snapshot: true,
            complete,
            cut_cursor: "cut".into(),
            continuation: String::new(),
            projection_id: "projection-1".into(),
            source: whalerein_proto::HistorySource::Online as i32,
        }
    }

    #[test]
    fn budget_evicts_whole_old_views_and_preserves_uncertain_commands() {
        let mut cache = Cache::open_in_memory([1; 32]).unwrap();
        for host in ["a", "b"] {
            cache
                .apply_history_page(&page(host, "", "one", "fixture", true))
                .unwrap();
        }
        let used: i64 = cache
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT sum(length(payload)) FROM conversation_messages",
                [],
                |r| r.get(0),
            )
            .unwrap();
        cache.history_budget_bytes = used as u64;
        cache.conn.lock().unwrap().execute("INSERT INTO commands(request_id,command_id,status,updated_at_unix_ms) VALUES('uncertain','command','unknown',0)",[]).unwrap();
        cache
            .apply_history_page(&page("c", "", "one", "fixture", true))
            .unwrap();
        assert!(cache
            .history_cursor(&SessionKey {
                host_id: "a".into(),
                session_id: "s".into()
            })
            .unwrap()
            .is_none());
        assert!(cache
            .history_cursor(&SessionKey {
                host_id: "b".into(),
                session_id: "s".into()
            })
            .unwrap()
            .is_some());
        let conn = cache.conn.lock().unwrap();
        let receipts: i64 = conn
            .query_row(
                "SELECT count(*) FROM history_receipts WHERE host_id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM commands WHERE request_id='uncertain'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 0);
        assert_eq!(status, "unknown");
    }

    #[test]
    fn oversized_replacement_rolls_back_cursor_and_discards_only_staging() {
        let mut cache = Cache::open_in_memory([1; 32]).unwrap();
        let key = SessionKey {
            host_id: "a".into(),
            session_id: "s".into(),
        };
        cache
            .apply_history_page(&page("a", "", "old", "original", true))
            .unwrap();
        cache.history_budget_bytes = 512;
        cache
            .apply_history_page(&page("a", "", "first", "small", false))
            .unwrap();
        assert!(matches!(
            cache.apply_history_page(&page("a", "first", "last", &"x".repeat(1024), true)),
            Err(CacheError::HistoryBudget)
        ));
        assert_eq!(cache.history_cursor(&key).unwrap().as_deref(), Some("old"));
        assert_eq!(cache.history_messages(&key).unwrap()[0].message_id, "old");
        let staged: i64 = cache
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM conversation_stage_messages",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(staged, 0);
        cache.history_budget_bytes = 0;
        cache.trim_history_cache(None).unwrap();
        assert!(cache.history_cursor(&key).unwrap().is_none());
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;
    use whalerein_proto::{
        content_block, ContentBlock, ConversationMessage as WireConversationMessage, HistoryBatch,
        MessageRole, TextContent,
    };

    fn text_message(id: &str) -> WireConversationMessage {
        WireConversationMessage {
            message_id: id.into(),
            role: MessageRole::Assistant as i32,
            blocks: vec![ContentBlock {
                block_id: "text".into(),
                content: Some(content_block::Content::Text(TextContent {
                    text: format!("text {id}"),
                })),
            }],
            ..Default::default()
        }
    }
    fn batch(from: &str, to: &str, ids: &[&str]) -> HistoryBatch {
        HistoryBatch {
            from_cursor: from.into(),
            to_cursor: to.into(),
            batch_id: format!("{from}:{to}"),
            projection_id: "projection-1".into(),
            changes: ids
                .iter()
                .map(|id| MessageUpsert {
                    message: Some(text_message(id)),
                    origin: None,
                })
                .collect(),
        }
    }
    fn page(batch: HistoryBatch, snapshot: bool) -> HistoryPage {
        let cut = batch.to_cursor.clone();
        HistoryPage {
            session: Some(whalerein_proto::SessionKey {
                host_id: "host".into(),
                session_id: "s".into(),
            }),
            batch: Some(batch),
            cut_cursor: cut,
            complete: true,
            snapshot,
            continuation: String::new(),
            projection_id: "projection-1".into(),
            source: whalerein_proto::HistorySource::Online as i32,
        }
    }

    #[test]
    fn gap_recovery_keeps_exactly_one_row_and_ordinal_per_message() {
        let cache = Cache::open_in_memory([1u8; 32]).unwrap();
        let key = SessionKey {
            host_id: "host".into(),
            session_id: "s".into(),
        };
        cache
            .apply_history_page(&page(batch("start", "cut-1", &["a"]), true))
            .unwrap();
        cache
            .apply_history_page(&page(batch("cut-1", "cut-2", &["b"]), false))
            .unwrap();
        // A reconnect/gap snapshot that overlaps live output must not create a
        // second row or reorder what was already committed.
        cache
            .apply_history_page(&page(batch("start", "cut-3", &["a", "b", "c"]), true))
            .unwrap();
        assert_eq!(
            cache.history_message_ids(&key).unwrap(),
            ["a", "b", "c"],
            "each committed identity appears once"
        );
        assert_eq!(cache.history_messages(&key).unwrap().len(), 3);
        // A late batch that repeats a known identity is still idempotent.
        cache
            .apply_history_page(&page(batch("cut-3", "cut-4", &["b"]), false))
            .unwrap();
        assert_eq!(cache.history_message_ids(&key).unwrap(), ["a", "b", "c"]);
        assert_eq!(
            cache.history_cursor(&key).unwrap().as_deref(),
            Some("cut-4")
        );
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use whalerein_proto::{
        content_block, ContentBlock, ConversationMessage as WireConversationMessage, HistoryBatch,
        MessageRole, TextContent,
    };

    fn key() -> SessionKey {
        SessionKey {
            host_id: "host".into(),
            session_id: "s".into(),
        }
    }

    fn text_message(id: &str) -> WireConversationMessage {
        WireConversationMessage {
            message_id: id.into(),
            role: MessageRole::Assistant as i32,
            blocks: vec![ContentBlock {
                block_id: "text".into(),
                content: Some(content_block::Content::Text(TextContent {
                    text: format!("body {id}"),
                })),
            }],
            ..Default::default()
        }
    }

    fn page(
        projection: &str,
        from: &str,
        to: &str,
        cut: &str,
        complete: bool,
        snapshot: bool,
        id: &str,
    ) -> HistoryPage {
        HistoryPage {
            session: Some((&key()).into()),
            batch: Some(HistoryBatch {
                from_cursor: from.into(),
                to_cursor: to.into(),
                batch_id: format!("{projection}:{from}:{to}"),
                projection_id: projection.into(),
                changes: vec![MessageUpsert {
                    message: Some(text_message(id)),
                    origin: None,
                }],
            }),
            cut_cursor: cut.into(),
            complete,
            snapshot,
            continuation: String::new(),
            projection_id: projection.into(),
            source: whalerein_proto::HistorySource::Online as i32,
        }
    }

    #[test]
    fn version_two_cache_migrates_additively_and_fences_until_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache_key = [9u8; 32];
        let tag = hex::encode(&crate::util::sha256(&cache_key)[..16]);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO meta VALUES('schema_version','2');
             INSERT INTO meta VALUES('cache_key_tag','{tag}');
             CREATE TABLE commands(request_id TEXT PRIMARY KEY, command_id TEXT NOT NULL, status TEXT NOT NULL, error_code TEXT, updated_at_unix_ms INTEGER NOT NULL);
             INSERT INTO commands VALUES('legacy','old-command','unknown',NULL,1);
             CREATE TABLE conversation_views(host_id TEXT NOT NULL, session_id TEXT NOT NULL, cursor TEXT NOT NULL, PRIMARY KEY(host_id,session_id));
             INSERT INTO conversation_views VALUES('host','s','old-cut');
             CREATE TABLE conversation_messages(host_id TEXT NOT NULL, session_id TEXT NOT NULL, message_id TEXT NOT NULL, ordinal INTEGER NOT NULL, payload BLOB NOT NULL, PRIMARY KEY(host_id,session_id,message_id));
             INSERT INTO conversation_messages VALUES('host','s','old-message',0,x'0102');
             CREATE TABLE history_receipts(host_id TEXT NOT NULL, session_id TEXT NOT NULL, batch_id TEXT NOT NULL, digest BLOB NOT NULL, applied_at INTEGER NOT NULL, PRIMARY KEY(host_id,session_id,batch_id));
             CREATE TABLE conversation_stages(host_id TEXT NOT NULL, session_id TEXT NOT NULL, cut TEXT NOT NULL, cursor TEXT NOT NULL, PRIMARY KEY(host_id,session_id));
             CREATE TABLE conversation_stage_messages(host_id TEXT NOT NULL, session_id TEXT NOT NULL, message_id TEXT NOT NULL, ordinal INTEGER NOT NULL, payload BLOB NOT NULL, PRIMARY KEY(host_id,session_id,message_id));"
        )).unwrap();
        drop(conn);

        let cache = Cache::open(&path, cache_key).unwrap();
        let migrated = cache.history_view(&key()).unwrap().unwrap();
        assert_eq!(migrated.cursor, "old-cut");
        assert_eq!(migrated.projection, "");
        assert_eq!(migrated.generation, 0);
        assert!(cache.get_command("legacy").unwrap().is_some());
        let old_generation = cache.cache_generation().unwrap();

        // The old view remains readable only by its own (empty) projection. A
        // replacement stages under the new projection without touching it.
        cache
            .apply_history_page(&page(
                "projection-2",
                "start",
                "middle",
                "new-cut",
                false,
                true,
                "new-1",
            ))
            .unwrap();
        assert_eq!(
            cache.history_view(&key()).unwrap().unwrap().cursor,
            "old-cut"
        );
        cache
            .apply_history_page(&page(
                "projection-2",
                "middle",
                "new-cut",
                "new-cut",
                true,
                true,
                "new-2",
            ))
            .unwrap();
        let replaced = cache.history_view(&key()).unwrap().unwrap();
        assert_eq!(replaced.cursor, "new-cut");
        assert_eq!(replaced.projection, "projection-2");
        assert_eq!(replaced.generation, 1);
        assert!(cache.cache_generation().unwrap() > old_generation);
        assert_eq!(
            cache
                .history_message_ids(&key())
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            ["new-1", "new-2"]
        );
        assert!(cache.get_command("legacy").unwrap().is_some());
        cache.clear().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn old_projection_deltas_and_receipts_are_fenced_without_advancing_the_view() {
        let cache = Cache::open_in_memory([1; 32]).unwrap();
        cache
            .apply_history_page(&page(
                "projection-1",
                "start",
                "cut-1",
                "cut-1",
                true,
                true,
                "one",
            ))
            .unwrap();
        let before = cache.history_view(&key()).unwrap().unwrap();
        let mismatch = page(
            "projection-2",
            "cut-1",
            "cut-2",
            "cut-2",
            true,
            false,
            "two",
        );
        assert!(matches!(
            cache.apply_history_page(&mismatch),
            Err(CacheError::ProjectionMismatch)
        ));
        assert_eq!(cache.history_view(&key()).unwrap().unwrap(), before);
        assert!(matches!(
            cache.apply_history_page(&page(
                "projection-2",
                "cut-1",
                "cut-2",
                "cut-2",
                true,
                false,
                "two"
            )),
            Err(CacheError::ProjectionMismatch)
        ));
        // A staged replacement with a changed cut is rejected and discarded;
        // the previous complete projection stays selected.
        assert!(cache
            .apply_history_page(&page(
                "projection-2",
                "start",
                "middle",
                "cut-new",
                false,
                true,
                "staged"
            ))
            .unwrap()
            .eq(&false));
        assert!(cache
            .apply_history_page(&page(
                "projection-2",
                "wrong",
                "cut-new",
                "cut-new",
                true,
                true,
                "staged-2"
            ))
            .is_err());
        assert_eq!(
            cache.history_view(&key()).unwrap().unwrap().projection,
            "projection-1"
        );
        let staged: i64 = cache
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM conversation_stage_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(staged, 1);
    }

    #[test]
    fn a_staged_snapshot_surviving_restart_never_replaces_the_old_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = Cache::open(&path, [4; 32]).unwrap();
        cache
            .apply_history_page(&page(
                "projection-1",
                "start",
                "old-cut",
                "old-cut",
                true,
                true,
                "old",
            ))
            .unwrap();
        cache
            .apply_history_page(&page(
                "projection-2",
                "start",
                "middle",
                "new-cut",
                false,
                true,
                "new",
            ))
            .unwrap();
        drop(cache);
        let reopened = Cache::open(&path, [4; 32]).unwrap();
        assert_eq!(
            reopened.history_view(&key()).unwrap().unwrap().projection,
            "projection-1"
        );
        assert_eq!(
            reopened.history_message_ids(&key()).unwrap(),
            vec!["old".to_owned()]
        );
        reopened.discard_history_stage(&key()).unwrap();
        reopened.clear().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
