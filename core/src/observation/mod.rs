//! Private session observation owner.
//!
//! One observation owns the profile OS watch lease, the single device MQTT
//! subscription and the conversation baseline. Conversation and Questions are
//! reconciled independently, so a failed or unsupported history facet never
//! blocks question readiness. The public handle only delivers typed
//! `SessionEvent`s; wire messages never leave this module.

pub mod bench;
mod history;
mod questions;

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use whalerein_proto::{
    request_payload, response_payload, session_update, CheckVersions, EventEnvelope,
    QuestionDescriptor, UnwatchSession, WatchSession,
};

use crate::access::DeliveryClass;
use crate::api::core::watch::{SinkFuture, UpdateSink};
use crate::api::{Cancellation, ObservationInterests, SessionEvent, SessionEventPayload};
use crate::api::{Core, CoreError, CoreResult, QuestionView};
use crate::connection::{QueuedEvent, SubscriptionGuard};
use crate::conversation::{Conversation, LiveDisposition, ViewUpdate};
use crate::observation::bench::{
    BenchContext, BenchEvent, DeliveryKind, DequeueDisposition, FrameRange, JoinMilestone,
    TokenDomain,
};
use crate::runtime::ConnectionState;
use crate::session::SessionKey;
use crate::sync::BatchOutcome;
use crate::util::random_id;

use history::{HistoryReadFuture, PagedHistory};
use questions::{PageFuture, QuestionPageSource, QuestionReconciler, Step};

/// Monotonic observer generation. One profile has one active observation, so a
/// process-local counter is enough to fence replacement within this Core.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Process-lifetime delivery ordinal for the public observation stream.
/// Distinct observations remain joinable because the ordinal is paired with
/// the process lifetime, not with a per-watch counter.
static NEXT_DELIVERY: AtomicU64 = AtomicU64::new(1);

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
    stop: Cancellation,
    terminal: Mutex<Option<CoreError>>,
    last: Mutex<Option<CoreError>>,
    admission: Mutex<Option<(WatchLease, WatchGuard)>>,
}

impl ObservationShared {
    fn cancelled(&self) -> bool {
        self.stop.is_cancelled()
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
        self.shared.stop.cancel();
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
        // Do not let a replacement attach before the old owner's bounded
        // Unwatch cleanup has ended.
        self.shared.release_admission();
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
        self.shared.stop.cancel();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.shared.release_admission();
    }
}

impl Core {
    /// Start one session observation selecting Conversation, Questions, or
    /// both. An authorized recent cache may be delivered immediately. Native
    /// watch registration precedes online baseline reads.
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
    let mut approvals = ApprovalCache::default();
    let mut paged = interests
        .recent_page
        .map(|limits| PagedHistory::new(&key, limits));
    if let Some(paged) = paged.as_mut() {
        let mut sink = paged_sink(&core, &events, &key, &observation_id, generation);
        if let Err(error) = paged
            .publish_cache(&core, &mut conversation, &mut sink)
            .await
        {
            sink.publish(SessionEventPayload::HistoryUnavailable {
                code: error.code().into(),
            })
            .await;
        }
    }
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
            paged.as_mut(),
            &mut approvals,
            &events,
            &cancellation,
            questions.as_ref(),
        );
        let result = tokio::select! {
            biased;
            () = wait_for_cloud_session_end(cloud_session.as_deref().map(|id| (&core, id))) => {
                Err(CoreError::AuthenticationRequired)
            }
            _ = cancellation.cancelled() => Ok(End::Done),
            _ = shared.stop.cancelled() => Ok(End::Done),
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
                tokio::select! {
                    _ = shared.stop.cancelled() => {}
                    _ = cancellation.cancelled() => {}
                    _ = wait_to_reconnect(&core, &key, cloud_session.as_deref()) => {}
                }
            }
            Err(error) if !watch_fatal(&error) => {
                if let Some(paged) = paged.as_mut() {
                    let mut sink = paged_sink(&core, &events, &key, &observation_id, generation);
                    paged.transport_unavailable(error.code(), &mut sink).await;
                }
                signal_lost(questions.as_ref(), error.code());
                // Keep the latest transient failure visible while the retry
                // loop is still running, so a bounded caller reports the
                // actionable cause instead of a bare timeout.
                *shared.last.lock().expect("observation error mutex") = Some(error);
                let _ = core
                    .runtime
                    .set_state(&key.host_id, ConnectionState::Reconnecting);
                fresh = true;
                tokio::select! {
                    _ = shared.stop.cancelled() => {}
                    _ = cancellation.cancelled() => {}
                    _ = wait_to_reconnect(&core, &key, cloud_session.as_deref()) => {}
                }
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
            delivery: None,
            preview_range: None,
            payload,
        })
        .await
        .is_ok()
}

/// Deliver one conversation update and stamp the actual delivery boundary.
///
/// The ordinal is internal metadata on `SessionEvent`; user-visible JSON never
/// carries it. `frame` is the opaque receipt token for the frame that produced
/// the update, or the sync operation token for a baseline page. `range` is the
/// tokenized live increment the update names, present only for a preview.
#[allow(clippy::too_many_arguments)]
async fn send_conversation(
    bench: &BenchContext,
    events: &mpsc::Sender<SessionEvent>,
    key: &SessionKey,
    observation_id: &str,
    generation: u64,
    update: ViewUpdate,
    frame: &str,
    range: Option<&FrameRange>,
) -> (bool, Option<u64>) {
    let capturing = bench.enabled();
    let kind = if capturing {
        delivered_kind(&update)
    } else {
        None
    };
    let bytes = if capturing {
        delivered_bytes(&update)
    } else {
        0
    };
    let preview_range = match (&update, range) {
        (ViewUpdate::Preview { .. }, Some(range)) => Some(range.clone()),
        _ => None,
    };
    let delivery_range = preview_range.clone();
    let ordinal = kind.map(|_| NEXT_DELIVERY.fetch_add(1, Ordering::SeqCst));
    let sent = events
        .send(SessionEvent {
            session: key.clone(),
            observation_id: observation_id.to_owned(),
            generation,
            delivery: ordinal,
            preview_range,
            payload: SessionEventPayload::Conversation(update),
        })
        .await
        .is_ok();
    if sent {
        if let (Some(kind), Some(ordinal)) = (kind, ordinal) {
            bench.emit(BenchEvent::CoreDelivery {
                frame: frame.to_owned(),
                delivery: ordinal,
                kind,
                bytes,
                stream: delivery_range.as_ref().map(|range| range.stream.clone()),
                block: delivery_range
                    .as_ref()
                    .and_then(|range| range.block.clone()),
                seq_start: delivery_range.as_ref().map(|range| range.seq_start),
                seq_end: delivery_range.as_ref().map(|range| range.seq_end),
                batch: delivery_range.as_ref().map(|range| range.batch.clone()),
            });
        }
    }
    (sent, ordinal)
}

fn delivered_kind(update: &ViewUpdate) -> Option<DeliveryKind> {
    match update {
        ViewUpdate::Preview { .. } => Some(DeliveryKind::Preview),
        ViewUpdate::Message { .. } => Some(DeliveryKind::Message),
        ViewUpdate::AttemptEnded { .. } => Some(DeliveryKind::AttemptEnded),
        ViewUpdate::State(_) | ViewUpdate::ToolActivity(_) | ViewUpdate::Reset => None,
    }
}

/// Content-free byte size of a delivered update. Only the rendered text length
/// is observed; no text leaves this function.
fn delivered_bytes(update: &ViewUpdate) -> u64 {
    match update {
        ViewUpdate::Preview { text, .. } => text.len() as u64,
        ViewUpdate::Message { message, .. } => message
            .blocks
            .iter()
            .map(|block| match &block.content {
                Some(crate::conversation::content_block::Content::Text(text)) => {
                    text.text.len() as u64
                }
                Some(crate::conversation::content_block::Content::Reasoning(reasoning)) => {
                    reasoning.text.len() as u64
                }
                _ => 0,
            })
            .sum(),
        ViewUpdate::AttemptEnded { .. }
        | ViewUpdate::State(_)
        | ViewUpdate::ToolActivity(_)
        | ViewUpdate::Reset => 0,
    }
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
    bench: Arc<BenchContext>,
    frame: String,
    events: &'a mpsc::Sender<SessionEvent>,
    key: &'a SessionKey,
    observation_id: &'a str,
    generation: u64,
    failed: bool,
}

impl EventSink<'_> {
    async fn publish(&mut self, payload: SessionEventPayload) -> bool {
        let sent = send_event(
            self.events,
            self.key,
            self.observation_id,
            self.generation,
            payload,
        )
        .await;
        self.failed |= !sent;
        sent
    }
}

fn paged_sink<'a>(
    core: &Core,
    events: &'a mpsc::Sender<SessionEvent>,
    key: &'a SessionKey,
    observation_id: &'a str,
    generation: u64,
) -> EventSink<'a> {
    EventSink {
        bench: core.diagnostics.bench().clone(),
        frame: String::new(),
        events,
        key,
        observation_id,
        generation,
        failed: false,
    }
}

impl UpdateSink for EventSink<'_> {
    fn emit<'a>(&'a mut self, update: &'a ViewUpdate) -> SinkFuture<'a> {
        Box::pin(async move {
            let (sent, _) = send_conversation(
                self.bench.as_ref(),
                self.events,
                self.key,
                self.observation_id,
                self.generation,
                update.clone(),
                &self.frame,
                None,
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
    paged: Option<&mut PagedHistory>,
    approvals: &mut ApprovalCache,
    events: &mpsc::Sender<SessionEvent>,
    cancellation: &Cancellation,
    questions: Option<&mpsc::UnboundedSender<QuestionSignal>>,
) -> CoreResult<End> {
    // Ordering: open and prepare the one authorized MQTT connection first, then
    // negotiate Noise on its query lane, then attach the observation's control
    // and preview receivers before WatchSession can produce events.
    let regional = core.cloud_transport().await?;
    let connection = core
        .host_connection_with(&key.host_id, regional.clone())
        .await?;
    // Register the observation's control and preview receivers before the
    // subscription becomes ready and before WatchSession can produce events.
    // A preview or control frame that arrives during attach is queued by this
    // subscription instead of being dropped by the shared connection.
    let (subscription, detach) = connection.subscribe();
    core.wait_host_transport(&key.host_id, &connection, cancellation)
        .await?;
    let subscribed_ns = core.diagnostics.bench().now_ns();
    let connection_generation = connection.generation();
    let channel = core.ensure_channel(&key.host_id, fresh).await?;
    let channel_ready_ns = core.diagnostics.bench().now_ns();
    let interrupted = subscription.interrupted.clone();
    let overflow = subscription.overflow.clone();
    let live_overflow = subscription.live_overflow.clone();
    let error = subscription.error.clone();
    let queued = subscription.queued.clone();
    let live_queued = subscription.live_queued.clone();
    let depth = subscription.depth.clone();
    let live_depth = subscription.live_depth.clone();
    let control_rx = subscription.control;
    let live_rx = subscription.live;
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
    let watch_ready_ns = core.diagnostics.bench().now_ns();
    let connection_id = format!("{}:{connection_generation}", key.host_id);
    emit_join_at(
        core,
        &ready.watch_id,
        &connection_id,
        connection_generation,
        JoinMilestone::Subscribed,
        subscribed_ns,
    );
    emit_join_at(
        core,
        &ready.watch_id,
        &connection_id,
        connection_generation,
        JoinMilestone::ChannelReady,
        channel_ready_ns,
    );
    emit_join_at(
        core,
        &ready.watch_id,
        &connection_id,
        connection_generation,
        JoinMilestone::WatchReady,
        watch_ready_ns,
    );
    let epoch = initial
        .as_ref()
        .map(|state| state.live_epoch.clone())
        .unwrap_or_default();
    let state_revision = initial.as_ref().map_or(0, |state| state.revision);
    let projection = initial
        .as_ref()
        .map(|state| state.projection_id.clone())
        .unwrap_or_default();
    let mut paged_state = None;
    if interests.conversation {
        let initial = initial.as_ref().expect("conversation baseline");
        if projection.is_empty() || projection.len() > 128 {
            return Err(CoreError::Security(
                "missing watch content projection".into(),
            ));
        }
        conversation.reset(&epoch);
        let bench = core.diagnostics.bench();
        let attach_frame = bench.token(TokenDomain::Frame, &format!("attach:{observation_id}"));
        let mut sink = EventSink {
            bench: bench.clone(),
            frame: attach_frame,
            events,
            key,
            observation_id,
            generation,
            failed: false,
        };
        if paged.is_none() {
            // Anything already in the full local view predates the CLI watch.
            for message_id in core.sync_service()?.history_message_ids(key)? {
                conversation.mark_delivered_id(&message_id);
            }
            if !sink.emit(&ViewUpdate::Reset).await {
                pump.finish().await;
                return Ok(End::Done);
            }
            core.sync_conversation(key, conversation, false, false, &projection, &mut sink)
                .await?;
            emit_join(
                core,
                &ready.watch_id,
                &connection_id,
                connection_generation,
                JoinMilestone::CaughtUp,
            );
        }
        let _ = core
            .runtime
            .set_state(&key.host_id, ConnectionState::Connected);
        // Approvals are content and never ride the baseline; they are read
        // through their own bounded query. A failed read must not erase the
        // pending approvals a later event or lease tick will restore, so the
        // baseline is emitted only with what Core actually read.
        if paged.is_none() {
            approvals.items = fetch_approvals(core, key).await.unwrap_or_default();
        } else {
            approvals.stale = true;
        }
        let mut baseline: crate::conversation::SessionState = initial.clone().into();
        baseline.approvals = approvals.items.clone();
        if paged.is_some() {
            paged_state = Some(baseline.clone());
        }
        if sink.failed || !sink.emit(&ViewUpdate::State(baseline)).await {
            pump.finish().await;
            return Ok(End::Done);
        }
    }
    let sync = if interests.conversation && paged.is_none() {
        Some(core.sync_service()?)
    } else {
        None
    };
    let watch_id_for_close = ready.watch_id.clone();
    let result = conversation_loop(
        core,
        &connection,
        connection_generation,
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
        initial.map(|state| state.history_head).unwrap_or_default(),
        conversation,
        paged,
        paged_state,
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
        depth,
        live_depth,
        sync,
    )
    .await;
    emit_join(
        core,
        &watch_id_for_close,
        &connection_id,
        connection_generation,
        JoinMilestone::Closed,
    );
    result
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
    connection: &Arc<crate::connection::HostConnection>,
    connection_generation: u64,
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
    initial_head: String,
    conversation: &mut Conversation,
    mut paged: Option<&mut PagedHistory>,
    mut paged_state: Option<crate::conversation::SessionState>,
    approvals: &mut ApprovalCache,
    mut control_rx: mpsc::Receiver<QueuedEvent>,
    mut live_rx: mpsc::Receiver<QueuedEvent>,
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
    depth: Arc<AtomicUsize>,
    live_depth: Arc<AtomicUsize>,
    sync: Option<crate::sync::SyncService>,
) -> CoreResult<End> {
    let mut connection_state = connection.subscribe_state();
    let byte_budget = connection.control_queue_bytes();
    let mut history_read: Option<HistoryReadFuture> = paged
        .as_deref_mut()
        .map(|paged| paged.prepare(core, &projection, &initial_head));
    let mut baseline_approvals = Box::pin(fetch_approvals(core, key));
    let mut live_open = true;
    let mut state_revision = initial_revision;
    let mut first_update = true;
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
            // The connection marks interruptions before publishing its
            // state. Wake an idle observation without waiting for its lease.
            _ = connection_state.changed() => {}
            result = async { history_read.as_mut().expect("pending history read").await }, if history_read.is_some() => {
                history_read = None;
                let paged = paged.as_deref_mut().expect("paged history mode");
                let buffered = paged.finish_read();
                let mut sink = paged_sink(core, events, key, observation_id, generation);
                match result {
                    Ok(history) => {
                        paged.install(core, conversation, history, &mut sink).await?;
                        for batch in buffered {
                            if sink.failed {
                                break;
                            }
                            if !paged.receive(core, conversation, batch, byte_budget, &mut sink).await? {
                                pump.finish().await;
                                return Ok(End::Reconnect);
                            }
                        }
                    }
                    Err(error) => paged.unavailable(error.code(), &mut sink).await,
                }
                if sink.failed {
                    pump.finish().await;
                    return Ok(End::Done);
                }
            }
            fresh = &mut baseline_approvals, if paged_state.is_some() => {
                let mut baseline = paged_state.take().expect("pending approvals baseline");
                // An authoritative state event may have already superseded
                // this bounded initial read while it was in flight.
                if let Some(fresh) = fresh.filter(|_| state_revision == initial_revision) {
                        approvals.items = fresh;
                        approvals.stale = false;
                        baseline.approvals = approvals.items.clone();
                        let mut sink = paged_sink(core, events, key, observation_id, generation);
                        if !sink.emit(&ViewUpdate::State(baseline)).await {
                            pump.finish().await;
                            return Ok(End::Done);
                        }
                }
            }
            received = control_rx.recv() => {
                let Some(queued_event) = received else {
                    let failure = error.lock().expect("watch error mutex").take();
                    if let Some(failure) = failure {
                        return Err(failure);
                    }
                    pump.finish().await;
                    return Ok(End::Reconnect);
                };
                let class = queued_event.class;
                let envelope = queued_event.envelope;
                queued.fetch_sub(envelope.encoded_len(), Ordering::SeqCst);
                depth.fetch_sub(1, Ordering::SeqCst);
                let (frame, wait_ns) = frame_fact(core, &envelope, queued_event.enqueued_ns);
                let handling = handle_envelope(
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
                    approvals,
                    events,
                    questions,
                    sync.as_ref(),
                    paged.as_deref_mut(),
                    byte_budget,
                    &envelope,
                    &frame,
                    wait_ns,
                )
                .await?;
                if handling.delivered && first_update {
                    first_update = false;
                    emit_join(
                        core,
                        &watch_id,
                        &format!("{}:{connection_generation}", key.host_id),
                        connection_generation,
                        JoinMilestone::FirstUpdate,
                    );
                }
                match handling.flow {
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
                let Some(queued_event) = received else {
                    // The live sender ended with the same MQTT task that owns
                    // control; control completion determines the loop's end.
                    live_open = false;
                    continue;
                };
                let class = queued_event.class;
                let envelope = queued_event.envelope;
                live_queued.fetch_sub(envelope.encoded_len(), Ordering::SeqCst);
                live_depth.fetch_sub(1, Ordering::SeqCst);
                let (frame, wait_ns) = frame_fact(core, &envelope, queued_event.enqueued_ns);
                let handling = handle_envelope(
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
                    approvals,
                    events,
                    questions,
                    sync.as_ref(),
                    paged.as_deref_mut(),
                    byte_budget,
                    &envelope,
                    &frame,
                    wait_ns,
                )
                .await?;
                if handling.delivered && first_update {
                    first_update = false;
                    emit_join(
                        core,
                        &watch_id,
                        &format!("{}:{connection_generation}", key.host_id),
                        connection_generation,
                        JoinMilestone::FirstUpdate,
                    );
                }
                match handling.flow {
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
                // A replacement broker session cannot carry this observation:
                // its generation, subscriptions and stream are gone.
                if connection.generation() != connection_generation {
                    pump.finish().await;
                    return Ok(End::Reconnect);
                }
                // The check is read-only. Ownership comes from the authenticated
                // connection instance, and a revoked query or terminal
                // connection state ends the observation; there is no HTTP
                // authority read and no renewal effect.
                let failure = error.lock().expect("watch error mutex").take();
                if let Some(failure) = failure {
                    return Err(failure);
                }
                let renewal = check_versions(core, key, &watch_id).await;
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
                    if paged.is_none() && !send_event(
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
                if let Some(paged) = paged.as_deref_mut().filter(|_| history_read.is_none()) {
                    history_read = Some(paged.prepare(core, &projection, &lease.history_head));
                }
                if let Some(sync) = sync.as_ref() {
                    // A moved history head means committed content was missed.
                    // Repair it through the paged history read, which keeps
                    // every response bounded.
                    if sync.cursor(key)?.as_deref() != Some(lease.history_head.as_str()) {
                        let lease_frame = core.diagnostics.bench().token(
                            TokenDomain::Frame,
                            &format!("lease:{observation_id}"),
                        );
                        let mut sink = EventSink {
                            bench: core.diagnostics.bench().clone(),
                            frame: lease_frame,
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

/// Outcome of one read-only version check.
enum LeaseRenewal {
    /// The authoritative markers for this observation.
    Extended(whalerein_proto::VersionCheck),
    /// The Host no longer owns this observation; re-attach for a baseline.
    Reattach,
    /// Deterministic failure: the caller must see it, never a reconnect.
    Terminal(CoreError),
    /// Transient failure: keep the stream and retry on the next tick.
    Retry,
}

/// Read the authoritative DSH version markers for one active observation.
///
/// The check is read-only: it never renews ownership, extends a lease or
/// creates a durable request. An authoritative absence or denial ends the
/// observer; a transient failure leaves the last known data stale until the
/// next tick.
async fn check_versions(core: &Core, key: &SessionKey, watch_id: &str) -> LeaseRenewal {
    let response = core
        .query(
            &key.host_id,
            whalerein_proto::RequestPayload {
                payload: Some(request_payload::Payload::CheckVersions(CheckVersions {
                    session_id: key.session_id.clone(),
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
                // cannot succeed and would mask the real cause.
                "payload_too_large" | "invalid_argument" => LeaseRenewal::Terminal(core_error),
                "unsupported_capability" | "protocol_version_incompatible" => {
                    LeaseRenewal::Terminal(core_error)
                }
                _ => LeaseRenewal::Retry,
            };
        }
    };
    let Some(response_payload::Payload::VersionChecks(checks)) = response.payload else {
        return LeaseRenewal::Terminal(CoreError::Security(
            "unexpected version check response".into(),
        ));
    };
    match checks
        .checks
        .into_iter()
        .find(|check| check.watch_id == watch_id)
    {
        Some(check) => LeaseRenewal::Extended(check),
        // An absent check means the observation is no longer authoritative.
        None => LeaseRenewal::Terminal(CoreError::AuthorityClosed("session_deleted".into())),
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
struct Handling {
    flow: Flow,
    disposition: DequeueDisposition,
    reason: Option<&'static str>,
    /// A conversation update actually reached the observation stream.
    delivered: bool,
}

fn emit_receipt(
    core: &Core,
    frame: &str,
    bytes: u64,
    decode: crate::observation::bench::Decode,
    scope: Option<String>,
    range: Option<&FrameRange>,
    reason: Option<&'static str>,
) {
    emit_receipt_fact(
        core.diagnostics.bench(),
        frame,
        bytes,
        decode,
        scope,
        range,
        reason,
    );
}

/// Emit one receipt fact against an explicit process outlet. Split from
/// [`emit_receipt`] so a test can observe the boundary without a live `Core`.
fn emit_receipt_fact(
    bench: &BenchContext,
    frame: &str,
    bytes: u64,
    decode: crate::observation::bench::Decode,
    scope: Option<String>,
    range: Option<&FrameRange>,
    reason: Option<&'static str>,
) {
    if !bench.enabled() {
        return;
    }
    bench.emit(BenchEvent::CoreReceipt {
        frame: frame.to_owned(),
        kind: crate::observation::bench::ReceiptKind::Event,
        bytes,
        decode,
        scope,
        stream: range.map(|range| range.stream.clone()),
        batch: range.map(|range| range.batch.clone()),
        block: range.and_then(|range| range.block.clone()),
        seq_start: range.map(|range| range.seq_start),
        seq_end: range.map(|range| range.seq_end),
        reason: reason.map(str::to_owned),
    });
}

fn emit_dequeue(
    core: &Core,
    frame: &str,
    wait_ns: u64,
    disposition: DequeueDisposition,
    reason: Option<&'static str>,
) {
    let bench = core.diagnostics.bench();
    if !bench.enabled() {
        return;
    }
    bench.emit(BenchEvent::CoreDequeue {
        frame: frame.to_owned(),
        wait_ns,
        disposition,
        reason: reason.map(str::to_owned),
        // The queue entry is a sealed envelope; dequeue stays frame-joined and
        // never guesses the increment a decoded frame may later name.
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
}

fn frame_fact(core: &Core, envelope: &EventEnvelope, enqueued_ns: u64) -> (String, u64) {
    let bench = core.diagnostics.bench();
    if !bench.enabled() {
        return (String::new(), 0);
    }
    let frame = bench.token(
        TokenDomain::Frame,
        &crate::observation::bench::event_frame_identity(envelope),
    );
    let wait_ns = if enqueued_ns == 0 {
        // The enqueue boundary was not observed; never invent residence time.
        0
    } else {
        bench.now_ns().saturating_sub(enqueued_ns)
    };
    (frame, wait_ns)
}

fn emit_join_at(
    core: &Core,
    watch_id: &str,
    connection_id: &str,
    generation: u64,
    milestone: JoinMilestone,
    ts: u64,
) {
    let bench = core.diagnostics.bench();
    if !bench.enabled() {
        return;
    }
    bench.emit_at(
        BenchEvent::CoreJoin {
            watch: bench.token(TokenDomain::Watch, watch_id),
            connection: bench.token(TokenDomain::Connection, connection_id),
            generation,
            milestone,
        },
        ts,
    );
}

fn emit_join(
    core: &Core,
    watch_id: &str,
    connection_id: &str,
    generation: u64,
    milestone: JoinMilestone,
) {
    let ts = core.diagnostics.bench().now_ns();
    emit_join_at(core, watch_id, connection_id, generation, milestone, ts);
}

fn scope_token(core: &Core, update: &whalerein_proto::SessionUpdate) -> Option<String> {
    update.session.as_ref().map(|session| {
        core.diagnostics.bench().token(
            TokenDomain::Session,
            &format!("{}:{}", session.host_id, session.session_id),
        )
    })
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
    paged: Option<&mut PagedHistory>,
    byte_budget: usize,
    envelope: &EventEnvelope,
    frame: &str,
    wait_ns: u64,
) -> CoreResult<Handling> {
    let bytes = envelope.encoded_len() as u64;
    // The control and live topics are shared by this Device's connections.
    // Only the observation that owns the Noise channel can open the event.
    if envelope.device_id == channel.device_id()
        && envelope.host_id == channel.host_id()
        && !envelope.channel_id.is_empty()
        && envelope.channel_id != channel.channel_id()
    {
        emit_receipt(
            core,
            frame,
            bytes,
            crate::observation::bench::Decode::Rejected,
            None,
            None,
            Some("channel_replaced"),
        );
        emit_dequeue(
            core,
            frame,
            wait_ns,
            DequeueDisposition::Stale,
            Some("channel_replaced"),
        );
        return Ok(Handling {
            flow: Flow::Continue,
            disposition: DequeueDisposition::Stale,
            reason: Some("channel_replaced"),
            delivered: false,
        });
    }
    // Resolve and verify a referenced control event before opening it.
    let envelope = match core.resolve_event_envelope(envelope).await {
        Ok(envelope) => envelope,
        Err(error) => {
            emit_receipt(
                core,
                frame,
                bytes,
                crate::observation::bench::Decode::Rejected,
                None,
                None,
                Some(error.code()),
            );
            emit_dequeue(
                core,
                frame,
                wait_ns,
                DequeueDisposition::Rejected,
                Some(error.code()),
            );
            return Err(error);
        }
    };
    let update = match crate::sync::open_event(channel, &envelope) {
        Ok(crate::sync::OpenedEvent::Update(update)) => {
            let range = FrameRange::from_update(core.diagnostics.bench().as_ref(), &update);
            emit_receipt(
                core,
                frame,
                bytes,
                crate::observation::bench::Decode::Ok,
                scope_token(core, &update),
                range.as_ref(),
                None,
            );
            update
        }
        Ok(crate::sync::OpenedEvent::Closed(closed)) => {
            emit_receipt(
                core,
                frame,
                bytes,
                crate::observation::bench::Decode::Ok,
                None,
                None,
                None,
            );
            // A terminal close is control intent; it is never a live preview.
            if class != DeliveryClass::Control {
                emit_dequeue(
                    core,
                    frame,
                    wait_ns,
                    DequeueDisposition::Rejected,
                    Some("live_close"),
                );
                return Err(CoreError::Security(
                    "control payload received on the live topic".into(),
                ));
            }
            if closed.watch_id == watch_id && closed.session.as_ref() == Some(wire_key) {
                // A Host-authored terminal close is authoritative, like an
                // Access authority reason: never reconnect.
                emit_dequeue(
                    core,
                    frame,
                    wait_ns,
                    DequeueDisposition::Rejected,
                    Some("watch_closed"),
                );
                return Err(CoreError::AuthorityClosed(closed.reason));
            }
            emit_dequeue(
                core,
                frame,
                wait_ns,
                DequeueDisposition::Stale,
                Some("other_watch"),
            );
            return Ok(Handling {
                flow: Flow::Continue,
                disposition: DequeueDisposition::Stale,
                reason: Some("other_watch"),
                delivered: false,
            });
        }
        Err(error) => {
            emit_receipt(
                core,
                frame,
                bytes,
                crate::observation::bench::Decode::Rejected,
                None,
                None,
                Some(error.code()),
            );
            emit_dequeue(
                core,
                frame,
                wait_ns,
                DequeueDisposition::Rejected,
                Some(error.code()),
            );
            return Err(error);
        }
    };
    if let Err(error) = enforce_delivery_class(class, &update) {
        emit_dequeue(
            core,
            frame,
            wait_ns,
            DequeueDisposition::Rejected,
            Some(error.code()),
        );
        return Err(error);
    }
    if update.session.as_ref() != Some(wire_key) || update.watch_id != watch_id {
        emit_dequeue(
            core,
            frame,
            wait_ns,
            DequeueDisposition::Stale,
            Some("scope_mismatch"),
        );
        return Ok(Handling {
            flow: Flow::Continue,
            disposition: DequeueDisposition::Stale,
            reason: Some("scope_mismatch"),
            delivered: false,
        });
    }
    let Some(payload) = update.payload else {
        let error = CoreError::Security("empty typed update".into());
        emit_dequeue(
            core,
            frame,
            wait_ns,
            DequeueDisposition::Rejected,
            Some(error.code()),
        );
        return Err(error);
    };
    let handling = match handle_update(
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
        paged,
        byte_budget,
        payload,
        frame,
    )
    .await
    {
        Ok(handling) => handling,
        Err(error) => {
            emit_dequeue(
                core,
                frame,
                wait_ns,
                DequeueDisposition::Rejected,
                Some(error.code()),
            );
            return Err(error);
        }
    };
    emit_dequeue(core, frame, wait_ns, handling.disposition, handling.reason);
    Ok(handling)
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
    paged: Option<&mut PagedHistory>,
    byte_budget: usize,
    payload: session_update::Payload,
    frame: &str,
) -> CoreResult<Handling> {
    let bench = core.diagnostics.bench();
    match payload {
        session_update::Payload::Questions(descriptor) => {
            if interests.questions {
                signal_descriptor(questions, descriptor);
            }
            Ok(Handling {
                flow: Flow::Continue,
                disposition: DequeueDisposition::Applied,
                reason: None,
                delivered: false,
            })
        }
        session_update::Payload::Live(frame_payload) => {
            if !interests.conversation {
                return Ok(Handling {
                    flow: Flow::Continue,
                    disposition: DequeueDisposition::Applied,
                    reason: None,
                    delivered: false,
                });
            }
            let range = FrameRange::from_frame(bench.as_ref(), &frame_payload);
            let (view, live) = conversation.live_decided(&frame_payload)?;
            let disposition = match live {
                LiveDisposition::Applied => DequeueDisposition::Applied,
                LiveDisposition::Duplicate => DequeueDisposition::Duplicate,
                LiveDisposition::Stale => DequeueDisposition::Stale,
            };
            let Some(view) = view else {
                return Ok(Handling {
                    flow: Flow::Continue,
                    disposition,
                    reason: None,
                    delivered: false,
                });
            };
            let (sent, _) = send_conversation(
                bench.as_ref(),
                events,
                key,
                observation_id,
                generation,
                view,
                frame,
                range.as_ref(),
            )
            .await;
            Ok(Handling {
                flow: if sent { Flow::Continue } else { Flow::Done },
                disposition,
                reason: None,
                delivered: sent,
            })
        }
        session_update::Payload::History(batch) => {
            if !interests.conversation {
                return Ok(Handling {
                    flow: Flow::Continue,
                    disposition: DequeueDisposition::Applied,
                    reason: None,
                    delivered: false,
                });
            }
            if let Some(paged) = paged {
                if batch.projection_id != *projection {
                    return Ok(Handling {
                        flow: Flow::Reconnect,
                        disposition: DequeueDisposition::Stale,
                        reason: Some("projection_changed"),
                        delivered: false,
                    });
                }
                let mut sink = paged_sink(core, events, key, observation_id, generation);
                let contiguous = paged
                    .receive(core, conversation, batch, byte_budget, &mut sink)
                    .await?;
                return Ok(if sink.failed {
                    Handling {
                        flow: Flow::Done,
                        disposition: DequeueDisposition::Rejected,
                        reason: None,
                        delivered: false,
                    }
                } else if contiguous {
                    Handling {
                        flow: Flow::Continue,
                        disposition: DequeueDisposition::Applied,
                        reason: None,
                        delivered: true,
                    }
                } else {
                    Handling {
                        flow: Flow::Reconnect,
                        disposition: DequeueDisposition::Applied,
                        reason: None,
                        delivered: false,
                    }
                });
            }
            let Some(sync) = sync else {
                return Err(CoreError::Internal(
                    "conversation sync is unavailable".into(),
                ));
            };
            if batch.projection_id.is_empty() || batch.projection_id != *projection {
                conversation.reset_projection("");
                *projection = batch.projection_id.clone();
                return Ok(Handling {
                    flow: Flow::Reconnect,
                    disposition: DequeueDisposition::Stale,
                    reason: Some("projection_changed"),
                    delivered: false,
                });
            }
            let outcome = sync.apply_batch(key, &batch)?;
            let mut delivered = false;
            match outcome {
                BatchOutcome::Applied => {
                    for change in &batch.changes {
                        let Some(view) = conversation.committed_once(change)? else {
                            continue;
                        };
                        let (sent, _) = send_conversation(
                            bench.as_ref(),
                            events,
                            key,
                            observation_id,
                            generation,
                            view,
                            frame,
                            None,
                        )
                        .await;
                        if !sent {
                            return Ok(Handling {
                                flow: Flow::Done,
                                disposition: DequeueDisposition::Applied,
                                reason: None,
                                delivered: true,
                            });
                        }
                        delivered = true;
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
                                return Ok(Handling {
                                    flow: Flow::Done,
                                    disposition: DequeueDisposition::Applied,
                                    reason: None,
                                    delivered: true,
                                });
                            }
                        }
                    }
                    Ok(Handling {
                        flow: Flow::Continue,
                        disposition: DequeueDisposition::Applied,
                        reason: None,
                        delivered,
                    })
                }
                BatchOutcome::Duplicate => Ok(Handling {
                    flow: Flow::Continue,
                    disposition: DequeueDisposition::Duplicate,
                    reason: None,
                    delivered: false,
                }),
                BatchOutcome::Gap => {
                    let mut sink = EventSink {
                        bench: bench.clone(),
                        frame: frame.to_owned(),
                        events,
                        key,
                        observation_id,
                        generation,
                        failed: false,
                    };
                    core.sync_conversation(key, conversation, true, false, projection, &mut sink)
                        .await?;
                    if sink.failed {
                        return Ok(Handling {
                            flow: Flow::Done,
                            disposition: DequeueDisposition::WaitingGap,
                            reason: Some("history_gap"),
                            delivered: false,
                        });
                    }
                    Ok(Handling {
                        flow: Flow::Continue,
                        disposition: DequeueDisposition::WaitingGap,
                        reason: Some("history_gap"),
                        delivered: false,
                    })
                }
            }
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
                return Ok(Handling {
                    flow: Flow::Continue,
                    disposition: DequeueDisposition::Applied,
                    reason: None,
                    delivered: false,
                });
            }
            if state.projection_id.is_empty() || state.projection_id != *projection {
                if paged.is_none() {
                    conversation.reset_projection("");
                }
                *projection = state.projection_id.clone();
                return Ok(Handling {
                    flow: Flow::Reconnect,
                    disposition: DequeueDisposition::Stale,
                    reason: Some("projection_changed"),
                    delivered: false,
                });
            }
            if state.live_epoch != epoch {
                return Ok(Handling {
                    flow: Flow::Reconnect,
                    disposition: DequeueDisposition::Stale,
                    reason: Some("epoch_changed"),
                    delivered: false,
                });
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
                    return Ok(Handling {
                        flow: Flow::Done,
                        disposition: DequeueDisposition::Applied,
                        reason: None,
                        delivered: false,
                    });
                }
                return Ok(Handling {
                    flow: Flow::Continue,
                    disposition: DequeueDisposition::Applied,
                    reason: None,
                    delivered: false,
                });
            }
            Ok(Handling {
                flow: Flow::Continue,
                disposition: DequeueDisposition::Duplicate,
                reason: None,
                delivered: false,
            })
        }
        session_update::Payload::Reset(_) => {
            if !interests.conversation {
                return Ok(Handling {
                    flow: Flow::Continue,
                    disposition: DequeueDisposition::Applied,
                    reason: None,
                    delivered: false,
                });
            }
            if let Some(paged) = paged {
                paged.invalidate();
            } else {
                conversation.reset("");
                core.sync_service()?.discard_stage(key)?;
            }
            Ok(Handling {
                flow: Flow::Reconnect,
                disposition: DequeueDisposition::Applied,
                reason: None,
                delivered: false,
            })
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
