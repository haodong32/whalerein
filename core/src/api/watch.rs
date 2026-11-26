//! Session key resolution, committed-history synchronization and the thin
//! CLI-facing watch adapter over the private observation owner.
//!
//! Watch orchestration lives in `crate::observation`; this module contains no
//! second watcher. Source cursors stay opaque here.
use super::*;
#[cfg(any(test, feature = "test-support", feature = "protocol-testing"))]
use crate::api::{Cancellation, ObservationInterests, SessionEventPayload};
use crate::conversation::{Conversation, ViewUpdate};
use crate::observation::bench::{BenchEvent, TokenDomain};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use whalerein_proto::{request_payload, response_payload, HistoryRead, RequestPayload};

/// Async sink for committed/live conversation updates.
///
/// The observation owner forwards events to its public stream; a plain sync
/// caller (for example `sync_session`) cannot fail delivery.
pub(crate) type SinkFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

pub(crate) trait UpdateSink {
    fn emit<'a>(&'a mut self, update: &'a ViewUpdate) -> SinkFuture<'a>;
}

#[cfg(test)]
pub(crate) struct AcceptAll;

#[cfg(test)]
impl UpdateSink for AcceptAll {
    fn emit<'a>(&'a mut self, _update: &'a ViewUpdate) -> SinkFuture<'a> {
        Box::pin(async { true })
    }
}

/// Bounded, secret-free candidate list for an ambiguous session resolution.
#[cfg(test)]
fn bounded_host_candidates(hosts: &[String]) -> String {
    const MAX_HOSTS: usize = 8;
    const MAX_HOST_CHARS: usize = 48;
    let mut shown = hosts
        .iter()
        .take(MAX_HOSTS)
        .map(|host| host.chars().take(MAX_HOST_CHARS).collect::<String>())
        .collect::<Vec<_>>()
        .join(", ");
    if hosts.len() > MAX_HOSTS {
        shown.push_str(&format!(" and {} more", hosts.len() - MAX_HOSTS));
    }
    shown
}

impl Core {
    /// Resolve an explicit Host and session once, verifying that the Host is
    /// still advertised by the current Access listing.
    #[cfg(test)]
    pub async fn scoped_session_key(
        &self,
        host_id: &str,
        session_id: &str,
    ) -> CoreResult<SessionKey> {
        let key = SessionKey::new(host_id, session_id)?;
        let hosts = self.hosts().await.map_err(|error| match error {
            CoreError::Revoked | CoreError::RevokedRetentionUnverified | CoreError::NotPaired => {
                error
            }
            _ => CoreError::Unavailable(
                "cannot verify authorized Hosts; specify a Host when available".into(),
            ),
        })?;
        if !hosts.iter().any(|host| host.host_id == key.host_id) {
            return Err(CoreError::NotFound("Host is not authorized".into()));
        }
        Ok(key)
    }

    /// Resolve an unqualified session against a fresh authorized Host listing.
    /// A partial discovery cannot prove uniqueness.
    #[cfg(test)]
    pub async fn session_key(&self, session_id: &str) -> CoreResult<SessionKey> {
        if session_id.is_empty() {
            return Err(CoreError::InvalidInput("session identity missing".into()));
        }
        let hosts = self.hosts().await.map_err(|error| match error {
            CoreError::Revoked | CoreError::RevokedRetentionUnverified | CoreError::NotPaired => {
                error
            }
            _ => CoreError::Unavailable(
                "cannot verify authorized Hosts; specify a Host when available".into(),
            ),
        })?;
        if hosts.len() == 1 {
            return SessionKey::new(hosts[0].host_id.clone(), session_id);
        }
        let mut matches = Vec::new();
        for host in hosts {
            // A revocation/settlement that lands between discovery and the
            // per-Host listing must end resolution. Mapping it to a generic
            // "unavailable, specify a Host" would invite a cross-Host action
            // while the profile is already revoked.
            if self.runtime.is_revoked() {
                return Err(CoreError::Revoked);
            }
            let list = self
                .sessions(&host.host_id)
                .await
                .map_err(|error| match error {
                    CoreError::Revoked
                    | CoreError::RevokedRetentionUnverified
                    | CoreError::NotPaired
                    | CoreError::Unauthorized => error,
                    _ => CoreError::Unavailable(
                        "cannot verify session on every authorized Host; specify a Host".into(),
                    ),
                })?;
            if self.runtime.is_revoked() {
                return Err(CoreError::Revoked);
            }
            if list.source != "remote" || !list.complete {
                return Err(CoreError::Unavailable(
                    "cannot verify session on every authorized Host; specify a Host".into(),
                ));
            }
            if list
                .sessions
                .iter()
                .any(|item| item.session_id == session_id)
            {
                matches.push(host.host_id);
            }
        }
        match matches.as_slice() {
            [host] => SessionKey::new(host.clone(), session_id),
            [] => Err(CoreError::NotFound(
                "session not found on authorized Hosts".into(),
            )),
            _ => Err(CoreError::Conflict(format!(
                "session exists on multiple Hosts ({}); specify a Host with --host",
                bounded_host_candidates(&matches)
            ))),
        }
    }

    /// Synchronise committed history through opaque cursors.
    ///
    /// `deliver_snapshot` distinguishes a cold watch attach (which rebuilds
    /// local history but must not print it as new output) from an explicit
    /// synchronisation or a gap recovery (which must deliver missed messages,
    /// deduplicated against what this watch already printed).
    ///
    /// `force_snapshot` makes an explicit `sync` report the whole committed
    /// history even when a cursor already exists. A watch never forces it, so
    /// a reconnect continues from its local cursor.
    pub(crate) async fn sync_conversation<S>(
        &self,
        key: &SessionKey,
        conversation: &mut Conversation,
        deliver_snapshot: bool,
        force_snapshot: bool,
        expected_projection: &str,
        sink: &mut S,
    ) -> CoreResult<u64>
    where
        S: UpdateSink + ?Sized,
    {
        let sync = self.sync_service()?;
        let bench = self.diagnostics.bench().clone();
        let capturing = bench.enabled();
        let sync_request = if capturing {
            bench.token(
                TokenDomain::Request,
                &format!("{}:{}:history", key.host_id, key.session_id),
            )
        } else {
            String::new()
        };
        let mut page_index = 0u64;
        let wire_key = whalerein_proto::SessionKey::from(key);
        self.runtime
            .set_sync_state(&key.host_id, crate::sync::SyncState::Syncing);
        let view = sync.history_view(key)?;
        let cache_matches = !expected_projection.is_empty()
            && view
                .as_ref()
                .is_some_and(|view| view.projection == expected_projection);
        let mut snapshot = force_snapshot || !cache_matches;
        let mut projection = if expected_projection.is_empty() {
            String::new()
        } else {
            expected_projection.to_owned()
        };
        let mut after = if snapshot || !cache_matches {
            String::new()
        } else {
            view.map(|view| view.cursor).unwrap_or_default()
        };
        let mut cut = String::new();
        let mut seen = HashSet::new();
        let mut emitted = 0;
        if snapshot {
            sync.discard_stage(key)?;
        }
        for _ in 0..10000 {
            let result = self
                .query(
                    &key.host_id,
                    RequestPayload {
                        payload: Some(request_payload::Payload::ReadHistory(HistoryRead {
                            session_id: key.session_id.clone(),
                            snapshot,
                            after_cursor: after.clone(),
                            cut_cursor: cut.clone(),
                            max_bytes: 256 * 1024,
                            max_messages: 100,
                            page: None,
                            exact: None,
                            continuation: String::new(),
                            source: whalerein_proto::HistorySource::Online as i32,
                        })),
                    },
                )
                .await;
            let response = match result {
                Err(error) if error.code() == "snapshot_required" && !snapshot => {
                    if !sink.emit(&conversation.reset("")).await {
                        return Ok(emitted);
                    }
                    snapshot = true;
                    after.clear();
                    cut.clear();
                    seen.clear();
                    sync.discard_stage(key)?;
                    continue;
                }
                other => other?,
            };
            let Some(response_payload::Payload::HistoryPage(page)) = response.payload else {
                return Err(CoreError::Security("unexpected history response".into()));
            };
            if page.session.as_ref() != Some(&wire_key) || page.snapshot != snapshot {
                return Err(CoreError::Security("history scope mismatch".into()));
            }
            if page.projection_id.is_empty()
                || page.projection_id.len() > 128
                || page.cut_cursor.is_empty()
                || page.cut_cursor.len() > 4096
            {
                return Err(CoreError::Security(
                    "history page has no valid content projection".into(),
                ));
            }
            let batch = page
                .batch
                .as_ref()
                .ok_or_else(|| CoreError::Security("missing history batch".into()))?;
            if batch.projection_id != page.projection_id {
                return Err(CoreError::Security(
                    "history page projection mismatch".into(),
                ));
            }
            if projection.is_empty() {
                projection = page.projection_id.clone();
            } else if projection != page.projection_id {
                // A watch baseline names the active projection. A page from a
                // different namespace means the watch must re-register rather
                // than adopt content under a stale state. Callers without a
                // baseline (explicit sync) may restart on the new projection.
                if !expected_projection.is_empty() {
                    sync.discard_stage(key)?;
                    return Err(CoreError::Api {
                        code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
                        message: "content projection changed; refresh the observation".into(),
                    });
                }
                if !sink.emit(&conversation.reset_projection("")).await {
                    return Ok(emitted);
                }
                snapshot = true;
                projection = page.projection_id.clone();
                after.clear();
                cut.clear();
                seen.clear();
                sync.discard_stage(key)?;
                continue;
            }
            let inconsistent = (!cut.is_empty() && cut != page.cut_cursor)
                || (!after.is_empty() && after != batch.from_cursor)
                || page.cut_cursor.is_empty()
                || (page.complete && batch.to_cursor != page.cut_cursor)
                || (!page.complete
                    && (batch.to_cursor == batch.from_cursor
                        || !seen.insert(batch.to_cursor.clone())));
            if inconsistent {
                // An interrupted or missing range cannot certify a complete
                // snapshot. Drop only staged pages and restart from the fresh
                // Source cut; the previously committed view remains untouched.
                if !snapshot {
                    if !sink.emit(&conversation.reset("")).await {
                        return Ok(emitted);
                    }
                    snapshot = true;
                }
                after.clear();
                cut.clear();
                seen.clear();
                sync.discard_stage(key)?;
                continue;
            }
            cut = page.cut_cursor.clone();
            after = batch.to_cursor.clone();
            let installed = sync.apply_page(key, &page)?;
            if capturing {
                page_index += 1;
                bench.emit(BenchEvent::CoreSync {
                    request: sync_request.clone(),
                    page: page_index,
                    messages: batch.changes.len() as u64,
                    bytes: history_page_bytes(batch),
                    verified: installed,
                });
            }
            if snapshot {
                // A snapshot replaces the whole committed view only on its
                // final page. Emitting from earlier pages would either replay
                // the previous view or deliver the replacement twice, so only
                // the committed complete view is considered. An idempotent
                // repeat of the same complete snapshot still reports the whole
                // view to an explicit caller.
                if !page.complete {
                    continue;
                }
                let mut ordinal = -1;
                while let Some((next, message)) = sync.next_message(key, ordinal)? {
                    ordinal = next;
                    if deliver_snapshot {
                        if let Some(update) = conversation.committed_once(&message)? {
                            emitted += 1;
                            if !sink.emit(&update).await {
                                return Ok(emitted);
                            }
                            for activity in conversation.take_activity_updates() {
                                if !sink.emit(&activity).await {
                                    return Ok(emitted);
                                }
                            }
                        }
                    } else {
                        // Cold baseline: record what the viewer has now seen
                        // without presenting it as new output.
                        conversation.mark_delivered(&message);
                    }
                }
                self.runtime
                    .set_sync_state(&key.host_id, crate::sync::SyncState::Synced);
                return Ok(emitted);
            } else if installed {
                for change in &batch.changes {
                    let Some(update) = conversation.committed_once(change)? else {
                        continue;
                    };
                    emitted += 1;
                    if !sink.emit(&update).await {
                        return Ok(emitted);
                    }
                    for activity in conversation.take_activity_updates() {
                        if !sink.emit(&activity).await {
                            return Ok(emitted);
                        }
                    }
                }
            }
            if page.complete {
                self.runtime
                    .set_sync_state(&key.host_id, crate::sync::SyncState::Synced);
                return Ok(emitted);
            }
        }
        Err(CoreError::Unavailable(
            "history pagination budget exceeded".into(),
        ))
    }

    /// Thin CLI-facing adapter over one Conversation observation.
    ///
    /// This exists only while one-shot callers migrate; it contains no watch
    /// loop of its own and shares the exact observation owner with the typed
    /// `observe_session` interface.
    // Test/example-only convenience over the one observation owner. It is not
    // part of the supported consumer surface; CLI and mobile use
    // observe_session directly, and no wire contract depends on this adapter.
    #[cfg(any(test, feature = "test-support", feature = "protocol-testing"))]
    pub async fn watch_session<F>(
        &self,
        key: &SessionKey,
        timeout: Option<Duration>,
        mut on_update: F,
    ) -> CoreResult<u64>
    where
        F: FnMut(&ViewUpdate) -> bool,
    {
        self.authorized_session(key)?;
        let mut observation = self
            .observe_session(
                key,
                ObservationInterests::conversation(),
                Cancellation::new(),
            )
            .await?;
        let mut emitted = 0u64;
        let operation = async {
            while let Some(event) = observation.next_event().await {
                let SessionEventPayload::Conversation(update) = event.payload else {
                    continue;
                };
                if matches!(
                    update,
                    ViewUpdate::Preview { .. }
                        | ViewUpdate::Message { .. }
                        | ViewUpdate::AttemptEnded { .. }
                ) {
                    emitted += 1;
                }
                if !on_update(&update) {
                    observation.close().await;
                    return;
                }
            }
        };
        let result = if let Some(limit) = timeout {
            match tokio::time::timeout(limit, operation).await {
                Ok(()) => Ok(()),
                Err(_) => Err(()),
            }
        } else {
            operation.await;
            Ok(())
        };
        match result {
            Ok(()) => {
                if let Some(error) = observation.take_terminal_error() {
                    return Err(error);
                }
                Ok(emitted)
            }
            Err(()) => {
                observation.close().await;
                if emitted == 0 {
                    if let Some(error) = observation
                        .take_terminal_error()
                        .or_else(|| observation.take_last_error())
                    {
                        return Err(error);
                    }
                    Err(CoreError::Timeout)
                } else {
                    Ok(emitted)
                }
            }
        }
    }
}

/// Content-free byte size of one verified history batch. Only block text and
/// reasoning lengths are read; no content enters the fact.
fn history_page_bytes(batch: &whalerein_proto::HistoryBatch) -> u64 {
    batch
        .changes
        .iter()
        .filter_map(|change| change.message.as_ref())
        .flat_map(|message| message.blocks.iter())
        .map(|block| match &block.content {
            Some(whalerein_proto::content_block::Content::Text(text)) => text.text.len() as u64,
            Some(whalerein_proto::content_block::Content::Reasoning(reasoning)) => {
                reasoning.text.len() as u64
            }
            _ => 0,
        })
        .sum()
}
