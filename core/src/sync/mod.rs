//! The only writer of committed conversation views. Native source positions
//! remain opaque; previews are handled by the in-memory conversation reducer.
use crate::api::{CoreError, CoreResult, HostInfo, SessionSummary};
use crate::cache::Cache;
use crate::security::{EventAad, SecureChannel};
use crate::session::SessionKey;
use prost::Message;
use std::sync::Arc;
use whalerein_proto::{
    EventEnvelope, EventPayload, HistoryBatch, HistoryPage, MessageUpsert, SessionUpdate,
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
    pub(crate) fn history_view(
        &self,
        key: &SessionKey,
    ) -> CoreResult<Option<crate::cache::HistoryView>> {
        Ok(self.cache.history_view(key)?)
    }
    pub fn history_message_ids(&self, key: &SessionKey) -> CoreResult<Vec<String>> {
        Ok(self.cache.history_message_ids(key)?)
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
        if page.session.as_ref() != Some(&key.into())
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
            Err(crate::cache::CacheError::HistoryGap)
            | Err(crate::cache::CacheError::ProjectionMismatch) => Ok(BatchOutcome::Gap),
            Err(error) => Err(error.into()),
        }
    }
}

/// One decrypted event is either a content update or a Host-authored terminal
/// close for one observation.
pub(crate) enum OpenedEvent {
    Update(SessionUpdate),
    Closed(whalerein_proto::WatchClosed),
}

/// Decrypt and scope-check one typed event without touching the cache.
///
/// Question-only observation uses this path, so a missing or failing local
/// history cache cannot hide questions.
pub(crate) fn open_event(
    channel: &SecureChannel,
    envelope: &EventEnvelope,
) -> CoreResult<OpenedEvent> {
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
    if let Some(closed) = payload.watch_closed {
        if closed.watch_id.is_empty()
            || closed.reason.is_empty()
            || closed
                .session
                .as_ref()
                .is_none_or(|key| key.host_id != channel.host_id() || key.session_id.is_empty())
        {
            return Err(CoreError::Security("invalid watch close scope".into()));
        }
        return Ok(OpenedEvent::Closed(closed));
    }
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
    Ok(OpenedEvent::Update(update))
}

pub(crate) fn validate_batch(batch: &HistoryBatch) -> CoreResult<()> {
    if batch.from_cursor.is_empty()
        || batch.to_cursor.is_empty()
        || batch.from_cursor.len() > 4096
        || batch.to_cursor.len() > 4096
        || batch.batch_id.is_empty()
        || batch.batch_id.len() > 128
        || batch.projection_id.is_empty()
        || batch.projection_id.len() > 128
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
        validate_message(message)?;
    }
    Ok(())
}

/// Shared canonical-message validation used by sync and independent history.
pub(crate) fn validate_message(message: &whalerein_proto::ConversationMessage) -> CoreResult<()> {
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
    for block in &message.blocks {
        if let Some(whalerein_proto::content_block::Content::ToolActivity(activity)) =
            &block.content
        {
            if activity.call_id.is_empty() || activity.call_id.len() > 128 {
                return Err(CoreError::Security("invalid tool activity identity".into()));
            }
        }
    }
    Ok(())
}
