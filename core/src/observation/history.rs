//! Recent-window ownership for a paged conversation observation.
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use prost::Message;
use whalerein_proto::{
    request_payload, response_payload, HistoryBatch, HistoryPage as WirePage, HistoryRead,
    HistorySource, MessageUpsert, RequestPayload,
};

use crate::api::core::watch::UpdateSink;
use crate::api::{
    Core, CoreError, CoreResult, HistoryContinuity, HistorySnapshot, SessionEventPayload,
};
use crate::conversation::Conversation;
use crate::history::{bounded, HistoryPage, HistoryPageRequest};
use crate::session::SessionKey;
use crate::sync::validate_batch;

use super::EventSink;

const REPAIR_PAGES: usize = 8;
const REPAIR_DEADLINE: Duration = Duration::from_secs(8);
const SEEN_POSITIONS: usize = 256;
const PENDING_BATCHES: usize = 256;
const REPAIR_MESSAGES: u32 = 100;
const REPAIR_BYTES: u32 = 256 * 1024;

#[derive(Clone)]
struct Position {
    projection: String,
    cursor: String,
}

pub(super) type HistoryReadFuture =
    Pin<Box<dyn Future<Output = CoreResult<PreparedHistory>> + Send>>;

pub(super) struct PreparedHistory {
    repair: Option<Vec<HistoryBatch>>,
    baseline: Option<(HistoryPage, WirePage)>,
}

/// Decrypted history held while the baseline query is pending. Use the
/// existing control-stream byte budget and a separate batch count bound.
#[derive(Default)]
struct PendingHistory {
    batches: Vec<HistoryBatch>,
    bytes: usize,
}

impl PendingHistory {
    fn push(&mut self, batch: HistoryBatch, byte_budget: usize) -> CoreResult<bool> {
        validate_batch(&batch)?;
        let bytes = self.bytes.saturating_add(batch.encoded_len());
        if bytes > byte_budget || self.batches.len() >= PENDING_BATCHES {
            return Ok(false);
        }
        self.bytes = bytes;
        self.batches.push(batch);
        Ok(true)
    }
}

/// The observation's forward position is independent of the reverse cut
/// returned in its history snapshot and the full synchronization cache.
pub(super) struct PagedHistory {
    request: HistoryPageRequest,
    position: Option<Position>,
    recent: Option<WirePage>,
    established: bool,
    invalidated: bool,
    unavailable: Option<String>,
    pending: Option<PendingHistory>,
    seen: VecDeque<String>,
}

impl PagedHistory {
    pub(super) fn new(key: &SessionKey, (count, bytes): (u32, u32)) -> Self {
        let mut request = HistoryPageRequest::new(key.clone());
        (request.max_messages, request.max_bytes) = bounded(count, bytes);
        Self {
            request,
            position: None,
            recent: None,
            established: false,
            invalidated: false,
            unavailable: None,
            pending: None,
            seen: VecDeque::new(),
        }
    }

    pub(super) async fn publish_cache(
        &mut self,
        core: &Core,
        conversation: &mut Conversation,
        sink: &mut EventSink<'_>,
    ) -> CoreResult<()> {
        let cache = core.cache()?;
        if let Some(recent) = cache.recent_history(&self.request.session)? {
            // When both records certify the same head, the complete view can
            // offer a local older continuation. Otherwise recent coverage has
            // its own cut and cannot be joined to an unrelated prefix.
            let same_full_head = cache
                .history_view(&self.request.session)?
                .is_some_and(|view| {
                    view.projection == recent.projection_id && view.cursor == recent.cut_cursor
                });
            if same_full_head {
                let (page, cursor) = core.full_cache_window(&self.request)?;
                if cursor.as_deref() == Some(&recent.cut_cursor)
                    && page.projection == recent.projection_id
                {
                    self.position = Some(Position {
                        projection: page.projection.clone(),
                        cursor: recent.cut_cursor.clone(),
                    });
                    self.recent = Some(recent);
                    self.emit_cached_page(page, conversation, sink).await;
                    return Ok(());
                }
            }
            let page = core.cached_recent_page(&self.request, &recent)?;
            // An incomplete recent-only cache row with no visible content
            // cannot establish readiness. A certified full view may still
            // supply a usable cache baseline.
            if !page.messages.is_empty() || page.complete {
                self.position = Some(Position {
                    projection: recent.projection_id.clone(),
                    cursor: recent.cut_cursor.clone(),
                });
                self.recent = Some(recent);
                self.emit_cached_page(page, conversation, sink).await;
                return Ok(());
            }
        }
        let (page, cursor) = core.full_cache_window(&self.request)?;
        if let Some(cursor) = cursor {
            self.position = Some(Position {
                projection: page.projection.clone(),
                cursor,
            });
            self.emit_cached_page(page, conversation, sink).await;
        }
        Ok(())
    }

    async fn emit_cached_page(
        &self,
        page: HistoryPage,
        conversation: &mut Conversation,
        sink: &mut EventSink<'_>,
    ) {
        for row in &page.messages {
            conversation.mark_delivered_id(&row.message_id);
        }
        sink.publish(SessionEventPayload::HistorySnapshot(HistorySnapshot {
            page,
            continuity: HistoryContinuity::Replacement,
        }))
        .await;
    }

    pub(super) fn prepare(
        &mut self,
        core: &Core,
        projection: &str,
        head: &str,
    ) -> HistoryReadFuture {
        self.pending = Some(PendingHistory::default());
        let core = core.clone();
        let request = self.request.clone();
        let position = self
            .position
            .as_ref()
            .filter(|pos| !self.invalidated && pos.projection == projection)
            .cloned();
        let needs_snapshot = !self.established || self.unavailable.is_some();
        let projection = projection.to_owned();
        let head = head.to_owned();
        Box::pin(async move {
            // Collect a fixed-cut repair before delivering any part of it.
            // Partial repair cannot certify adjacency to the recent page.
            let repair = match position {
                Some(position) => {
                    core.repair_recent(&request.session, &position, &head)
                        .await?
                }
                None => None,
            };
            let baseline = if !needs_snapshot && repair.is_some() {
                None
            } else {
                Some(core.history_page_at(&request, &projection, &head).await?)
            };
            Ok(PreparedHistory { repair, baseline })
        })
    }

    pub(super) async fn install(
        &mut self,
        core: &Core,
        conversation: &mut Conversation,
        prepared: PreparedHistory,
        sink: &mut EventSink<'_>,
    ) -> CoreResult<()> {
        let PreparedHistory { repair, baseline } = prepared;
        let Some((page, recent)) = baseline else {
            self.unavailable = None;
            return self
                .apply_repair(core, conversation, repair.expect("prepared repair"), sink)
                .await;
        };
        let replaced = repair.is_none();
        if let Some(repair) = repair {
            self.apply_repair(core, conversation, repair, sink).await?;
        }
        if replaced {
            conversation.clear_history_projection();
            self.seen.clear();
        }
        for row in &page.messages {
            conversation.mark_delivered_id(&row.message_id);
        }
        self.position = Some(Position {
            projection: recent.projection_id.clone(),
            cursor: recent.cut_cursor.clone(),
        });
        self.recent = Some(recent);
        self.established = true;
        self.invalidated = false;
        self.unavailable = None;
        self.persist(core);
        sink.publish(SessionEventPayload::HistorySnapshot(HistorySnapshot {
            page,
            continuity: if replaced {
                HistoryContinuity::Replacement
            } else {
                HistoryContinuity::Continuous
            },
        }))
        .await;
        Ok(())
    }

    pub(super) fn finish_read(&mut self) -> Vec<HistoryBatch> {
        std::mem::take(&mut self.pending)
            .map(|pending| pending.batches)
            .unwrap_or_default()
    }

    pub(super) fn invalidate(&mut self) {
        self.invalidated = true;
    }

    pub(super) async fn transport_unavailable(&mut self, code: &str, sink: &mut EventSink<'_>) {
        // Readable history survives a transport interruption. Connection
        // state reports the interruption while coverage is recovered.
        if !self.established {
            self.unavailable(code, sink).await;
        }
    }

    pub(super) async fn unavailable(&mut self, code: &str, sink: &mut EventSink<'_>) {
        if self.unavailable.as_deref() != Some(code) {
            self.unavailable = Some(code.into());
            sink.publish(SessionEventPayload::HistoryUnavailable { code: code.into() })
                .await;
        }
    }

    /// Apply a validated contiguous batch. An unknown predecessor may be a
    /// pre-cut event queued during attach or a genuine gap. Forward repair
    /// against an authoritative cut handles both without comparing tokens.
    pub(super) async fn receive(
        &mut self,
        core: &Core,
        conversation: &mut Conversation,
        batch: HistoryBatch,
        byte_budget: usize,
        sink: &mut EventSink<'_>,
    ) -> CoreResult<bool> {
        if let Some(pending) = self.pending.as_mut() {
            return pending.push(batch, byte_budget);
        }
        validate_batch(&batch)?;
        if !self.established {
            return Ok(true);
        }
        let position = self.position.as_ref().expect("established paged history");
        if position.projection != batch.projection_id {
            return Ok(false);
        }
        if position.cursor == batch.to_cursor || self.seen.contains(&batch.to_cursor) {
            return Ok(true);
        }
        if position.cursor == batch.from_cursor {
            self.apply(core, conversation, batch, sink).await?;
            return Ok(true);
        }
        let Some(repair) = core
            .repair_recent(&self.request.session, position, "")
            .await?
        else {
            return Ok(false);
        };
        self.apply_repair(core, conversation, repair, sink).await?;
        Ok(true)
    }

    async fn apply_repair(
        &mut self,
        core: &Core,
        conversation: &mut Conversation,
        batches: Vec<HistoryBatch>,
        sink: &mut EventSink<'_>,
    ) -> CoreResult<()> {
        for batch in batches {
            self.apply(core, conversation, batch, sink).await?;
            if sink.failed {
                break;
            }
        }
        Ok(())
    }

    async fn apply(
        &mut self,
        core: &Core,
        conversation: &mut Conversation,
        batch: HistoryBatch,
        sink: &mut EventSink<'_>,
    ) -> CoreResult<()> {
        let position = self.position.as_mut().expect("validated prior position");
        self.seen.push_back(std::mem::replace(
            &mut position.cursor,
            batch.to_cursor.clone(),
        ));
        if self.seen.len() > SEEN_POSITIONS {
            self.seen.pop_front();
        }
        if let Some(recent) = self.recent.as_mut() {
            fold_recent(
                recent,
                &batch,
                self.request.max_messages,
                self.request.max_bytes,
            );
            self.persist(core);
        }
        for change in &batch.changes {
            if let Some(update) = conversation.committed_once(change)? {
                if !sink.emit(&update).await {
                    break;
                }
            }
            for activity in conversation.take_activity_updates() {
                if !sink.emit(&activity).await {
                    break;
                }
            }
        }
        Ok(())
    }

    fn persist(&self, core: &Core) {
        if let Err(error) = core.cache().and_then(|cache| {
            cache
                .store_recent_history(
                    &self.request.session,
                    self.recent.as_ref().expect("recent range"),
                )
                .map_err(Into::into)
        }) {
            core.diagnostics
                .record_stage("core", "recent_history_cache", "failed", error.code());
        }
    }
}

fn fold_recent(recent: &mut WirePage, incoming: &HistoryBatch, count: u32, budget: u32) {
    let batch = recent.batch.as_mut().expect("validated recent range");
    for change in &incoming.changes {
        let id = message_id(change);
        if let Some(index) = batch
            .changes
            .iter()
            .position(|existing| message_id(existing) == id)
        {
            batch.changes[index] = change.clone();
        } else {
            batch.changes.push(change.clone());
        }
    }
    let mut bytes = 0;
    let keep = batch
        .changes
        .iter()
        .rev()
        .take(count as usize)
        .take_while(|change| {
            bytes += change.encoded_len();
            bytes <= budget as usize
        })
        .count();
    if keep < batch.changes.len() {
        batch.changes.drain(..batch.changes.len() - keep);
        recent.complete = false;
        // The retained suffix is now shorter than its original reverse page.
        // Its updated lower bound is internal and never used with a Source.
        batch.from_cursor = batch.changes.first().map_or_else(
            || incoming.to_cursor.clone(),
            |change| format!("retained:{}", message_id(change)),
        );
    }
    batch.to_cursor = incoming.to_cursor.clone();
    recent.cut_cursor = incoming.to_cursor.clone();
    recent.continuation.clear();
}

fn message_id(change: &MessageUpsert) -> &str {
    &change
        .message
        .as_ref()
        .expect("validated history message")
        .message_id
}

impl Core {
    async fn repair_recent(
        &self,
        key: &SessionKey,
        position: &Position,
        cut: &str,
    ) -> CoreResult<Option<Vec<HistoryBatch>>> {
        if position.cursor == cut {
            return Ok(Some(Vec::new()));
        }
        match tokio::time::timeout(
            REPAIR_DEADLINE,
            self.repair_recent_range(key, position, cut),
        )
        .await
        {
            Ok(Ok(repair)) => Ok(repair),
            Ok(Err(error)) if !super::watch_fatal(&error) => Ok(None),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(None),
        }
    }

    async fn repair_recent_range(
        &self,
        key: &SessionKey,
        position: &Position,
        cut: &str,
    ) -> CoreResult<Option<Vec<HistoryBatch>>> {
        let mut batches = Vec::new();
        let mut after = position.cursor.clone();
        let mut cut = cut.to_owned();
        let mut seen = HashSet::from([after.clone()]);
        let mut bytes = 0;
        for _ in 0..REPAIR_PAGES {
            let response = self
                .query(
                    &key.host_id,
                    RequestPayload {
                        payload: Some(request_payload::Payload::ReadHistory(HistoryRead {
                            session_id: key.session_id.clone(),
                            snapshot: false,
                            after_cursor: after.clone(),
                            cut_cursor: cut.clone(),
                            max_bytes: REPAIR_BYTES,
                            max_messages: REPAIR_MESSAGES,
                            source: HistorySource::Online as i32,
                            ..Default::default()
                        })),
                    },
                )
                .await?;
            let Some(response_payload::Payload::HistoryPage(page)) = response.payload else {
                return Err(CoreError::Security("unexpected history repair".into()));
            };
            let Some(batch) = page.batch else {
                return Err(CoreError::Security(
                    "missing history repair interval".into(),
                ));
            };
            if page.session.as_ref() != Some(&whalerein_proto::SessionKey::from(key))
                || page.snapshot
                || page.source != HistorySource::Online as i32
            {
                return Err(CoreError::Security("history repair scope mismatch".into()));
            }
            if page.projection_id != position.projection
                || batch.projection_id != position.projection
                || batch.from_cursor != after
                || page.cut_cursor.is_empty()
                || (!cut.is_empty() && page.cut_cursor != cut)
                || (page.complete && page.cut_cursor != batch.to_cursor)
            {
                return Ok(None);
            }
            validate_batch(&batch)?;
            bytes += batch.encoded_len();
            if bytes > REPAIR_PAGES * REPAIR_BYTES as usize
                || batch.changes.len() > REPAIR_MESSAGES as usize
            {
                return Ok(None);
            }
            cut = page.cut_cursor;
            after.clone_from(&batch.to_cursor);
            if after != batch.from_cursor {
                if !seen.insert(after.clone()) {
                    return Ok(None);
                }
                batches.push(batch);
            } else if !page.complete {
                return Ok(None);
            }
            if page.complete {
                return Ok(Some(batches));
            }
        }
        Ok(None)
    }
}
