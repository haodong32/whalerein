//! Typed watch/synchronization orchestration. Source cursors stay opaque here.
use super::*;
use crate::conversation::{Conversation, ViewUpdate};
use crate::sync::BatchOutcome;
use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use whalerein_proto::{
    request_payload, response_payload, session_update, HistoryRead, RefreshWatches, SessionKey,
    UnwatchSession, WatchSession,
};

// The v1 device transport owns one MQTT subscription. Reject competing
// watches rather than starting clients that evict each other at the broker.
struct WatchGuard(Arc<AtomicBool>);
impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Pump {
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Pump {
    async fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_millis(500), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

impl Core {
    pub fn session_key(&self, session_id: &str) -> CoreResult<SessionKey> {
        if session_id.is_empty() {
            return Err(CoreError::InvalidInput("session identity missing".into()));
        }
        Ok(SessionKey {
            host_id: self.require_config()?.host_id,
            session_id: session_id.into(),
        })
    }

    pub(super) async fn sync_conversation<F>(
        &self,
        key: &SessionKey,
        conversation: &mut Conversation,
        on_update: &mut F,
    ) -> CoreResult<u64>
    where
        F: FnMut(&ViewUpdate) -> bool,
    {
        let sync = self.sync_service()?;
        self.runtime
            .set_sync_state(&key.host_id, crate::sync::SyncState::Syncing);
        let mut snapshot = sync.cursor(key)?.is_none();
        let mut after = sync.cursor(key)?.unwrap_or_default();
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
                        })),
                    },
                )
                .await;
            let response = match result {
                Err(error) if error.code() == "snapshot_required" && !snapshot => {
                    if !on_update(&conversation.reset("")) {
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
            if page.session.as_ref() != Some(key) || page.snapshot != snapshot {
                return Err(CoreError::Security("history scope mismatch".into()));
            }
            let batch = page
                .batch
                .as_ref()
                .ok_or_else(|| CoreError::Security("missing history batch".into()))?;
            if (!cut.is_empty() && cut != page.cut_cursor)
                || (!after.is_empty() && after != batch.from_cursor)
                || page.cut_cursor.is_empty()
                || (page.complete && batch.to_cursor != page.cut_cursor)
                || (!page.complete
                    && (batch.to_cursor == batch.from_cursor
                        || !seen.insert(batch.to_cursor.clone())))
            {
                return Err(CoreError::Security("inconsistent history cut".into()));
            }
            cut = page.cut_cursor.clone();
            after = batch.to_cursor.clone();
            let installed = sync.apply_page(key, &page)?;
            if installed {
                if snapshot {
                    let mut ordinal = -1;
                    while let Some((next, message)) = sync.next_message(key, ordinal)? {
                        ordinal = next;
                        let update = conversation.committed(&message)?;
                        emitted += 1;
                        if !on_update(&update) {
                            return Ok(emitted);
                        }
                    }
                } else {
                    for change in &batch.changes {
                        let update = conversation.committed(change)?;
                        emitted += 1;
                        if !on_update(&update) {
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

    pub async fn watch_session<F>(
        &self,
        key: &SessionKey,
        timeout: Option<Duration>,
        mut on_update: F,
    ) -> CoreResult<u64>
    where
        F: FnMut(&ViewUpdate) -> bool,
    {
        self.watch_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| CoreError::Conflict("a conversation watch is already active".into()))?;
        let _watch_guard = WatchGuard(self.watch_active.clone());
        let mut emitted = 0;
        let mut last_watch = None;
        let operation = async {
            let mut fresh = false;
            loop {
                if self.runtime.is_revoked() {
                    return Err(CoreError::Revoked);
                }
                match self
                    .watch_connection(key, fresh, &mut last_watch, &mut emitted, &mut on_update)
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false)
                    | Err(CoreError::Unavailable(_))
                    | Err(CoreError::HostOffline)
                    | Err(CoreError::ChannelMismatch) => {
                        fresh = true;
                        tokio::time::sleep(self.runtime.next_reconnect_delay(&key.host_id)).await;
                    }
                    Err(error)
                        if matches!(
                            error.code(),
                            "expired" | "revocations_pending" | "snapshot_required" | "unavailable"
                        ) =>
                    {
                        fresh = true;
                        tokio::time::sleep(self.runtime.next_reconnect_delay(&key.host_id)).await;
                    }
                    Err(error) => return Err(error),
                }
            }
        };
        let result = if let Some(limit) = timeout {
            match tokio::time::timeout(limit, operation).await {
                Ok(result) => result,
                Err(_) => {
                    if emitted == 0 {
                        Err(CoreError::Timeout)
                    } else {
                        Ok(())
                    }
                }
            }
        } else {
            operation.await
        };
        if let Some(watch_id) = last_watch {
            let cleanup = self.query(
                &key.host_id,
                RequestPayload {
                    payload: Some(request_payload::Payload::UnwatchSession(UnwatchSession {
                        watch_id,
                    })),
                },
            );
            let _ = tokio::time::timeout(Duration::from_millis(250), cleanup).await;
        }
        result?;
        Ok(emitted)
    }

    async fn watch_connection<F>(
        &self,
        key: &SessionKey,
        fresh: bool,
        last_watch: &mut Option<String>,
        emitted: &mut u64,
        on_update: &mut F,
    ) -> CoreResult<bool>
    where
        F: FnMut(&ViewUpdate) -> bool,
    {
        let config = self.require_config()?;
        let channel = self.ensure_channel(&key.host_id, fresh).await?;
        let token = load_device_token(&self.options.secrets, &config.device_id)?
            .ok_or(CoreError::NotPaired)?;
        let client = DeviceMqttClient::new(
            self.mqtt_settings(&config)?,
            &key.host_id,
            &config.device_id,
            token,
        )
        .with_trace_id(self.diagnostic_id());
        let (tx, mut rx) = mpsc::channel(16);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut ready_tx = Some(ready_tx);
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = stop.clone();
        let runtime = self.runtime.clone();
        let interrupted = Arc::new(AtomicBool::new(false));
        let task_interrupted = interrupted.clone();
        let queued = Arc::new(AtomicUsize::new(0));
        let task_queued = queued.clone();
        let error = Arc::new(Mutex::new(None));
        let task_error = error.clone();
        let task = tokio::spawn(async move {
            let result = client
                .run(
                    || task_stop.load(Ordering::SeqCst) || runtime.is_revoked(),
                    |envelope| {
                        let bytes = envelope.ciphertext.len();
                        if task_queued.fetch_add(bytes, Ordering::SeqCst) + bytes > 32 * 1024 * 1024
                        {
                            task_queued.fetch_sub(bytes, Ordering::SeqCst);
                            return true;
                        }
                        if tx.try_send(envelope.clone()).is_err() {
                            task_queued.fetch_sub(bytes, Ordering::SeqCst);
                        }
                        true
                    },
                    |state| match state {
                        MqttConnectionEvent::Subscribed => {
                            if let Some(sender) = ready_tx.take() {
                                let _ = sender.send(());
                            }
                        }
                        MqttConnectionEvent::Disconnected => {
                            task_interrupted.store(true, Ordering::SeqCst);
                        }
                        _ => {}
                    },
                )
                .await;
            if let Err(failure) = result {
                *task_error.lock().expect("watch error mutex") = Some(CoreError::from(failure));
            }
            task_interrupted.store(true, Ordering::SeqCst);
        });
        let pump = Pump {
            stop,
            task: Some(task),
        };
        if ready_rx.await.is_err() {
            return Err(error
                .lock()
                .expect("watch error mutex")
                .take()
                .unwrap_or_else(|| CoreError::Unavailable("subscription unavailable".into())));
        }
        let response = self
            .query(
                &key.host_id,
                RequestPayload {
                    payload: Some(request_payload::Payload::WatchSession(WatchSession {
                        session_id: key.session_id.clone(),
                    })),
                },
            )
            .await?;
        let Some(response_payload::Payload::WatchReady(ready)) = response.payload else {
            return Err(CoreError::Security("unexpected watch response".into()));
        };
        *last_watch = Some(ready.watch_id.clone());
        let initial = ready
            .state
            .ok_or_else(|| CoreError::Security("missing watch baseline".into()))?;
        if initial.session.as_ref() != Some(key)
            || initial.live_epoch.is_empty()
            || ready.watch_id.is_empty()
        {
            return Err(CoreError::Security("invalid watch baseline".into()));
        }
        self.journal
            .record("core", "watch_ready", "succeeded", "none");
        let epoch = initial.live_epoch.clone();
        let mut state_revision = initial.revision;
        let mut conversation = Conversation::default();
        conversation.reset(&epoch);
        if !on_update(&ViewUpdate::Reset) {
            pump.finish().await;
            return Ok(true);
        }
        let mut keep_going = true;
        *emitted += self
            .sync_conversation(key, &mut conversation, &mut |update| {
                keep_going = on_update(update);
                keep_going
            })
            .await?;
        if !keep_going {
            pump.finish().await;
            return Ok(true);
        }
        let _ = self
            .runtime
            .set_state(&key.host_id, ConnectionState::Connected);
        if !on_update(&ViewUpdate::State(initial)) {
            pump.finish().await;
            return Ok(true);
        }
        let sync = self.sync_service()?;
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        loop {
            if self.runtime.is_revoked() {
                return Err(CoreError::Revoked);
            }
            if interrupted.load(Ordering::SeqCst) {
                if let Some(failure) = error.lock().expect("watch error mutex").take() {
                    return Err(failure);
                }
                pump.finish().await;
                return Ok(false);
            }
            tokio::select! {
                envelope=rx.recv()=>{
                    let Some(envelope)=envelope else{if let Some(failure)=error.lock().expect("watch error mutex").take(){return Err(failure)}pump.finish().await;return Ok(false)};
                    queued.fetch_sub(envelope.ciphertext.len(),Ordering::SeqCst);
                    let update=sync.open_update(&channel,&envelope)?;
                    if update.session.as_ref()!=Some(key)||update.watch_id!=ready.watch_id{continue}
                    match update.payload {
                        Some(session_update::Payload::Live(frame))=>{if let Some(view)=conversation.live(&frame)?{*emitted+=1;if !on_update(&view){pump.finish().await;return Ok(true)}}},
                        Some(session_update::Payload::History(batch))=>{
                            match sync.apply_batch(key,&batch)? {
                                BatchOutcome::Applied=>for change in &batch.changes{let view=conversation.committed(change)?;*emitted+=1;if !on_update(&view){pump.finish().await;return Ok(true)}},
                                BatchOutcome::Duplicate=>{},
                                BatchOutcome::Gap=>{*emitted+=self.sync_conversation(key,&mut conversation,&mut |u|{keep_going=on_update(u);keep_going}).await?;},
                            }
                        },
                        Some(session_update::Payload::State(state))=>{
                            if state.session.as_ref()!=Some(key){return Err(CoreError::Security("state update scope mismatch".into()))}
                            if state.live_epoch!=epoch{pump.finish().await;return Ok(false)}
                            if state.revision>state_revision {state_revision=state.revision;if !on_update(&ViewUpdate::State(state)){pump.finish().await;return Ok(true)}}
                        },
                        Some(session_update::Payload::Reset(_))=>{conversation.reset("");sync.discard_stage(key)?;pump.finish().await;return Ok(false)},
                        None=>return Err(CoreError::Security("empty typed update".into())),
                    }
                },
                _=interval.tick()=>{
                    if let Some(failure)=error.lock().expect("watch error mutex").take(){return Err(failure)}
                    let response=self.query(&key.host_id,RequestPayload{payload:Some(request_payload::Payload::RefreshWatches(RefreshWatches{watch_ids:vec![ready.watch_id.clone()]}))}).await;
                    let response=match response{Err(e)if e.code()=="expired"=>{pump.finish().await;return Ok(false)},other=>other?};
                    let Some(response_payload::Payload::WatchStates(states))=response.payload else{return Err(CoreError::Security("unexpected watch refresh".into()))};
                    let state=states.watches.into_iter().find(|w|w.watch_id==ready.watch_id).and_then(|w|w.state).ok_or_else(||CoreError::Security("missing watch refresh".into()))?;
                    if state.session.as_ref()!=Some(key){return Err(CoreError::Security("watch refresh scope mismatch".into()))}
                    if state.live_epoch != epoch { pump.finish().await; return Ok(false) }
                    if sync.cursor(key)?.as_deref()!=Some(state.history_head.as_str()) {*emitted+=self.sync_conversation(key,&mut conversation,&mut |u|{keep_going=on_update(u);keep_going}).await?;}
                    if state.revision>state_revision {state_revision=state.revision;if !on_update(&ViewUpdate::State(state)){pump.finish().await;return Ok(true)}}
                }
            }
            if !keep_going {
                pump.finish().await;
                return Ok(true);
            }
        }
    }
}
