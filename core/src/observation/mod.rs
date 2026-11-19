//! Private session observation owner.
//!
//! One observation owns the profile OS watch lease, the single device MQTT
//! subscription and the conversation baseline. Conversation and Questions are
//! reconciled independently, so a failed or unsupported history facet never
//! blocks question readiness. The public handle only delivers typed
//! `SessionEvent`s; wire messages never leave this module.

mod questions;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use whalerein_proto::{
    request_payload, response_payload, session_update, EventEnvelope, QuestionDescriptor,
    RefreshWatches, UnwatchSession, WatchSession,
};

use crate::access::{DeviceMqttClient, MqttConnectionEvent};
use crate::api::core::watch::{SinkFuture, UpdateSink};
use crate::api::{Cancellation, ObservationInterests, SessionEvent, SessionEventPayload};
use crate::api::{Core, CoreError, CoreResult, QuestionView};
use crate::conversation::{Conversation, ViewUpdate};
use crate::runtime::ConnectionState;
use crate::security::load_device_token;
use crate::session::SessionKey;
use crate::sync::BatchOutcome;
use crate::util::random_id;

use questions::{PageFuture, QuestionPageSource, QuestionReconciler, Step};

/// Monotonic observer generation. One profile has one active observation, so a
/// process-local counter is enough to fence replacement within this Core.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Why question reconciliation was requested before registration, after a
/// transport loss, or by an authoritative invalidation.
#[derive(Debug)]
pub(crate) enum QuestionSignal {
    Descriptor(Box<QuestionDescriptor>),
    TransportLost { code: String },
}

/// Failures a reconnect cannot fix. Everything else during a watch is treated
/// as a transient transport/path problem and retried with backoff.
fn watch_fatal(error: &CoreError) -> bool {
    if matches!(
        error.code(),
        // A scope denial is an authoritative answer, not a transport problem.
        // Reconnecting would only repeat the same 401/403 until the deadline,
        // hiding the revocation from a bounded caller.
        "unsupported_capability"
            | "protocol_version_incompatible"
            | "unauthorized"
            | "forbidden"
    ) {
        return true;
    }
    matches!(
        error,
        CoreError::Revoked
            | CoreError::RevokedRetentionUnverified
            | CoreError::AuthorityClosed(_)
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

/// One active observation per client instance, including across processes.
///
/// Two CLI processes share one MQTT client id, so the later connection would
/// silently replace the earlier subscription. An instance-scoped OS lock makes
/// the conflict deterministic: the second observation fails with `conflict`
/// while unrelated commands such as `send` keep working. The lock is released
/// by the OS when the process exits, including after Ctrl-C.
#[derive(Debug)]
struct WatchLease {
    _file: Option<std::fs::File>,
}

impl WatchLease {
    fn acquire(instance: &std::path::Path) -> CoreResult<Self> {
        let path = instance.join("watch.lock");
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
                    "another watch is active for this client instance; stop it or use a separate instance".into(),
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
                    "another watch is active for this client instance; stop it or use a separate instance".into(),
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

#[derive(Default)]
struct ObservationShared {
    stop: AtomicBool,
    terminal: Mutex<Option<CoreError>>,
    last: Mutex<Option<CoreError>>,
    admission: Mutex<Option<(WatchLease, WatchGuard)>>,
}

impl ObservationShared {
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn release_admission(&self) {
        self.admission
            .lock()
            .expect("observation admission mutex")
            .take();
    }
}

/// One scoped session observation handle.
///
/// `next_event` yields producer-scoped events until the observation ends or is
/// cancelled. `close` stops only this observation; it never cancels a separate
/// command execution.
pub struct SessionObservation {
    session: SessionKey,
    observation_id: String,
    generation: u64,
    cancellation: Cancellation,
    events: mpsc::Receiver<SessionEvent>,
    shared: Arc<ObservationShared>,
    tasks: Vec<JoinHandle<()>>,
    closed: bool,
    access_session: Option<(Core, String)>,
}

impl std::fmt::Debug for SessionObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionObservation")
            .field("observation_id", &self.observation_id)
            .field("generation", &self.generation)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl SessionObservation {
    pub fn session(&self) -> &SessionKey {
        &self.session
    }

    pub fn observation_id(&self) -> &str {
        &self.observation_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    /// Wait for the next event. `None` means the observation ended.
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        if self.closed {
            return None;
        }
        let event = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => None,
            () = wait_for_cloud_session_end(self.access_session.as_ref().map(|(core, id)| (core, id.as_str()))) => None,
            event = self.events.recv() => event,
        };
        let event = if self
            .access_session
            .as_ref()
            .is_some_and(|(core, id)| !same_cloud_session(core, id))
        {
            *self
                .shared
                .terminal
                .lock()
                .expect("observation error mutex") = Some(CoreError::AuthenticationRequired);
            None
        } else {
            event
        };
        if event.is_none() {
            self.shutdown().await;
        }
        event
    }

    /// Stop refresh, delivery and the underlying transport for this
    /// observation. An independently running answer command is unaffected.
    pub async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.shutdown().await;
    }

    async fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Release the profile lease with the tasks so a new observation can
        // attach immediately after close.
        self.shared.release_admission();
        let tasks = std::mem::take(&mut self.tasks);
        for mut task in tasks {
            if tokio::time::timeout(Duration::from_millis(700), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        self.closed = true;
    }

    /// Last transient transport failure observed before the stream ended.
    ///
    /// A bounded caller can report the actionable cause instead of a bare
    /// timeout when no output was delivered. `None` means no transient failure
    /// was recorded.
    pub fn take_last_error(&mut self) -> Option<CoreError> {
        self.shared
            .last
            .lock()
            .expect("observation error mutex")
            .take()
    }

    /// Fatal failure that ended the observation, if any.
    ///
    /// `next_event` yields `None` for both a normal end and a fatal failure;
    /// callers that must surface the specific Core error (revoked, not paired,
    /// unsupported, security) take it once from here. Taking it does not
    /// change the observation's lifetime or another facet's state.
    pub fn take_terminal_error(&mut self) -> Option<CoreError> {
        self.shared
            .terminal
            .lock()
            .expect("observation error mutex")
            .take()
    }
}

impl Drop for SessionObservation {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.shared.release_admission();
    }
}

impl Core {
    /// Start one session observation selecting Conversation, Questions, or
    /// both. The native watch registration happens before any facet read.
    pub async fn observe_session(
        &self,
        key: &SessionKey,
        interests: ObservationInterests,
        cancellation: Cancellation,
    ) -> CoreResult<SessionObservation> {
        self.authorized_session(key)?;
        let cloud_session = self.cloud_session_id()?;
        if interests.is_empty() {
            return Err(CoreError::InvalidInput(
                "at least one observation interest is required".into(),
            ));
        }
        let lease = WatchLease::acquire(self.data_dir())?;
        self.watch_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| CoreError::Conflict("a conversation watch is already active".into()))?;
        let guard = WatchGuard(self.watch_active.clone());
        let observation_id = random_id(16);
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst);
        let (events_tx, events_rx) = mpsc::channel(256);
        let (questions_tx, questions_rx) = if interests.questions {
            let (tx, rx) = mpsc::unbounded_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let shared = Arc::new(ObservationShared {
            admission: Mutex::new(Some((lease, guard))),
            ..ObservationShared::default()
        });
        let mut tasks = Vec::new();
        if let (Some(signal_tx), Some(signal_rx)) = (questions_tx.clone(), questions_rx) {
            tasks.push(tokio::spawn(run_questions(
                self.clone(),
                key.clone(),
                observation_id.clone(),
                generation,
                events_tx.clone(),
                signal_rx,
                cancellation.clone(),
            )));
            drop(signal_tx);
        }
        tasks.push(tokio::spawn(run_owner(
            self.clone(),
            key.clone(),
            observation_id.clone(),
            generation,
            interests,
            events_tx,
            shared.clone(),
            cancellation.clone(),
            questions_tx,
            cloud_session.clone(),
        )));
        Ok(SessionObservation {
            session: key.clone(),
            observation_id,
            generation,
            cancellation,
            events: events_rx,
            shared,
            tasks,
            closed: false,
            access_session: cloud_session.map(|id| (self.clone(), id)),
        })
    }
}

fn same_cloud_session(core: &Core, expected: &str) -> bool {
    core.cloud_session_id().ok().flatten().as_deref() == Some(expected)
}

async fn wait_for_cloud_session_end(session: Option<(&Core, &str)>) {
    let Some((core, id)) = session else {
        return std::future::pending().await;
    };
    while same_cloud_session(core, id) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_owner(
    core: Core,
    key: SessionKey,
    observation_id: String,
    generation: u64,
    interests: ObservationInterests,
    events: mpsc::Sender<SessionEvent>,
    shared: Arc<ObservationShared>,
    cancellation: Cancellation,
    questions: Option<mpsc::UnboundedSender<QuestionSignal>>,
    cloud_session: Option<String>,
) {
    let mut fresh = false;
    let mut last_watch = None;
    let mut conversation = Conversation::default();
    loop {
        if shared.cancelled() || cancellation.is_cancelled() {
            break;
        }
        if core.runtime.is_revoked() {
            *shared.terminal.lock().expect("observation error mutex") = Some(CoreError::Revoked);
            break;
        }
        let connection = run_connection(
            &core,
            &key,
            &observation_id,
            generation,
            interests,
            fresh,
            &mut last_watch,
            &mut conversation,
            &events,
            &cancellation,
            questions.as_ref(),
        );
        let result = tokio::select! {
            biased;
            () = wait_for_cloud_session_end(cloud_session.as_deref().map(|id| (&core, id))) => {
                Err(CoreError::AuthenticationRequired)
            }
            result = connection => result,
        };
        if matches!(result, Err(CoreError::AuthenticationRequired)) {
            *shared.terminal.lock().expect("observation error mutex") =
                Some(CoreError::AuthenticationRequired);
            shared.release_admission();
            break;
        }
        match result {
            Ok(End::Done) => break,
            Ok(End::Reconnect) => {
                signal_lost(questions.as_ref(), "observation_interrupted");
                let _ = core
                    .runtime
                    .set_state(&key.host_id, ConnectionState::Reconnecting);
                fresh = true;
                wait_to_reconnect(&core, &key, cloud_session.as_deref()).await;
            }
            Err(error) if !watch_fatal(&error) => {
                signal_lost(questions.as_ref(), error.code());
                // Keep the latest transient failure visible while the retry
                // loop is still running, so a bounded caller reports the
                // actionable cause instead of a bare timeout.
                *shared.last.lock().expect("observation error mutex") = Some(error);
                let _ = core
                    .runtime
                    .set_state(&key.host_id, ConnectionState::Reconnecting);
                fresh = true;
                wait_to_reconnect(&core, &key, cloud_session.as_deref()).await;
            }
            Err(error) => {
                signal_lost(questions.as_ref(), error.code());
                *shared.terminal.lock().expect("observation error mutex") = Some(error);
                break;
            }
        }
    }
    if let Some(watch_id) = last_watch.filter(|_| {
        cloud_session
            .as_deref()
            .is_none_or(|id| same_cloud_session(&core, id))
    }) {
        let cleanup = core.query(
            &key.host_id,
            whalerein_proto::RequestPayload {
                payload: Some(request_payload::Payload::UnwatchSession(UnwatchSession {
                    watch_id,
                })),
            },
        );
        let _ = tokio::time::timeout(Duration::from_millis(250), cleanup).await;
    }
}

async fn wait_to_reconnect(core: &Core, key: &SessionKey, session: Option<&str>) {
    let delay = core.runtime.next_reconnect_delay(&key.host_id);
    tokio::select! {
        () = wait_for_cloud_session_end(session.map(|id| (core, id))) => {}
        () = tokio::time::sleep(delay) => {}
    }
}

enum End {
    Done,
    Reconnect,
}

fn signal_lost(questions: Option<&mpsc::UnboundedSender<QuestionSignal>>, code: &str) {
    if let Some(questions) = questions {
        let _ = questions.send(QuestionSignal::TransportLost {
            code: code.to_owned(),
        });
    }
}

fn signal_descriptor(
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
    descriptor: QuestionDescriptor,
) {
    if let Some(questions) = questions {
        let _ = questions.send(QuestionSignal::Descriptor(Box::new(descriptor)));
    }
}

async fn send_event(
    events: &mpsc::Sender<SessionEvent>,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    payload: SessionEventPayload,
) -> bool {
    events
        .send(SessionEvent {
            session: key.clone(),
            observation_id: observation_id.to_owned(),
            generation,
            payload,
        })
        .await
        .is_ok()
}

async fn emit_question(
    events: &mpsc::Sender<SessionEvent>,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    view: QuestionView,
) -> bool {
    send_event(
        events,
        key,
        observation_id,
        generation,
        SessionEventPayload::Questions(view),
    )
    .await
}

struct EventSink<'a> {
    events: &'a mpsc::Sender<SessionEvent>,
    key: &'a SessionKey,
    observation_id: &'a str,
    generation: u64,
    failed: bool,
}

impl UpdateSink for EventSink<'_> {
    fn emit<'a>(&'a mut self, update: &'a ViewUpdate) -> SinkFuture<'a> {
        Box::pin(async move {
            let sent = send_event(
                self.events,
                self.key,
                self.observation_id,
                self.generation,
                SessionEventPayload::Conversation(update.clone()),
            )
            .await;
            if !sent {
                self.failed = true;
            }
            sent
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_connection(
    core: &Core,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    interests: ObservationInterests,
    fresh: bool,
    last_watch: &mut Option<String>,
    conversation: &mut Conversation,
    events: &mpsc::Sender<SessionEvent>,
    cancellation: &Cancellation,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
) -> CoreResult<End> {
    let channel = core.ensure_channel(&key.host_id, fresh).await?;
    let config = core.require_config()?;
    // The secure channel is only one part of observation. Until the device MQTT
    // subscription and baseline are ready, report reconnecting rather than
    // online, so a broker outage cannot look healthy through HTTPS alone.
    if fresh {
        let _ = core
            .runtime
            .set_state(&key.host_id, ConnectionState::Reconnecting);
    }
    let regional = core.cloud_transport().await?;
    let token = match &regional {
        Some(session) => session.token.clone(),
        None => load_device_token(core.secret_store(), &config.device_id)?
            .ok_or(CoreError::NotPaired)?,
    };
    let mut client = DeviceMqttClient::new(
        core.mqtt_settings(&config)?,
        &key.host_id,
        &config.device_id,
        token,
    )
    .with_trace_id(core.diagnostic_id());
    let session_id = regional.map(|session| session.id);
    if let Some(id) = &session_id {
        client = client.with_cloud_session(id.clone());
    }
    // A snapshot attach can take several pages; buffers must absorb the live
    // frames that arrive meanwhile instead of dropping previews.
    let (tx, rx) = mpsc::channel(1024);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let mut ready_tx = Some(ready_tx);
    let stop = Arc::new(AtomicBool::new(false));
    let task_stop = stop.clone();
    let runtime = core.runtime.clone();
    let state_host = key.host_id.clone();
    let interrupted = Arc::new(AtomicBool::new(false));
    let task_interrupted = interrupted.clone();
    let queued = Arc::new(AtomicUsize::new(0));
    let task_queued = queued.clone();
    let error = Arc::new(Mutex::new(None));
    let task_error = error.clone();
    let overflow = Arc::new(AtomicBool::new(false));
    let task_overflow = overflow.clone();
    let transport_core = core.clone();
    let task = tokio::spawn(async move {
        let stop_requested = || {
            task_stop.load(Ordering::SeqCst)
                || runtime.is_revoked()
                || session_id.as_ref().is_some_and(|id| {
                    transport_core.cloud_session_id().ok().flatten().as_ref() != Some(id)
                })
        };
        let mqtt = client.run(
            stop_requested,
            |envelope| {
                let bytes = envelope.ciphertext.len();
                if task_queued.fetch_add(bytes, Ordering::SeqCst) + bytes > 32 * 1024 * 1024 {
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
                    // The transport is down now, not when the reconnect
                    // loop happens to wake. Publish that immediately so a
                    // connection poller cannot keep reporting online.
                    let _ = runtime.set_state(&state_host, ConnectionState::Reconnecting);
                }
                _ => {}
            },
        );
        let result = if let Some(id) = &session_id {
            let renew = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if stop_requested() {
                        return;
                    }
                    match transport_core.cloud_transport().await {
                        Ok(Some(active))
                            if active.id == *id
                                && active.permission.as_ref().is_some_and(|permit| {
                                    permit.access_origin.as_str() == config.access_url
                                }) => {}
                        Err(CoreError::Unavailable(_) | CoreError::Timeout) => {}
                        _ => return,
                    }
                }
            };
            tokio::select! {
                result = mqtt => result,
                () = renew => Ok(()),
            }
        } else {
            mqtt.await
        };
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
    let response = core
        .query(
            &key.host_id,
            whalerein_proto::RequestPayload {
                payload: Some(request_payload::Payload::WatchSession(WatchSession {
                    session_id: key.session_id.clone(),
                    interests: Some(interests.into()),
                })),
            },
        )
        .await?;
    let Some(response_payload::Payload::WatchReady(ready)) = response.payload else {
        return Err(CoreError::Security("unexpected watch response".into()));
    };
    *last_watch = Some(ready.watch_id.clone());
    let wire_key = whalerein_proto::SessionKey::from(key);
    if ready.watch_id.is_empty() {
        return Err(CoreError::Security("invalid watch baseline".into()));
    }
    let initial = match ready.state {
        Some(state) if state.session.as_ref() != Some(&wire_key) => {
            return Err(CoreError::Security("invalid watch baseline".into()));
        }
        state => state,
    };
    if interests.conversation {
        let Some(initial) = &initial else {
            return Err(CoreError::Security("missing watch baseline".into()));
        };
        if initial.live_epoch.is_empty() {
            return Err(CoreError::Security("invalid watch baseline".into()));
        }
    }
    // Registration precedes baseline reads. A missing question descriptor is a
    // protocol error for a questions interest, never an empty collection.
    if interests.questions {
        let descriptor = ready
            .questions
            .or_else(|| initial.as_ref().and_then(|state| state.questions.clone()))
            .ok_or_else(|| CoreError::Security("missing question descriptor".into()))?;
        signal_descriptor(questions, descriptor);
    }
    core.record_journal("core", "watch_ready", "succeeded", "none");
    let epoch = initial
        .as_ref()
        .map(|state| state.live_epoch.clone())
        .unwrap_or_default();
    let state_revision = initial.as_ref().map_or(0, |state| state.revision);
    if interests.conversation {
        let initial = initial.as_ref().expect("conversation baseline");
        conversation.reset(&epoch);
        // Anything already in the local committed view predates this watch:
        // it is baseline, not output, and a reconnect must not replay it.
        for message_id in core.sync_service()?.history_message_ids(key)? {
            conversation.mark_delivered_id(&message_id);
        }
        let mut sink = EventSink {
            events,
            key,
            observation_id,
            generation,
            failed: false,
        };
        if !sink.emit(&ViewUpdate::Reset).await {
            pump.finish().await;
            return Ok(End::Done);
        }
        core.sync_conversation(key, conversation, false, false, &mut sink)
            .await?;
        let _ = core
            .runtime
            .set_state(&key.host_id, ConnectionState::Connected);
        if sink.failed || !sink.emit(&ViewUpdate::State(initial.clone().into())).await {
            pump.finish().await;
            return Ok(End::Done);
        }
    }
    let sync = if interests.conversation {
        Some(core.sync_service()?)
    } else {
        None
    };
    conversation_loop(
        core,
        key,
        observation_id,
        generation,
        interests,
        wire_key,
        ready.watch_id,
        channel,
        epoch,
        state_revision,
        conversation,
        rx,
        events,
        cancellation,
        questions,
        pump,
        interrupted,
        overflow,
        error,
        queued,
        sync,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn conversation_loop(
    core: &Core,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    interests: ObservationInterests,
    wire_key: whalerein_proto::SessionKey,
    watch_id: String,
    channel: crate::security::SecureChannel,
    epoch: String,
    initial_revision: u64,
    conversation: &mut Conversation,
    mut rx: mpsc::Receiver<EventEnvelope>,
    events: &mpsc::Sender<SessionEvent>,
    cancellation: &Cancellation,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
    pump: Pump,
    interrupted: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    error: Arc<Mutex<Option<CoreError>>>,
    queued: Arc<AtomicUsize>,
    sync: Option<crate::sync::SyncService>,
) -> CoreResult<End> {
    let mut state_revision = initial_revision;
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.tick().await;
    loop {
        if cancellation.is_cancelled() {
            pump.finish().await;
            return Ok(End::Done);
        }
        if core.runtime.is_revoked() {
            pump.finish().await;
            return Err(CoreError::Revoked);
        }
        if overflow.swap(false, Ordering::SeqCst) {
            // A dropped live frame or question hint would leave a hole; the
            // owner reconnects and re-registers from current authority.
            pump.finish().await;
            return Ok(End::Reconnect);
        }
        if interrupted.load(Ordering::SeqCst) {
            let failure = error.lock().expect("watch error mutex").take();
            if let Some(failure) = failure {
                pump.finish().await;
                return Err(failure);
            }
            pump.finish().await;
            return Ok(End::Reconnect);
        }
        tokio::select! {
            envelope = rx.recv() => {
                let Some(envelope) = envelope else {
                    let failure = error.lock().expect("watch error mutex").take();
                    if let Some(failure) = failure {
                        return Err(failure);
                    }
                    pump.finish().await;
                    return Ok(End::Reconnect);
                };
                queued.fetch_sub(envelope.ciphertext.len(), Ordering::SeqCst);
                let update = crate::sync::open_update(&channel, &envelope)?;
                if update.session.as_ref() != Some(&wire_key) || update.watch_id != watch_id {
                    continue;
                }
                let Some(payload) = update.payload else {
                    return Err(CoreError::Security("empty typed update".into()));
                };
                match handle_update(
                    core,
                    key,
                    observation_id,
                    generation,
                    interests,
                    &wire_key,
                    &watch_id,
                    &epoch,
                    &mut state_revision,
                    conversation,
                    events,
                    questions,
                    sync.as_ref(),
                    payload,
                )
                .await?
                {
                    Flow::Continue => {}
                    Flow::Done => {
                        pump.finish().await;
                        return Ok(End::Done);
                    }
                    Flow::Reconnect => {
                        pump.finish().await;
                        return Ok(End::Reconnect);
                    }
                }
            }
            _ = interval.tick() => {
                // The encrypted stream cannot carry an Access-authored close,
                // and a removed grant can leave it idle. Renew the authority
                // lease over HTTP; a typed reason is terminal immediately.
                if let Err(authority) = core.check_host_authority(&key.host_id).await {
                    if matches!(authority, CoreError::AuthorityClosed(_)) {
                        return Err(authority);
                    }
                }
                let failure = error.lock().expect("watch error mutex").take();
                    if let Some(failure) = failure {
                    return Err(failure);
                }
                let response = core.query(
                    &key.host_id,
                    whalerein_proto::RequestPayload {
                        payload: Some(request_payload::Payload::RefreshWatches(RefreshWatches {
                            watch_ids: vec![watch_id.clone()],
                        })),
                    },
                ).await;
                let response = match response {
                    Err(error) if error.code() == "expired" => {
                        pump.finish().await;
                        return Ok(End::Reconnect);
                    }
                    other => other?,
                };
                let Some(response_payload::Payload::WatchStates(states)) = response.payload else {
                    return Err(CoreError::Security("unexpected watch refresh".into()));
                };
                let refreshed = states.watches.into_iter().find(|watch| watch.watch_id == watch_id)
                    .ok_or_else(|| CoreError::Security("missing watch refresh".into()))?;
                if interests.questions {
                    if let Some(descriptor) = refreshed.questions {
                        signal_descriptor(questions, descriptor);
                    } else if let Some(state) = &refreshed.state {
                        if let Some(descriptor) = state.questions.clone() {
                            signal_descriptor(questions, descriptor);
                        }
                    }
                }
                if !interests.conversation {
                    continue;
                }
                let Some(state) = refreshed.state else {
                    continue;
                };
                if state.session.as_ref() != Some(&wire_key) {
                    return Err(CoreError::Security("watch refresh scope mismatch".into()));
                }
                if state.live_epoch != epoch {
                    pump.finish().await;
                    return Ok(End::Reconnect);
                }
                if let Some(sync) = sync.as_ref() {
                    if sync.cursor(key)?.as_deref() != Some(state.history_head.as_str()) {
                        let mut sink = EventSink {
                            events,
                            key,
                            observation_id,
                            generation,
                            failed: false,
                        };
                        core.sync_conversation(key, conversation, true, false, &mut sink)
                            .await?;
                        if sink.failed {
                            pump.finish().await;
                            return Ok(End::Done);
                        }
                    }
                }
                if interests.conversation
                    && state.revision > state_revision
                    && !send_event(
                        events,
                        key,
                        observation_id,
                        generation,
                        SessionEventPayload::Conversation(ViewUpdate::State(state.into())),
                    )
                    .await
                {
                    pump.finish().await;
                    return Ok(End::Done);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_update(
    core: &Core,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    interests: ObservationInterests,
    wire_key: &whalerein_proto::SessionKey,
    watch_id: &str,
    epoch: &str,
    state_revision: &mut u64,
    conversation: &mut Conversation,
    events: &mpsc::Sender<SessionEvent>,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
    sync: Option<&crate::sync::SyncService>,
    payload: session_update::Payload,
) -> CoreResult<Flow> {
    let _ = watch_id;
    match payload {
        session_update::Payload::Questions(descriptor) => {
            if interests.questions {
                signal_descriptor(questions, descriptor);
            }
            Ok(Flow::Continue)
        }
        session_update::Payload::Live(frame) => {
            if !interests.conversation {
                return Ok(Flow::Continue);
            }
            let Some(view) = conversation.live(&frame)? else {
                return Ok(Flow::Continue);
            };
            Ok(
                if send_event(
                    events,
                    key,
                    observation_id,
                    generation,
                    SessionEventPayload::Conversation(view),
                )
                .await
                {
                    Flow::Continue
                } else {
                    Flow::Done
                },
            )
        }
        session_update::Payload::History(batch) => {
            if !interests.conversation {
                return Ok(Flow::Continue);
            }
            let Some(sync) = sync else {
                return Err(CoreError::Internal(
                    "conversation sync is unavailable".into(),
                ));
            };
            match sync.apply_batch(key, &batch)? {
                BatchOutcome::Applied => {
                    for change in &batch.changes {
                        let Some(view) = conversation.committed_once(change)? else {
                            continue;
                        };
                        if !send_event(
                            events,
                            key,
                            observation_id,
                            generation,
                            SessionEventPayload::Conversation(view),
                        )
                        .await
                        {
                            return Ok(Flow::Done);
                        }
                    }
                }
                BatchOutcome::Duplicate => {}
                BatchOutcome::Gap => {
                    let mut sink = EventSink {
                        events,
                        key,
                        observation_id,
                        generation,
                        failed: false,
                    };
                    core.sync_conversation(key, conversation, true, false, &mut sink)
                        .await?;
                    if sink.failed {
                        return Ok(Flow::Done);
                    }
                }
            }
            Ok(Flow::Continue)
        }
        session_update::Payload::State(state) => {
            if state.session.as_ref() != Some(wire_key) {
                return Err(CoreError::Security("state update scope mismatch".into()));
            }
            if interests.questions {
                if let Some(descriptor) = state.questions.clone() {
                    signal_descriptor(questions, descriptor);
                }
            }
            if !interests.conversation {
                return Ok(Flow::Continue);
            }
            if state.live_epoch != epoch {
                return Ok(Flow::Reconnect);
            }
            if state.revision > *state_revision {
                *state_revision = state.revision;
                if !send_event(
                    events,
                    key,
                    observation_id,
                    generation,
                    SessionEventPayload::Conversation(ViewUpdate::State(state.into())),
                )
                .await
                {
                    return Ok(Flow::Done);
                }
            }
            Ok(Flow::Continue)
        }
        session_update::Payload::Reset(_) => {
            if !interests.conversation {
                return Ok(Flow::Continue);
            }
            conversation.reset("");
            core.sync_service()?.discard_stage(key)?;
            Ok(Flow::Reconnect)
        }
    }
}

enum Flow {
    Continue,
    Done,
    Reconnect,
}

struct CoreQuestionSource {
    core: Core,
    key: SessionKey,
}

impl QuestionPageSource for CoreQuestionSource {
    fn page<'a>(&'a self, cursor: &'a str) -> PageFuture<'a> {
        Box::pin(async move { self.core.read_questions(&self.key, cursor).await })
    }
}

#[derive(Debug)]
enum Applied {
    Stop,
    Ignore,
    Emit(QuestionView),
    Retry,
}

fn apply_question_signal(
    reconciler: &mut QuestionReconciler,
    signal: Option<QuestionSignal>,
) -> Applied {
    let Some(signal) = signal else {
        return Applied::Stop;
    };
    match signal {
        QuestionSignal::Descriptor(descriptor) => {
            match reconciler.observe_descriptor(&descriptor) {
                Step::Idle => Applied::Ignore,
                Step::Emit(view) => Applied::Emit(view),
                Step::Reconcile => Applied::Retry,
            }
        }
        QuestionSignal::TransportLost { code } => Applied::Emit(reconciler.transport_lost(&code)),
    }
}

async fn run_questions(
    core: Core,
    key: SessionKey,
    observation_id: String,
    generation: u64,
    events: mpsc::Sender<SessionEvent>,
    mut signals: mpsc::UnboundedReceiver<QuestionSignal>,
    cancellation: Cancellation,
) {
    let source = CoreQuestionSource {
        core,
        key: key.clone(),
    };
    let mut reconciler = QuestionReconciler::new();
    if !emit_question(
        &events,
        &key,
        &observation_id,
        generation,
        QuestionView::loading(),
    )
    .await
    {
        return;
    }
    loop {
        let signal = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return,
            signal = signals.recv() => signal,
        };
        match apply_question_signal(&mut reconciler, signal) {
            Applied::Stop => return,
            Applied::Ignore => continue,
            Applied::Emit(view) => {
                if !emit_question(&events, &key, &observation_id, generation, view).await {
                    return;
                }
            }
            Applied::Retry => {
                if !reconcile_questions(
                    &mut reconciler,
                    &source,
                    &mut signals,
                    &cancellation,
                    &events,
                    &key,
                    &observation_id,
                    generation,
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_questions(
    reconciler: &mut QuestionReconciler,
    source: &CoreQuestionSource,
    signals: &mut mpsc::UnboundedReceiver<QuestionSignal>,
    cancellation: &Cancellation,
    events: &mpsc::Sender<SessionEvent>,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
) -> bool {
    const MAX_RESTARTS: usize = 4;
    let mut restarts = 0usize;
    'reconcile: loop {
        match reconciler.begin() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let view = reconciler
                    .accepted_view()
                    .unwrap_or_else(QuestionView::loading);
                return emit_question(events, key, observation_id, generation, view).await;
            }
            Err(view) => return emit_question(events, key, observation_id, generation, view).await,
        }
        loop {
            let Some(cursor) = reconciler.read_cursor() else {
                return true;
            };
            let mut page = source.page(&cursor);
            let outcome = loop {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return true,
                    signal = signals.recv() => {
                        match apply_question_signal(reconciler, signal) {
                            Applied::Stop => return true,
                            Applied::Ignore => continue,
                            Applied::Emit(view) => {
                                reconciler.abort_stage();
                                return emit_question(events, key, observation_id, generation, view).await;
                            }
                            Applied::Retry => {
                                reconciler.abort_stage();
                                restarts += 1;
                                if restarts > MAX_RESTARTS {
                                    let view = stale_view(reconciler, "question_reconciliation_churn");
                                    return emit_question(events, key, observation_id, generation, view).await;
                                }
                                continue 'reconcile;
                            }
                        }
                    }
                    page = &mut page => break page,
                }
            };
            match outcome {
                Ok(page) => match reconciler.accept_page(page) {
                    questions::PageStep::Read => continue,
                    questions::PageStep::Installed(view) => {
                        return emit_question(events, key, observation_id, generation, view).await
                    }
                    questions::PageStep::Restart(_) => {
                        reconciler.abort_stage();
                        restarts += 1;
                        if restarts > MAX_RESTARTS {
                            let view = stale_view(reconciler, "question_reconciliation_churn");
                            return emit_question(events, key, observation_id, generation, view)
                                .await;
                        }
                        continue 'reconcile;
                    }
                    questions::PageStep::Failed(code) => {
                        reconciler.abort_stage();
                        let view = stale_view(reconciler, &code);
                        return emit_question(events, key, observation_id, generation, view).await;
                    }
                },
                Err(error) => {
                    reconciler.abort_stage();
                    let view = stale_view(reconciler, error.code());
                    return emit_question(events, key, observation_id, generation, view).await;
                }
            }
        }
    }
}

fn stale_view(reconciler: &QuestionReconciler, code: &str) -> QuestionView {
    match reconciler.accepted_view() {
        Some(current) => QuestionView::stale(current.stamp, current.batches, code),
        None => QuestionView::unavailable(None, Vec::new(), code),
    }
}

#[cfg(test)]
mod tests;
