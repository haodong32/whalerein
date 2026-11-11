//! The only writer of committed conversation views. Native source positions
//! remain opaque; previews are handled by the in-memory conversation reducer.
use crate::api::{CoreError, CoreResult, HostInfo, SessionSummary};
use crate::cache::Cache;
use crate::security::{EventAad, SecureChannel};
use prost::Message;
use std::sync::Arc;
use whalerein_proto::{
    EventEnvelope, EventPayload, HistoryBatch, HistoryPage, MessageUpsert, SessionKey,
    SessionUpdate,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Idle,
    Syncing,
    Synced,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOutcome {
    Applied,
    Duplicate,
    Gap,
}
pub struct SyncService {
    cache: Arc<Cache>,
}
impl SyncService {
    pub fn new(cache: Arc<Cache>) -> Self {
        Self { cache }
    }
    pub fn store_hosts(&self, hosts: &[HostInfo]) -> CoreResult<()> {
        Ok(self.cache.put_hosts(hosts)?)
    }
    pub fn store_sessions(
        &self,
        host: &str,
        sessions: &[SessionSummary],
        source: &str,
        complete: bool,
    ) -> CoreResult<()> {
        Ok(self.cache.put_sessions(host, sessions, source, complete)?)
    }
    pub fn load_sessions(&self, host: &str) -> CoreResult<Vec<SessionSummary>> {
        Ok(self.cache.load_sessions(host)?)
    }
    pub fn cursor(&self, key: &SessionKey) -> CoreResult<Option<String>> {
        Ok(self.cache.history_cursor(key)?)
    }
    pub fn discard_stage(&self, key: &SessionKey) -> CoreResult<()> {
        Ok(self.cache.discard_history_stage(key)?)
    }
    pub fn next_message(
        &self,
        key: &SessionKey,
        after: i64,
    ) -> CoreResult<Option<(i64, MessageUpsert)>> {
        Ok(self.cache.next_history_message(key, after)?)
    }
    pub fn apply_page(&self, key: &SessionKey, page: &HistoryPage) -> CoreResult<bool> {
        if page.session.as_ref() != Some(key)
            || page.cut_cursor.is_empty()
            || page.cut_cursor.len() > 4096
        {
            return Err(CoreError::Security("history scope or cut mismatch".into()));
        }
        validate_batch(
            page.batch
                .as_ref()
                .ok_or_else(|| CoreError::Security("missing history batch".into()))?,
        )?;
        Ok(self.cache.apply_history_page(page)?)
    }
    pub fn apply_batch(&self, key: &SessionKey, batch: &HistoryBatch) -> CoreResult<BatchOutcome> {
        validate_batch(batch)?;
        match self.cache.apply_history_batch(key, batch) {
            Ok(true) => Ok(BatchOutcome::Applied),
            Ok(false) => Ok(BatchOutcome::Duplicate),
            Err(crate::cache::CacheError::HistoryGap) => Ok(BatchOutcome::Gap),
            Err(error) => Err(error.into()),
        }
    }
    pub fn open_update(
        &self,
        channel: &SecureChannel,
        envelope: &EventEnvelope,
    ) -> CoreResult<SessionUpdate> {
        if envelope.device_id != channel.device_id()
            || envelope.host_id != channel.host_id()
            || envelope.channel_id != channel.channel_id()
        {
            return Err(CoreError::ChannelMismatch);
        }
        let aad = EventAad {
            host_id: &envelope.host_id,
            device_id: &envelope.device_id,
            channel_id: &envelope.channel_id,
            created_at_unix_ms: envelope.created_at_unix_ms,
        };
        let frame = whalerein_proto::EncryptedFrame {
            channel_id: envelope.channel_id.clone(),
            nonce: envelope.nonce.clone(),
            ciphertext: envelope.ciphertext.clone(),
        };
        let bytes = channel.open_event(&aad, &frame)?;
        let payload = EventPayload::decode(bytes.as_slice())
            .map_err(|_| CoreError::Security("invalid typed payload".into()))?;
        let update = payload
            .update
            .ok_or_else(|| CoreError::Security("missing typed update".into()))?;
        if update
            .session
            .as_ref()
            .is_none_or(|key| key.host_id != channel.host_id() || key.session_id.is_empty())
            || update.payload.is_none()
        {
            return Err(CoreError::Security("invalid typed update scope".into()));
        }
        Ok(update)
    }
}
fn validate_batch(batch: &HistoryBatch) -> CoreResult<()> {
    if batch.from_cursor.is_empty()
        || batch.to_cursor.is_empty()
        || batch.from_cursor.len() > 4096
        || batch.to_cursor.len() > 4096
        || batch.batch_id.is_empty()
        || batch.batch_id.len() > 128
        || batch.changes.len() > 1000
        || (batch.from_cursor == batch.to_cursor && !batch.changes.is_empty())
    {
        return Err(CoreError::Security("invalid history interval".into()));
    }
    for change in &batch.changes {
        let message = change
            .message
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing message".into()))?;
        if message.message_id.is_empty()
            || message.message_id.len() > 512
            || message.blocks.len() > 1000
            || whalerein_proto::MessageRole::try_from(message.role).is_err()
            || message.role == 0
            || message
                .blocks
                .iter()
                .any(|block| block.block_id.is_empty() || block.content.is_none())
        {
            return Err(CoreError::Security("invalid normalized message".into()));
        }
    }
    Ok(())
}
