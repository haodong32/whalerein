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

use prost::Message;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use whalerein_proto::{
    request_payload, response_payload, session_update, EventEnvelope, QuestionDescriptor,
    RefreshWatches, UnwatchSession, WatchSession,
};

use crate::access::DeliveryClass;
use crate::api::core::watch::{SinkFuture, UpdateSink};
use crate::api::{Cancellation, ObservationInterests, SessionEvent, SessionEventPayload};
use crate::api::{Core, CoreError, CoreResult, QuestionView};
use crate::connection::SubscriptionGuard;
use crate::conversation::{Conversation, ViewUpdate};
use crate::runtime::ConnectionState;
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
    // A deterministic failure (an oversized or malformed response) is
    // guaranteed to repeat: rebuilding the connection cannot change the
    // outcome, and looping would present it as a network problem.
    if matches!(error.code(), "payload_too_large" | "invalid_argument") {
        return true;
    }
    if matches!(
        error.code(),
        // A scope denial is an authoritative answer, not a transport problem.
        // Reconnecting would only repeat the same 401/403 until the deadline,
        // hiding the revocation from a bounded caller.
        "unsupported_capability" | "protocol_version_incompatible" | "unauthorized" | "forbidden"
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
            | CoreError::Security(_)
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

/// Session-side handles for the shared Host connection. Dropping the pump
/// stops the region watch; the connection outlives it.
struct Pump {
    renewal: Option<JoinHandle<()>>,
    /// Keeps this observer registered with the shared connection.
    _detach: SubscriptionGuard,
}

impl Drop for Pump {
    fn drop(&mut self) {
        if let Some(renewal) = self.renewal.take() {
            renewal.abort();
        }
    }
}

impl Pump {
    async fn finish(mut self) {
        if let Some(renewal) = self.renewal.take() {
            renewal.abort();
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
    // The transport belongs to the Host, not to this session. Attaching here
    // only subscribes to it; the connection keeps running for other sessions
    // and for the app-level monitor.
    let regional = core.cloud_transport().await?;
    let connection = core
        .host_connection_with(&key.host_id, regional.clone())
        .await?;
    let (subscription, detach) = connection.subscribe();
    let interrupted = subscription.interrupted.clone();
    let overflow = subscription.overflow.clone();
    let live_overflow = subscription.live_overflow.clone();
    let error = subscription.error.clone();
    let queued = subscription.queued.clone();
    let live_queued = subscription.live_queued.clone();
    let control_rx = subscription.control;
    let live_rx = subscription.live;
    connection.wait_ready(cancellation).await?;
    // A cloud region change invalidates the token this transport connected
    // with. The watch notices it and closes the shared connection; the
    // binding re-attaches when the session settles.
    let renewal = regional.map(|session| {
        let core = core.clone();
        let host_id = key.host_id.clone();
        let expected = session.id;
        let access_url = core
            .require_config()
            .map(|config| config.access_url)
            .unwrap_or_default();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                match core.cloud_transport().await {
                    Ok(Some(active))
                        if active.id == expected
                            && active.permission.as_ref().is_some_and(|permit| {
                                permit.access_origin.as_str() == access_url
                            }) => {}
                    Err(CoreError::Unavailable(_) | CoreError::Timeout) => {}
                    _ => {
                        core.disconnect_host(&host_id);
                        return;
                    }
                }
            }
        })
    });
    let pump = Pump {
        renewal,
        _detach: detach,
    };
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
    core.diagnostics
        .record_stage("core", "watch_ready", "succeeded", "none");
    let epoch = initial
        .as_ref()
        .map(|state| state.live_epoch.clone())
        .unwrap_or_default();
    let state_revision = initial.as_ref().map_or(0, |state| state.revision);
    let projection = initial
        .as_ref()
        .map(|state| state.projection_id.clone())
        .unwrap_or_default();
    // The last approvals delivered to the viewer. A bounded read that fails
    // transiently must not publish an empty list, so the cache survives until
    // a later read succeeds.
    let mut approvals = ApprovalCache::default();
    if interests.conversation {
        let initial = initial.as_ref().expect("conversation baseline");
        if projection.is_empty() || projection.len() > 128 {
            return Err(CoreError::Security(
                "missing watch content projection".into(),
            ));
        }
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
        core.sync_conversation(key, conversation, false, false, &projection, &mut sink)
            .await?;
        let _ = core
            .runtime
            .set_state(&key.host_id, ConnectionState::Connected);
        // Approvals are content and never ride the baseline; they are read
        // through their own bounded query. A failed read must not erase the
        // pending approvals a later event or lease tick will restore, so the
        // baseline is emitted only with what Core actually read.
        approvals.items = fetch_approvals(core, key).await.unwrap_or_default();
        let mut baseline: crate::conversation::SessionState = initial.clone().into();
        baseline.approvals = approvals.items.clone();
        if sink.failed || !sink.emit(&ViewUpdate::State(baseline)).await {
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
        projection,
        conversation,
        approvals,
        control_rx,
        live_rx,
        events,
        cancellation,
        questions,
        pump,
        interrupted,
        overflow,
        live_overflow,
        error,
        queued,
        live_queued,
        sync,
    )
    .await
}

/// The approvals last delivered to the viewer, kept so a transiently failed
/// bounded read cannot erase a pending approval. `stale` asks the next lease
/// tick to retry the read that failed.
#[derive(Debug, Default)]
struct ApprovalCache {
    items: Vec<crate::conversation::Approval>,
    stale: bool,
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
    mut projection: String,
    conversation: &mut Conversation,
    mut approvals: ApprovalCache,
    mut control_rx: mpsc::Receiver<(DeliveryClass, EventEnvelope)>,
    mut live_rx: mpsc::Receiver<(DeliveryClass, EventEnvelope)>,
    events: &mpsc::Sender<SessionEvent>,
    cancellation: &Cancellation,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
    pump: Pump,
    interrupted: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    live_overflow: Arc<AtomicBool>,
    error: Arc<Mutex<Option<CoreError>>>,
    queued: Arc<AtomicUsize>,
    live_queued: Arc<AtomicUsize>,
    sync: Option<crate::sync::SyncService>,
) -> CoreResult<End> {
    let mut live_open = true;
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
            // A dropped control frame would leave a durable hole; the owner
            // reconnects and re-registers from current authority.
            pump.finish().await;
            return Ok(End::Reconnect);
        }
        if live_overflow.swap(false, Ordering::SeqCst) {
            // A dropped live preview only makes derived display state
            // incomplete. It never reconnects, retries or blocks control.
            conversation.mark_preview_incomplete();
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
            biased;
            received = control_rx.recv() => {
                let Some((class, envelope)) = received else {
                    let failure = error.lock().expect("watch error mutex").take();
                    if let Some(failure) = failure {
                        return Err(failure);
                    }
                    pump.finish().await;
                    return Ok(End::Reconnect);
                };
                queued.fetch_sub(envelope.encoded_len(), Ordering::SeqCst);
                match handle_envelope(
                    core,
                    key,
                    observation_id,
                    generation,
                    interests,
                    &wire_key,
                    &watch_id,
                    class,
                    &epoch,
                    &channel,
                    &mut state_revision,
                    &mut projection,
                    conversation,
                    &mut approvals,
                    events,
                    questions,
                    sync.as_ref(),
                    &envelope,
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
            received = live_rx.recv(), if live_open => {
                let Some((class, envelope)) = received else {
                    // The live sender ended with the same MQTT task that owns
                    // control; control completion determines the loop's end.
                    live_open = false;
                    continue;
                };
                live_queued.fetch_sub(envelope.encoded_len(), Ordering::SeqCst);
                match handle_envelope(
                    core,
                    key,
                    observation_id,
                    generation,
                    interests,
                    &wire_key,
                    &watch_id,
                    class,
                    &epoch,
                    &channel,
                    &mut state_revision,
                    &mut projection,
                    conversation,
                    &mut approvals,
                    events,
                    questions,
                    sync.as_ref(),
                    &envelope,
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
                let renewal = renew_watch_lease(core, key, &watch_id).await;
                let lease = match renewal {
                    LeaseRenewal::Extended(lease) => lease,
                    // The Host no longer holds this observation; only a fresh
                    // attach can restore a consistent baseline.
                    LeaseRenewal::Reattach => {
                        pump.finish().await;
                        return Ok(End::Reconnect);
                    }
                    // A deterministic lease failure is not a connectivity
                    // event. The caller sees the real cause instead of an
                    // endless reconnect.
                    LeaseRenewal::Terminal(failure) => return Err(failure),
                    // A transient failure leaves the stream untouched: the
                    // lease still has room, so the next tick retries on this
                    // connection instead of building a new one.
                    LeaseRenewal::Retry => continue,
                };
                if interests.questions {
                    if let Some(descriptor) = lease.questions {
                        signal_descriptor(questions, descriptor);
                    }
                }
                if !interests.conversation {
                    continue;
                }
                if lease.projection_id.is_empty() || lease.projection_id != projection {
                    if !send_event(
                        events,
                        key,
                        observation_id,
                        generation,
                        SessionEventPayload::Conversation(conversation.reset_projection("")),
                    )
                    .await
                    {
                        pump.finish().await;
                        return Ok(End::Done);
                    }
                    pump.finish().await;
                    return Ok(End::Reconnect);
                }
                if lease.live_epoch != epoch {
                    pump.finish().await;
                    return Ok(End::Reconnect);
                }
                // A newer revision without a delivered state event means a
                // hint was lost; an approvals read left stale by an earlier
                // state event is retried here for the same revision. Rebuild
                // the same bounded state the event would have carried: markers
                // from the lease and approvals from their own paged query.
                let rebuild = lease.revision > state_revision
                    || (approvals.stale && lease.revision >= state_revision);
                if rebuild {
                    if let Some(fresh) = fetch_approvals(core, key).await {
                        approvals.items = fresh;
                        approvals.stale = false;
                        state_revision = lease.revision;
                        let state = crate::conversation::SessionState {
                            session: Some(wire_key.clone().into()),
                            live_epoch: lease.live_epoch.clone(),
                            revision: lease.revision as i64,
                            history_head: lease.history_head.clone(),
                            projection_id: lease.projection_id.clone(),
                            run_phase: crate::conversation::RunPhase::try_from(lease.run_phase)
                                .unwrap_or_default(),
                            approvals: approvals.items.clone(),
                        };
                        if !send_event(
                            events,
                            key,
                            observation_id,
                            generation,
                            SessionEventPayload::Conversation(ViewUpdate::State(state)),
                        )
                        .await
                        {
                            pump.finish().await;
                            return Ok(End::Done);
                        }
                    }
                }
                if let Some(sync) = sync.as_ref() {
                    // A moved history head means committed content was missed.
                    // Repair it through the paged history read, which keeps
                    // every response bounded.
                    if sync.cursor(key)?.as_deref() != Some(lease.history_head.as_str()) {
                        let mut sink = EventSink {
                            events,
                            key,
                            observation_id,
                            generation,
                            failed: false,
                        };
                        core.sync_conversation(
                            key,
                            conversation,
                            true,
                            false,
                            &projection,
                            &mut sink,
                        )
                        .await?;
                        if sink.failed {
                            pump.finish().await;
                            return Ok(End::Done);
                        }
                    }
                }
            }
        }
    }
}

/// Fetch the pending approvals of this session through their own bounded
/// query. `None` means the read failed; the next lease tick retries it rather
/// than emitting a state that would erase visible approvals.
async fn fetch_approvals(
    core: &Core,
    key: &SessionKey,
) -> Option<Vec<crate::conversation::Approval>> {
    let response = core
        .query(
            &key.host_id,
            whalerein_proto::RequestPayload {
                payload: Some(request_payload::Payload::ListApprovals(
                    whalerein_proto::ListApprovals {
                        session_id: key.session_id.clone(),
                    },
                )),
            },
        )
        .await
        .ok()?;
    let Some(response_payload::Payload::ApprovalList(list)) = response.payload else {
        return None;
    };
    Some(list.approvals.into_iter().map(Into::into).collect())
}

/// Outcome of one lease renewal.
enum LeaseRenewal {
    /// The lease was extended and carries only divergence markers.
    Extended(whalerein_proto::WatchLease),
    /// The Host no longer owns this observation; re-attach for a baseline.
    Reattach,
    /// Deterministic failure: the caller must see it, never a reconnect.
    Terminal(CoreError),
    /// Transient failure: keep the stream and retry on the next tick.
    Retry,
}

/// Renew one watch lease. The Host answers from cached markers, so the
/// response is constant-sized no matter how large the transcript is.
async fn renew_watch_lease(core: &Core, key: &SessionKey, watch_id: &str) -> LeaseRenewal {
    let response = core
        .query(
            &key.host_id,
            whalerein_proto::RequestPayload {
                payload: Some(request_payload::Payload::RefreshWatches(RefreshWatches {
                    watch_ids: vec![watch_id.to_owned()],
                })),
            },
        )
        .await;
    let response = match response {
        Ok(response) => response,
        Err(core_error) => {
            return match core_error.code() {
                // The Host no longer holds the observation. A re-attach is the
                // only path that can restore a consistent baseline.
                "expired" => LeaseRenewal::Reattach,
                // An authoritative answer that the session is gone or that the
                // device lost the grant is terminal, even when the explicit
                // `watch_closed` event is delayed or lost.
                "not_found" => {
                    LeaseRenewal::Terminal(CoreError::AuthorityClosed("session_deleted".into()))
                }
                "forbidden" | "unauthorized" => {
                    LeaseRenewal::Terminal(CoreError::AuthorityClosed("unauthorized".into()))
                }
                // Retrying a deterministic refusal with a new connection
                // cannot succeed and would mask the real cause. `fix_input`
                // and `upgrade` codes are terminal for the current attempt,
                // exactly as WIRE.md classifies them.
                "payload_too_large" | "invalid_argument" => LeaseRenewal::Terminal(core_error),
                "unsupported_capability" | "protocol_version_incompatible" => {
                    LeaseRenewal::Terminal(core_error)
                }
                _ => LeaseRenewal::Retry,
            };
        }
    };
    let Some(response_payload::Payload::WatchStates(states)) = response.payload else {
        return LeaseRenewal::Terminal(CoreError::Security("unexpected watch refresh".into()));
    };
    match states
        .watches
        .into_iter()
        .find(|lease| lease.watch_id == watch_id)
    {
        Some(lease) => LeaseRenewal::Extended(lease),
        None => LeaseRenewal::Terminal(CoreError::Security("missing watch refresh".into())),
    }
}

/// Enforce the delivery-class payload contract after decryption. Control may
/// carry committed history, state, question and reset updates; only the live
/// class may carry a `LiveFrame`. The two classes never fall back to each
/// other, so a crossed payload is a stable security/protocol failure.
fn enforce_delivery_class(
    class: DeliveryClass,
    update: &whalerein_proto::SessionUpdate,
) -> CoreResult<()> {
    let live = matches!(update.payload, Some(session_update::Payload::Live(_)));
    match (class, live) {
        (DeliveryClass::Control, true) => Err(CoreError::Security(
            "live preview received on the control event topic".into(),
        )),
        (DeliveryClass::Live, false) => Err(CoreError::Security(
            "control payload received on the live topic".into(),
        )),
        _ => Ok(()),
    }
}

/// Resolve, decrypt and apply one received envelope through the class contract.
#[allow(clippy::too_many_arguments)]
async fn handle_envelope(
    core: &Core,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    interests: ObservationInterests,
    wire_key: &whalerein_proto::SessionKey,
    watch_id: &str,
    class: DeliveryClass,
    epoch: &str,
    channel: &crate::security::SecureChannel,
    state_revision: &mut u64,
    projection: &mut String,
    conversation: &mut Conversation,
    approvals: &mut ApprovalCache,
    events: &mpsc::Sender<SessionEvent>,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
    sync: Option<&crate::sync::SyncService>,
    envelope: &EventEnvelope,
) -> CoreResult<Flow> {
    // A referenced control event is fetched and verified over HTTPS before
    // AEAD; a partial or failed fetch never becomes an event. Live envelopes
    // are always inline, so this resolves to the same bytes without a fetch.
    let envelope = core.resolve_event_envelope(envelope).await?;
    let update = match crate::sync::open_event(channel, &envelope)? {
        crate::sync::OpenedEvent::Update(update) => update,
        crate::sync::OpenedEvent::Closed(closed) => {
            // A terminal close is control intent; it is never a live preview.
            if class != DeliveryClass::Control {
                return Err(CoreError::Security(
                    "control payload received on the live topic".into(),
                ));
            }
            if closed.watch_id == watch_id && closed.session.as_ref() == Some(wire_key) {
                // A Host-authored terminal close is authoritative, like an
                // Access authority reason: never reconnect.
                return Err(CoreError::AuthorityClosed(closed.reason));
            }
            return Ok(Flow::Continue);
        }
    };
    enforce_delivery_class(class, &update)?;
    if update.session.as_ref() != Some(wire_key) || update.watch_id != watch_id {
        return Ok(Flow::Continue);
    }
    let Some(payload) = update.payload else {
        return Err(CoreError::Security("empty typed update".into()));
    };
    handle_update(
        core,
        key,
        observation_id,
        generation,
        interests,
        wire_key,
        epoch,
        state_revision,
        projection,
        conversation,
        approvals,
        events,
        questions,
        sync,
        payload,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn handle_update(
    core: &Core,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    interests: ObservationInterests,
    wire_key: &whalerein_proto::SessionKey,
    epoch: &str,
    state_revision: &mut u64,
    projection: &mut String,
    conversation: &mut Conversation,
    approvals: &mut ApprovalCache,
    events: &mpsc::Sender<SessionEvent>,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
    sync: Option<&crate::sync::SyncService>,
    payload: session_update::Payload,
) -> CoreResult<Flow> {
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
            if batch.projection_id.is_empty() || batch.projection_id != *projection {
                conversation.reset_projection("");
                *projection = batch.projection_id.clone();
                return Ok(Flow::Reconnect);
            }
            let outcome = sync.apply_batch(key, &batch)?;
            match outcome {
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
                        for activity in conversation.take_activity_updates() {
                            if !send_event(
                                events,
                                key,
                                observation_id,
                                generation,
                                SessionEventPayload::Conversation(activity),
                            )
                            .await
                            {
                                return Ok(Flow::Done);
                            }
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
                    core.sync_conversation(key, conversation, true, false, projection, &mut sink)
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
            if state.projection_id.is_empty() || state.projection_id != *projection {
                conversation.reset_projection("");
                *projection = state.projection_id.clone();
                return Ok(Flow::Reconnect);
            }
            if state.live_epoch != epoch {
                return Ok(Flow::Reconnect);
            }
            if state.revision > *state_revision {
                *state_revision = state.revision;
                // A state event carries transition markers; approvals are
                // merged from their bounded query so they stay current without
                // riding the event. A transiently failed read keeps the last
                // emitted approvals and marks them stale so the next lease
                // tick retries instead of visibly erasing a pending approval.
                let mut next: crate::conversation::SessionState = state.into();
                match fetch_approvals(core, key).await {
                    Some(fresh) => {
                        approvals.items = fresh;
                        approvals.stale = false;
                    }
                    None => approvals.stale = true,
                }
                next.approvals = approvals.items.clone();
                if !send_event(
                    events,
                    key,
                    observation_id,
                    generation,
                    SessionEventPayload::Conversation(ViewUpdate::State(next)),
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
