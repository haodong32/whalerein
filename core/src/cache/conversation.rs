//! Host-scoped, encrypted conversation views. Snapshot staging never replaces
//! a complete view until its final page commits.
use super::*;
use prost::Message;
use whalerein_proto::{ConversationMessage, HistoryPage, MessageUpsert, SessionKey};

fn aad(key: &SessionKey, id: &str) -> Vec<u8> {
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
        let excluded = protected.cloned().unwrap_or_default();
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
    }
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
    pub fn initialize_conversations(&self) -> Result<(), CacheError> {
        self.conn.lock().expect("cache mutex").execute_batch("CREATE TABLE IF NOT EXISTS conversation_views (
          host_id TEXT NOT NULL, session_id TEXT NOT NULL, cursor TEXT NOT NULL,
          PRIMARY KEY(host_id,session_id));
          CREATE TABLE IF NOT EXISTS conversation_messages (
          host_id TEXT NOT NULL, session_id TEXT NOT NULL, message_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL, payload BLOB NOT NULL, PRIMARY KEY(host_id,session_id,message_id));
          CREATE TABLE IF NOT EXISTS history_receipts (
          host_id TEXT NOT NULL, session_id TEXT NOT NULL, batch_id TEXT NOT NULL,
          digest BLOB NOT NULL, applied_at INTEGER NOT NULL, PRIMARY KEY(host_id,session_id,batch_id));
          CREATE TABLE IF NOT EXISTS conversation_stages (
          host_id TEXT NOT NULL, session_id TEXT NOT NULL, cut TEXT NOT NULL, cursor TEXT NOT NULL,
          PRIMARY KEY(host_id,session_id));
          CREATE TABLE IF NOT EXISTS conversation_stage_messages (
          host_id TEXT NOT NULL, session_id TEXT NOT NULL, message_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL, payload BLOB NOT NULL, PRIMARY KEY(host_id,session_id,message_id));")?;
        Ok(())
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
    pub fn history_messages(
        &self,
        key: &SessionKey,
    ) -> Result<Vec<ConversationMessage>, CacheError> {
        let conn = self.conn.lock().expect("cache mutex");
        let mut statement=conn.prepare("SELECT message_id,payload FROM conversation_messages WHERE host_id=?1 AND session_id=?2 ORDER BY ordinal")?;
        let rows = statement.query_map(params![key.host_id, key.session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (id, blob) = row?;
            let bytes = self.open_blob(&aad(key, &id), &blob)?;
            messages.push(
                MessageUpsert::decode(bytes.as_slice())
                    .map_err(|_| CacheError::Corrupt)?
                    .message
                    .ok_or(CacheError::Corrupt)?,
            );
        }
        Ok(messages)
    }
    pub fn next_history_message(
        &self,
        key: &SessionKey,
        after: i64,
    ) -> Result<Option<(i64, MessageUpsert)>, CacheError> {
        let row:Option<(i64,String,Vec<u8>)>=self.conn.lock().expect("cache mutex").query_row(
            "SELECT ordinal,message_id,payload FROM conversation_messages WHERE host_id=?1 AND session_id=?2 AND ordinal>?3 ORDER BY ordinal LIMIT 1",
            params![key.host_id,key.session_id,after],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
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
    pub fn apply_history_page(&self, page: &HistoryPage) -> Result<bool, CacheError> {
        let key = page.session.as_ref().ok_or(CacheError::Corrupt)?;
        let batch = page.batch.as_ref().ok_or(CacheError::Corrupt)?;
        if key.host_id.is_empty()
            || key.session_id.is_empty()
            || batch.to_cursor.is_empty()
            || page.cut_cursor.is_empty()
        {
            return Err(CacheError::Corrupt);
        }
        let mut canonical = batch.clone();
        for change in &mut canonical.changes {
            change.origin = None;
        }
        let digest = crate::util::sha256(&canonical.encode_to_vec());
        let receipt_aad = serde_json::to_vec(&[
            "history-receipt",
            key.host_id.as_str(),
            key.session_id.as_str(),
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
                    self.seal(&aad(key, &message.message_id), &change.encode_to_vec())?,
                ))
            })
            .collect::<Result<_, CacheError>>()?;
        let mut conn = self.conn.lock().expect("cache mutex");
        let tx = conn.transaction()?;
        let known:Option<Vec<u8>>=tx.query_row("SELECT digest FROM history_receipts WHERE host_id=?1 AND session_id=?2 AND batch_id=?3",params![key.host_id,key.session_id,batch.batch_id],|row|row.get(0)).optional()?;
        if let Some(known) = known {
            if self.open_blob(&receipt_aad, &known)? != digest {
                return Err(CacheError::Corrupt);
            }
        }
        let table = if page.snapshot {
            "conversation_stage_messages"
        } else {
            "conversation_messages"
        };
        if page.snapshot {
            let stage: Option<(String, String)> = tx
                .query_row(
                    "SELECT cut,cursor FROM conversation_stages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((cut, cursor)) = stage {
                if cut != page.cut_cursor || cursor != batch.from_cursor {
                    return Err(CacheError::Corrupt);
                }
            } else {
                tx.execute(
                    "DELETE FROM conversation_stage_messages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
            }
        } else {
            let cursor: Option<String> = tx
                .query_row(
                    "SELECT cursor FROM conversation_views WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                    |row| row.get(0),
                )
                .optional()?;
            if cursor.as_deref() == Some(batch.to_cursor.as_str()) {
                return Ok(false);
            }
            if cursor.as_deref() != Some(batch.from_cursor.as_str()) {
                return Err(CacheError::HistoryGap);
            }
        }
        let ordinal_start:i64=tx.query_row(&format!("SELECT COALESCE(MAX(ordinal),-1)+1 FROM {table} WHERE host_id=?1 AND session_id=?2"),params![key.host_id,key.session_id],|row|row.get(0))?;
        for (index, (id, payload)) in sealed.into_iter().enumerate() {
            let ordinal = ordinal_start + index as i64;
            tx.execute(&format!("INSERT INTO {table}(host_id,session_id,message_id,ordinal,payload) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(host_id,session_id,message_id) DO UPDATE SET payload=excluded.payload"),params![key.host_id,key.session_id,id,ordinal,payload])?;
        }
        if page.snapshot && !page.complete {
            tx.execute("INSERT INTO conversation_stages VALUES(?1,?2,?3,?4) ON CONFLICT(host_id,session_id) DO UPDATE SET cut=excluded.cut,cursor=excluded.cursor",params![key.host_id,key.session_id,page.cut_cursor,batch.to_cursor])?;
        } else {
            if page.snapshot {
                tx.execute(
                    "DELETE FROM conversation_messages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
                tx.execute("INSERT INTO conversation_messages SELECT * FROM conversation_stage_messages WHERE host_id=?1 AND session_id=?2",params![key.host_id,key.session_id])?;
                tx.execute(
                    "DELETE FROM conversation_stage_messages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
                tx.execute(
                    "DELETE FROM conversation_stages WHERE host_id=?1 AND session_id=?2",
                    params![key.host_id, key.session_id],
                )?;
            }
            tx.execute("INSERT INTO conversation_views VALUES(?1,?2,?3) ON CONFLICT(host_id,session_id) DO UPDATE SET cursor=excluded.cursor",params![key.host_id,key.session_id,batch.to_cursor])?;
        }
        tx.execute("INSERT INTO history_receipts VALUES(?1,?2,?3,?4,?5) ON CONFLICT(host_id,session_id,batch_id) DO NOTHING",params![key.host_id,key.session_id,batch.batch_id,protected_digest,crate::util::now_unix_ms()])?;
        tx.execute("DELETE FROM history_receipts WHERE host_id=?1 AND session_id=?2 AND batch_id NOT IN (SELECT batch_id FROM history_receipts WHERE host_id=?1 AND session_id=?2 ORDER BY applied_at DESC,rowid DESC LIMIT 512)",params![key.host_id,key.session_id])?;
        if let Err(error) = trim_history(&tx, Some(key), self.history_budget_bytes) {
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
            session: Some(key.clone()),
            batch: Some(batch.clone()),
            cut_cursor: batch.to_cursor.clone(),
            complete: true,
            snapshot: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_proto::{content_block, ContentBlock, HistoryBatch, MessageRole, TextContent};
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
            session: Some(key(host)),
            batch: Some(HistoryBatch {
                from_cursor: from.into(),
                to_cursor: to.into(),
                batch_id: format!("{from}:{to}"),
                changes: if id.is_empty() {
                    vec![]
                } else {
                    vec![MessageUpsert {
                        message: Some(ConversationMessage {
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
        original.session = Some(parent.clone());
        cache.apply_history_page(&original).unwrap();
        let mut inherited = original.clone();
        inherited.session = Some(child.clone());
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
        child_delta.session = Some(child.clone());
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
    use whalerein_proto::{content_block, ContentBlock, HistoryBatch, MessageRole, TextContent};

    fn page(host: &str, from: &str, to: &str, text: &str, complete: bool) -> HistoryPage {
        HistoryPage {
            session: Some(SessionKey {
                host_id: host.into(),
                session_id: "s".into(),
            }),
            batch: Some(HistoryBatch {
                from_cursor: from.into(),
                to_cursor: to.into(),
                batch_id: format!("{from}:{to}"),
                changes: vec![MessageUpsert {
                    message: Some(ConversationMessage {
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
