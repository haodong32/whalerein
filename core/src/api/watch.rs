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

/// One active watch per device profile, including across processes.
///
/// Two CLI processes share one MQTT client id, so the later connection would
/// silently replace the earlier subscription. A profile-scoped OS lock makes
/// the conflict deterministic: the second `tail` fails with `conflict` while
/// unrelated commands such as `send` keep working. The lock is released by the
/// OS when the process exits, including after Ctrl-C.
/// Failures that a reconnect cannot fix. Everything else during a watch is
/// treated as a transient transport/path problem and retried with backoff.
fn watch_fatal(error: &CoreError) -> bool {
    // Upgrade-required codes may arrive as wrapped `Host`/`Api` errors.
    if matches!(
        error.code(),
        "unsupported_capability" | "protocol_version_incompatible"
    ) {
        return true;
    }
    matches!(
        error,
        CoreError::Revoked
            | CoreError::RevokedRetentionUnverified
            | CoreError::NotPaired
            | CoreError::FingerprintMismatch { .. }
            | CoreError::Unsupported
            | CoreError::ProtocolIncompatible
            | CoreError::InvalidInput(_)
            | CoreError::Config(_)
            | CoreError::Cache(_)
            | CoreError::HostRejected
            | CoreError::PairingCancelled
            | CoreError::PairingFenced
            | CoreError::PairingCommittedUnavailable
            | CoreError::PairingCommittedUnconfirmed
    )
}

#[derive(Debug)]
struct WatchLease {
    _file: Option<std::fs::File>,
}

impl WatchLease {
    fn acquire(profile: &std::path::Path) -> CoreResult<Self> {
        let path = profile.join("watch.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| CoreError::Cache("watch lock directory is unavailable".into()))?;
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .map_err(|_| CoreError::Cache("watch lock is unavailable".into()))?;
            extern "C" {
                fn flock(
                    fd: std::os::raw::c_int,
                    operation: std::os::raw::c_int,
                ) -> std::os::raw::c_int;
            }
            // LOCK_EX | LOCK_NB
            if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
                Err(CoreError::Conflict(
                    "another watch is active for this device profile; stop it or use a separate device profile".into(),
                ))
            } else {
                Ok(Self { _file: Some(file) })
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&path);
            match file {
                Ok(file) => Ok(Self { _file: Some(file) }),
                Err(error) if error.raw_os_error() == Some(32) => Err(CoreError::Conflict(
                    "another watch is active for this device profile; stop it or use a separate device profile".into(),
                )),
                Err(_) => Err(CoreError::Cache("watch lock is unavailable".into())),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Ok(Self { _file: None })
        }
    }
}

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

/// Bounded, secret-free candidate list for an ambiguous session resolution.
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
    /// Resolve against a fresh authorized Host listing. A partial discovery cannot prove uniqueness.
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
        if let Some(host) = &self.session_host {
            if !hosts.iter().any(|item| &item.host_id == host) {
                return Err(CoreError::NotFound("Host is not authorized".into()));
            }
            return Ok(SessionKey {
                host_id: host.clone(),
                session_id: session_id.into(),
            });
        }
        if hosts.len() == 1 {
            return Ok(SessionKey {
                host_id: hosts[0].host_id.clone(),
                session_id: session_id.into(),
            });
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
            [host] => Ok(SessionKey {
                host_id: host.clone(),
                session_id: session_id.into(),
            }),
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
    pub(super) async fn sync_conversation<F>(
        &self,
        key: &SessionKey,
        conversation: &mut Conversation,
        deliver_snapshot: bool,
        force_snapshot: bool,
        on_update: &mut F,
    ) -> CoreResult<u64>
    where
        F: FnMut(&ViewUpdate) -> bool,
    {
        let sync = self.sync_service()?;
        self.runtime
            .set_sync_state(&key.host_id, crate::sync::SyncState::Syncing);
        let mut snapshot = force_snapshot || sync.cursor(key)?.is_none();
        let mut after = if force_snapshot {
            String::new()
        } else {
            sync.cursor(key)?.unwrap_or_default()
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
                    // A snapshot replaces the whole committed view only on its
                    // final page. Emitting from earlier pages would either
                    // replay the previous view or deliver the replacement
                    // twice, so only the committed complete view is considered.
                    if !page.complete {
                        continue;
                    }
                    let mut ordinal = -1;
                    while let Some((next, message)) = sync.next_message(key, ordinal)? {
                        ordinal = next;
                        if deliver_snapshot {
                            if let Some(update) = conversation.committed_once(&message)? {
                                emitted += 1;
                                if !on_update(&update) {
                                    return Ok(emitted);
                                }
                            }
                        } else {
                            // Cold baseline: record what the viewer has now
                            // seen without presenting it as new output.
                            conversation.mark_delivered(&message);
                        }
                    }
                    self.runtime
                        .set_sync_state(&key.host_id, crate::sync::SyncState::Synced);
                    return Ok(emitted);
                } else {
                    for change in &batch.changes {
                        let Some(update) = conversation.committed_once(change)? else {
                            continue;
                        };
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
        let _lease = WatchLease::acquire(&self.options.data_dir)?;
        self.watch_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| CoreError::Conflict("a conversation watch is already active".into()))?;
        let _watch_guard = WatchGuard(self.watch_active.clone());
        let mut emitted = 0;
        let mut last_watch = None;
        // One Conversation spans reconnects so committed identities already
        // delivered to this viewer are not printed again after a gap recovery.
        let mut conversation = Conversation::default();
        // Preserve the last actionable transport failure. A bounded tail
        // with no output reports that cause instead of a bare deadline.
        let mut last_error: Option<CoreError> = None;
        let operation = async {
            let mut fresh = false;
            loop {
                if self.runtime.is_revoked() {
                    return Err(CoreError::Revoked);
                }
                match self
                    .watch_connection(
                        key,
                        fresh,
                        &mut last_watch,
                        &mut conversation,
                        &mut emitted,
                        &mut on_update,
                    )
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) => {
                        fresh = true;
                        tokio::time::sleep(self.runtime.next_reconnect_delay(&key.host_id)).await;
                    }
                    // Transport changes, channel replacement and a transient
                    // source/baseline inconsistency are reconnectable: a path
                    // change must rebuild the channel inside the deadline, not
                    // end the watch with an internal/security error.
                    Err(error) if !watch_fatal(&error) => {
                        last_error = Some(error);
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
                        Err(last_error.unwrap_or(CoreError::Timeout))
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
        conversation: &mut Conversation,
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
        // A snapshot attach can take several pages; buffers must absorb the
        // live frames that arrive meanwhile instead of dropping previews.
        let (tx, mut rx) = mpsc::channel(1024);
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
        // Overflow is never silent: the watch reconnects and re-syncs so a
        // committed message is still delivered exactly once.
        let overflow = Arc::new(AtomicBool::new(false));
        let task_overflow = overflow.clone();
        let task = tokio::spawn(async move {
            let result = client
                .run(
                    || task_stop.load(Ordering::SeqCst) || runtime.is_revoked(),
                    |envelope| {
                        let bytes = envelope.ciphertext.len();
                        if task_queued.fetch_add(bytes, Ordering::SeqCst) + bytes > 32 * 1024 * 1024
                        {
                            task_queued.fetch_sub(bytes, Ordering::SeqCst);
                            task_overflow.store(true, Ordering::SeqCst);
                            return true;
                        }
                        if tx.try_send(envelope.clone()).is_err() {
                            task_queued.fetch_sub(bytes, Ordering::SeqCst);
                            task_overflow.store(true, Ordering::SeqCst);
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
        conversation.reset(&epoch);
        // Anything already in the local committed view predates this watch:
        // it is baseline, not output, and a reconnect must not replay it.
        for message_id in self.sync_service()?.history_message_ids(key)? {
            conversation.mark_delivered_id(&message_id);
        }
        if !on_update(&ViewUpdate::Reset) {
            pump.finish().await;
            return Ok(true);
        }
        let mut keep_going = true;
        *emitted += self
            .sync_conversation(key, &mut *conversation, false, false, &mut |update| {
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
            if overflow.swap(false, Ordering::SeqCst) {
                // A dropped live frame would leave a hole in the preview or
                // hide a committed turn; reconnect and resync instead.
                pump.finish().await;
                return Ok(false);
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
                                BatchOutcome::Applied=>for change in &batch.changes{if let Some(view)=conversation.committed_once(change)?{*emitted+=1;if !on_update(&view){pump.finish().await;return Ok(true)}}},
                                BatchOutcome::Duplicate=>{},
                                // A missed batch is output the viewer never saw;
                                // recovery must deliver it (deduplicated against
                                // messages this watch already printed).
                                BatchOutcome::Gap=>{*emitted+=self.sync_conversation(key,conversation,true,false,&mut |u|{keep_going=on_update(u);keep_going}).await?;},
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
                    // The head moved past the local cursor: this is a recovery
                    // gap, not a cold baseline, so deliver the unseen messages.
                    if sync.cursor(key)?.as_deref()!=Some(state.history_head.as_str()) {*emitted+=self.sync_conversation(key,conversation,true,false,&mut |u|{keep_going=on_update(u);keep_going}).await?;}
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

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[test]
    fn a_second_watch_lease_for_the_same_profile_is_a_deterministic_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let first = WatchLease::acquire(directory.path()).unwrap();
        let error = WatchLease::acquire(directory.path()).unwrap_err();
        assert_eq!(error.code(), "conflict");
        assert_eq!(error.exit_code(), 8);
        assert!(error.to_string().contains("another watch is active"));
        drop(first);
        // The OS lock is released immediately; a later tail can attach.
        WatchLease::acquire(directory.path()).unwrap();
    }
}
