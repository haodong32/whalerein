//! The `MobileCore` facade exported to Swift.
//!
//! All Swift-facing methods are synchronous. Async Core work runs on a
//! dedicated multi-thread Tokio runtime owned by this object; request/response
//! calls block the calling thread with a bounded timeout. Background pairing
//! and watching publish ordered events into a bounded queue consumed by
//! `pollEvents`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::task::AbortHandle;
use whalerein_core::api::{
    Cancellation, ClientConfig, ClientService, Core, Environment, HistoryContinuity,
    ObservationInterests, PairingDecision, PairingPrompt, QuestionAnswer, SessionEventPayload,
    SessionKey,
};
use whalerein_core::attachments::{DownloadProgress, DownloadRequest};
use whalerein_core::history::{HistoryContinuation, HistoryPageRequest};
use whalerein_core::CoreError;

use crate::diagnostics::{CoreDiagnosticSink, DiagnosticContext, DiagnosticDispatcher};
use crate::error::{from_core_code, MobileError, CODE_CANCELLED};
use crate::queue::{
    action_is_advertised, PendingQueueItemView, PendingQueueView, QueueAction, QueueMutationView,
};
use crate::secret::{MemorySecretStore, SecretStore, SecretStoreAdapter};
use crate::types::{
    bounded_question_view, convert_update, message_view, operation_status_name,
    question_batch_view, question_guard, question_page_view, questions_view, ApprovalView,
    AttachmentDownloadRequestView, AttachmentDownloadView, AttachmentProgressView,
    AttachmentTransferView, CloudAccountSessionView, CloudAccountStatusView, CloudAccountView,
    CloudAuthAttemptView, CloudConnectResultView, CloudDirectoryHostView, CommandView,
    ConversationEvent, ConversationEventEnvelope, DeliveryChoice, DiagnosticsReportView,
    HistoryContinuityView, HistoryPageView, HostConnectionView, HostSummary, MessageReferenceView,
    MessageView, PairingPromptView, QuestionAnswerView, QuestionBatchView,
    QuestionCancelReceiptView, QuestionPageView, RetainedOperationView, SendOperationView,
    SessionCapabilitiesView, SessionKeyView, SessionSummaryView, WatchEventScope,
    WatchHistoryBaseline,
};
use crate::wait::{WaitAttempts, WaitResultView, MAX_WAIT_TIMEOUT_MS};

/// Request/response budget for listing and local operations.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Request/response budget for operations Core may wait on until its TTL.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound for one follow-up/content-read budget supplied by the caller.
const MAX_FOLLOW_TIMEOUT_MS: u64 = 55_000;
/// How long `pairingStart` waits for Core to produce a prompt.
const PAIRING_PROMPT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a pending pairing prompt waits for a decision before rejecting.
const PAIRING_DECISION_TIMEOUT: Duration = Duration::from_secs(120);
/// Upper bound for one `pollEvents` call; callers poll repeatedly.
const MAX_POLL_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_QUEUE_CAPACITY: usize = 512;
const MAX_ID_BYTES: usize = 512;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_INVITATION_BYTES: usize = 16 * 1024;
/// Bound on an app-supplied platform trust bundle. A real platform store is
/// far smaller; this only rejects absurd input before parsing.
const MAX_CA_PEM_BYTES: usize = 4 * 1024 * 1024;

/// Reject an unusable platform trust bundle before it reaches Core. A real
/// platform store is far smaller than the bound; the check only stops absurd or
/// empty input, while Core still validates the anchors when it opens a
/// transport.
fn platform_trust_pem(pem: Vec<u8>) -> Result<Vec<u8>, MobileError> {
    if pem.is_empty() || pem.len() > MAX_CA_PEM_BYTES {
        return Err(MobileError::invalid_input());
    }
    Ok(pem)
}
const MAX_FINGERPRINT_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const MAX_APP_VERSION_BYTES: usize = 256;
const MAX_CALLBACK_BYTES: usize = 8192;
/// Upper bound for one caller-supplied attachment result wait.
const MAX_ATTACHMENT_WAIT_MS: u64 = 30_000;
/// Upper bound for concurrent app-started attachment transfers.
const MAX_ATTACHMENT_TRANSFERS: usize = 32;

/// Build-time Core package version, recorded by `build.rs`.
pub(crate) const CORE_VERSION: &str = env!("WHALERIN_CORE_VERSION");

#[derive(uniffi::Enum)]
pub enum MobileClientService {
    WhaleReinCloud,
    Standalone,
}

#[derive(uniffi::Enum)]
pub enum MobileCloudEnvironment {
    Production,
    Staging,
    Local,
}

#[uniffi::export]
pub fn write_client_config(
    config_path: String,
    service: MobileClientService,
    data_dir: String,
    cloud_environment: Option<MobileCloudEnvironment>,
    standalone_ca_certificate: Option<String>,
) -> Result<(), MobileError> {
    if config_path.is_empty()
        || config_path.len() > MAX_PATH_BYTES
        || data_dir.is_empty()
        || data_dir.len() > MAX_PATH_BYTES
    {
        return Err(MobileError::invalid_input());
    }
    let service = match service {
        MobileClientService::WhaleReinCloud => ClientService::WhaleReinCloud,
        MobileClientService::Standalone => ClientService::Standalone,
    };
    let mut config = ClientConfig::new(service, data_dir);
    config.cloud_environment = cloud_environment.map(|environment| match environment {
        MobileCloudEnvironment::Production => Environment::Production,
        MobileCloudEnvironment::Staging => Environment::Staging,
        MobileCloudEnvironment::Local => Environment::Local,
    });
    config.standalone_ca_certificate = standalone_ca_certificate.map(Into::into);
    Core::write_config(Path::new(&config_path), &config).map_err(MobileError::from)
}

/// Unified Core version plus git provenance for this binding build.
#[uniffi::export]
pub fn core_revision() -> String {
    env!("WHALERIN_MOBILE_REVISION").to_owned()
}

/// Allocate a non-secret send operation identity before submission.
#[uniffi::export]
pub fn new_operation_id() -> String {
    Core::new_command_id()
}

/// The pinned Android App Link callback for one official Cloud environment.
#[uniffi::export]
pub fn android_redirect_uri(environment: String) -> String {
    whalerein_core::api::android_redirect_uri(&environment).to_owned()
}

impl WatchEventScope {
    fn pending(generation: u64, host_id: String, session_id: String) -> Self {
        Self {
            generation,
            host_id,
            session_id,
            observation_id: None,
        }
    }

    fn with_observation(&self, observation_id: String) -> Self {
        Self {
            observation_id: Some(observation_id),
            ..self.clone()
        }
    }

    fn same_watch(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.host_id == other.host_id
            && self.session_id == other.session_id
    }
}

impl ConversationEventEnvelope {
    fn unscoped(event: ConversationEvent) -> Self {
        Self { scope: None, event }
    }

    fn scoped(scope: WatchEventScope, event: ConversationEvent) -> Self {
        Self {
            scope: Some(scope),
            event,
        }
    }
}

/// Bounded, drop-oldest event queue with an explicit loss counter.
///
/// A burst larger than the capacity keeps the newest events and reports how
/// many older ones were discarded on the next poll. Memory never grows past
/// `EVENT_QUEUE_CAPACITY`.
///
/// Watch-scoped entries are only accepted for the active watch. Replacing the
/// watch removes every entry from the previous one, so a consumer that pauses
/// across `startWatch` can never observe the old session's events as the new
/// session's events. Pairing and command events stay queued.
struct EventQueue {
    shared: Mutex<EventQueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct EventQueueState {
    events: std::collections::VecDeque<ConversationEventEnvelope>,
    dropped: u64,
    watch_dropped: Option<(WatchEventScope, u64)>,
    active_watch: Option<WatchEventScope>,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            shared: Mutex::new(EventQueueState::default()),
            ready: Condvar::new(),
        }
    }

    /// Queue an event that does not belong to a watch. These survive watch
    /// replacement because they describe independent activity (pairing, a
    /// command receipt, a bounded-queue loss notice).
    fn push(&self, event: ConversationEvent) {
        let mut state = self.lock();
        Self::enqueue(&mut state, ConversationEventEnvelope::unscoped(event));
        drop(state);
        self.ready.notify_one();
    }

    /// Start a new watch scope, discarding queued output from every replaced
    /// watch. Unscoped events remain in order.
    fn begin_watch(&self, scope: &WatchEventScope) {
        let mut state = self.lock();
        state.events.retain(|entry| entry.scope.is_none());
        state.watch_dropped = None;
        state.active_watch = Some(scope.clone());
    }

    /// Register the Core producer so later output and termination keep its identity.
    fn attach_watch(&self, scope: &WatchEventScope) -> bool {
        let mut state = self.lock();
        let Some(active) = state.active_watch.as_mut() else {
            return false;
        };
        if !active.same_watch(scope) || active.observation_id.is_some() {
            return false;
        }
        *active = scope.clone();
        true
    }

    /// Queue output produced by the active watch.
    ///
    /// A late callback from a replaced watch is dropped here even if it passed
    /// its generation check just before replacement.
    fn push_watch(&self, scope: &WatchEventScope, event: ConversationEvent) {
        let mut state = self.lock();
        if !state
            .active_watch
            .as_ref()
            .is_some_and(|active| active == scope)
        {
            return;
        }
        Self::enqueue(
            &mut state,
            ConversationEventEnvelope::scoped(scope.clone(), event),
        );
        drop(state);
        self.ready.notify_one();
    }

    /// End a watch and queue its terminal event.
    ///
    /// A stale terminal from a replaced watch cannot clear its successor.
    fn end_watch(&self, scope: &WatchEventScope) {
        let mut state = self.lock();
        let Some(stored) = state.active_watch.as_ref().filter(|active| {
            active.same_watch(scope)
                && (scope.observation_id.is_none() || active.observation_id == scope.observation_id)
        }) else {
            return;
        };
        let stored = stored.clone();
        state.active_watch = None;
        Self::enqueue(
            &mut state,
            ConversationEventEnvelope::scoped(stored, ConversationEvent::WatchEnded),
        );
        drop(state);
        self.ready.notify_one();
    }

    fn enqueue(state: &mut EventQueueState, entry: ConversationEventEnvelope) {
        if state.events.len() >= EVENT_QUEUE_CAPACITY {
            if let Some(scope) = state.events.pop_front().and_then(|entry| entry.scope) {
                let (_, count) = state.watch_dropped.get_or_insert((scope, 0));
                *count = count.saturating_add(1);
            } else {
                state.dropped = state.dropped.saturating_add(1);
            }
        }
        state.events.push_back(entry);
    }

    fn poll(&self, timeout: Duration) -> Vec<ConversationEventEnvelope> {
        self.poll_waiting(timeout, || {})
    }

    fn poll_waiting(
        &self,
        timeout: Duration,
        before_wait: impl FnOnce(),
    ) -> Vec<ConversationEventEnvelope> {
        let mut before_wait = Some(before_wait);
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if !state.events.is_empty() || state.dropped > 0 || state.watch_dropped.is_some() {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if let Some(before_wait) = before_wait.take() {
                before_wait();
            }
            let (next, _) = self
                .ready
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
        let mut events = Vec::with_capacity(state.events.len() + 2);
        if state.dropped > 0 {
            events.push(ConversationEventEnvelope::unscoped(
                ConversationEvent::DroppedEvents {
                    count: state.dropped,
                },
            ));
            state.dropped = 0;
        }
        if let Some((scope, count)) = state.watch_dropped.take() {
            events.push(ConversationEventEnvelope::scoped(
                scope,
                ConversationEvent::DroppedEvents { count },
            ));
        }
        events.extend(state.events.drain(..));
        events
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, EventQueueState> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One terminal transfer outcome. The shape keeps the stable error code and
/// message separate from the verified download so Swift never inspects Core
/// text to tell a cancellation from an integrity failure.
struct AttachmentTerminal {
    state: &'static str,
    download: Option<AttachmentDownloadView>,
    error: Option<MobileError>,
}

/// Shared state for one app-started verified attachment download.
struct AttachmentTransferState {
    id: String,
    progress: Mutex<AttachmentProgressView>,
    terminal: Mutex<Option<AttachmentTerminal>>,
    notify: Condvar,
    cancellation: Cancellation,
}

impl AttachmentTransferState {
    fn new(id: String) -> Self {
        Self {
            id,
            progress: Mutex::new(AttachmentProgressView {
                received: 0,
                total: 0,
            }),
            terminal: Mutex::new(None),
            notify: Condvar::new(),
            cancellation: Cancellation::new(),
        }
    }

    fn set_progress(&self, progress: DownloadProgress) {
        if let Ok(mut current) = self.progress.lock() {
            *current = progress.into();
        }
        self.notify.notify_all();
    }

    fn finish(&self, terminal: AttachmentTerminal) {
        if let Ok(mut current) = self.terminal.lock() {
            *current = Some(terminal);
        }
        self.notify.notify_all();
    }

    fn view(&self, terminal: Option<AttachmentTerminal>) -> AttachmentTransferView {
        let progress = self
            .progress
            .lock()
            .map(|value| *value)
            .unwrap_or(AttachmentProgressView {
                received: 0,
                total: 0,
            });
        match terminal {
            Some(terminal) => AttachmentTransferView {
                download_id: self.id.clone(),
                state: terminal.state.into(),
                progress,
                download: terminal.download,
                error_code: terminal.error.as_ref().map(|error| error.code().to_owned()),
                error_message: terminal
                    .error
                    .as_ref()
                    .map(|error| error.message().to_owned()),
            },
            None => AttachmentTransferView {
                download_id: self.id.clone(),
                state: "running".into(),
                progress,
                download: None,
                error_code: None,
                error_message: None,
            },
        }
    }
}

/// Bounded registry of app-started attachment transfers.
#[derive(Default)]
struct AttachmentTransfers {
    next: AtomicU64,
    transfers: Mutex<HashMap<String, Arc<AttachmentTransferState>>>,
}

impl AttachmentTransfers {
    fn insert(&self, state: Arc<AttachmentTransferState>) -> Result<(), MobileError> {
        let mut transfers = self
            .transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if transfers.len() >= MAX_ATTACHMENT_TRANSFERS {
            return Err(MobileError::conflict());
        }
        transfers.insert(state.id.clone(), state);
        Ok(())
    }

    fn get(&self, id: &str) -> Option<Arc<AttachmentTransferState>> {
        self.transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }

    fn remove(&self, id: &str) {
        self.transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    fn cancel_all(&self) {
        let transfers = self
            .transfers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for transfer in transfers.values() {
            transfer.cancellation.cancel();
        }
    }
}

/// Owns the multi-thread runtime on a dedicated thread.
///
/// FFI calls use `Handle::block_on`; the thread that built the runtime keeps it
/// alive and drops it after shutdown, so dropping `MobileCore` joins cleanly.
struct RuntimeHost {
    handle: Handle,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RuntimeHost {
    fn start() -> Result<Self, MobileError> {
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("whalerein-mobile-runtime".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = handle_tx.send(None);
                        return;
                    }
                };
                if handle_tx.send(Some(runtime.handle().clone())).is_err() {
                    return;
                }
                runtime.block_on(async move {
                    let _ = shutdown_rx.await;
                });
            })
            .map_err(|_| MobileError::internal())?;
        let Some(handle) = handle_rx.recv().ok().flatten() else {
            let _ = thread.join();
            return Err(MobileError::internal());
        };
        Ok(Self {
            handle,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }

    fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RuntimeHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A pending or completed pairing attempt.
#[derive(Default)]
struct PairingState {
    generation: u64,
    running: bool,
    decision: Option<Sender<PairingDecision>>,
    abort: Option<AbortHandle>,
}

/// The currently active watch.
#[derive(Default)]
struct WatchState {
    generation: u64,
    host_id: String,
    session_id: String,
    active: bool,
    watch: Option<AbortHandle>,
    /// Dropped with the watch task. Waiting for disconnect before starting the
    /// replacement prevents two Core observations from racing the profile
    /// lease and the one-observation constraint.
    watch_stopped: Option<std::sync::mpsc::Receiver<()>>,
}

/// Signals the end of one watch task when the task (or its future) is dropped.
struct WatchStopSignal(Option<Sender<()>>);

impl Drop for WatchStopSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn open_mobile_core(
    config_path: String,
    secret_store: Arc<dyn SecretStore>,
    app_version: String,
    diagnostics: Option<whalerein_core::api::DiagnosticContext>,
    dispatcher: Option<Arc<DiagnosticDispatcher>>,
    platform_roots: Option<Vec<u8>>,
) -> Result<Arc<MobileCore>, MobileError> {
    if config_path.is_empty() || config_path.len() > MAX_PATH_BYTES {
        return Err(MobileError::invalid_input());
    }
    if app_version.len() > MAX_APP_VERSION_BYTES {
        return Err(MobileError::invalid_input());
    }
    let runtime = RuntimeHost::start()?;
    let secrets = Arc::new(SecretStoreAdapter::new(secret_store));
    let path = Path::new(&config_path);
    let core = match diagnostics {
        Some(context) => {
            Core::open_with_diagnostics_and_platform_roots(path, secrets, context, platform_roots)
        }
        None => Core::open_with_platform_roots(path, secrets, platform_roots),
    }
    .map_err(MobileError::from)?;
    Ok(Arc::new(MobileCore {
        core,
        runtime,
        events: Arc::new(EventQueue::new()),
        app_version,
        pairing: Arc::new(Mutex::new(PairingState::default())),
        pairing_generation: Arc::new(AtomicU64::new(0)),
        watch: Arc::new(Mutex::new(WatchState::default())),
        watch_generation: Arc::new(AtomicU64::new(0)),
        wait_attempts: WaitAttempts::default(),
        attachment_transfers: Arc::new(AttachmentTransfers::default()),
        monitor: Arc::new(Mutex::new(None)),
        monitored: Arc::new(Mutex::new(Vec::new())),
        _diagnostics: dispatcher,
    }))
}

/// Swift-facing Core facade. See `README.md` for the exported surface.
#[derive(uniffi::Object)]
pub struct MobileCore {
    core: Core,
    runtime: RuntimeHost,
    events: Arc<EventQueue>,
    app_version: String,
    pairing: Arc<Mutex<PairingState>>,
    pairing_generation: Arc<AtomicU64>,
    watch: Arc<Mutex<WatchState>>,
    watch_generation: Arc<AtomicU64>,
    wait_attempts: WaitAttempts,
    attachment_transfers: Arc<AttachmentTransfers>,
    /// The app-level Host connection probe. Host-scoped, independent of any
    /// watch.
    monitor: Arc<Mutex<Option<AbortHandle>>>,
    monitored: Arc<Mutex<Vec<String>>>,
    /// Keeps the diagnostic dispatcher alive for the lifetime of the handle.
    _diagnostics: Option<Arc<DiagnosticDispatcher>>,
}

#[uniffi::export]
impl MobileCore {
    /// Create a Core instance backed by `secretStore`.
    ///
    /// `configPath` is selected by the app inside its persistent sandbox.
    #[uniffi::constructor]
    pub fn new(
        config_path: String,
        secret_store: Arc<dyn SecretStore>,
        app_version: String,
    ) -> Result<Arc<Self>, MobileError> {
        open_mobile_core(config_path, secret_store, app_version, None, None, None)
    }

    /// Create a Core instance that forwards Core's redacted diagnostics to
    /// `sink` from the moment Core opens. The receiver is invoked on the
    /// binding's bounded delivery thread, never on the caller's thread.
    #[uniffi::constructor]
    pub fn new_with_diagnostic_sink(
        config_path: String,
        secret_store: Arc<dyn SecretStore>,
        app_version: String,
        sink: Arc<dyn CoreDiagnosticSink>,
    ) -> Result<Arc<Self>, MobileError> {
        let context = DiagnosticContext::new(sink);
        Self::new_with_diagnostic_context(config_path, secret_store, app_version, context)
    }

    /// Open Core with a diagnostic context reserved before configuration is
    /// loaded, so startup failures stay observable and the supplied trace ID
    /// can be used for correlation.
    #[uniffi::constructor]
    pub fn new_with_diagnostic_context(
        config_path: String,
        secret_store: Arc<dyn SecretStore>,
        app_version: String,
        context: Arc<DiagnosticContext>,
    ) -> Result<Arc<Self>, MobileError> {
        let core_context = context.take_core().ok_or_else(MobileError::conflict)?;
        open_mobile_core(
            config_path,
            secret_store,
            app_version,
            Some(core_context),
            Some(context.dispatcher()),
            None,
        )
    }

    /// Create a Core instance whose system trust comes from platform anchors
    /// the app supplies as data.
    ///
    /// Used where Rust cannot enumerate the platform certificate store. The
    /// bundle is read once at construction; there is no process-global state
    /// and no ordering requirement beyond this call.
    #[uniffi::constructor]
    pub fn new_with_diagnostic_context_and_platform_trust(
        config_path: String,
        secret_store: Arc<dyn SecretStore>,
        app_version: String,
        context: Arc<DiagnosticContext>,
        pem: Vec<u8>,
    ) -> Result<Arc<Self>, MobileError> {
        let platform_trust = platform_trust_pem(pem)?;
        let core_context = context.take_core().ok_or_else(MobileError::conflict)?;
        open_mobile_core(
            config_path,
            secret_store,
            app_version,
            Some(core_context),
            Some(context.dispatcher()),
            Some(platform_trust),
        )
    }

    /// Create a Core instance whose system trust comes from platform anchors
    /// the app supplies as data, without a diagnostic context.
    #[uniffi::constructor]
    pub fn new_with_platform_trust(
        config_path: String,
        secret_store: Arc<dyn SecretStore>,
        app_version: String,
        pem: Vec<u8>,
    ) -> Result<Arc<Self>, MobileError> {
        let platform_trust = platform_trust_pem(pem)?;
        open_mobile_core(
            config_path,
            secret_store,
            app_version,
            None,
            None,
            Some(platform_trust),
        )
    }

    /// Create a Core instance with a process-local secret store.
    ///
    /// Intended for Simulator and Rust tests: secrets are lost on exit and are
    /// never written to disk. Production uses `new` with the Keychain store.
    #[uniffi::constructor]
    pub fn new_with_memory_secret_store(
        config_path: String,
        app_version: String,
    ) -> Result<Arc<Self>, MobileError> {
        Self::new(config_path, MemorySecretStore::new(), app_version)
    }

    /// Build-time provenance: `<git-sha>[-dirty] core:<version>`.
    pub fn core_revision(&self) -> String {
        core_revision()
    }

    // -----------------------------------------------------------------
    // Pairing
    // -----------------------------------------------------------------

    /// Start pairing with an invitation URI.
    ///
    /// Spawns Core pairing and returns the Host prompt once Core has verified
    /// the Noise handshake. The same prompt is queued as `pairingPrompt`.
    /// Pairing completes asynchronously; `pollEvents` reports
    /// `pairingFinished`.
    pub fn pairing_start(&self, invitation: String) -> Result<PairingPromptView, MobileError> {
        if invitation.is_empty() || invitation.len() > MAX_INVITATION_BYTES {
            return Err(MobileError::invalid_input());
        }
        let (generation, prompt_rx, result_rx) = {
            let mut state = self.lock_pairing();
            if state.running {
                return Err(MobileError::conflict());
            }
            let generation = self.next_pairing_generation();
            state.generation = generation;
            state.running = true;
            state.decision = None;
            state.abort = None;

            let (prompt_tx, prompt_rx) = std::sync::mpsc::channel();
            let (decision_tx, decision_rx) = std::sync::mpsc::channel();
            state.decision = Some(decision_tx);
            let (result_tx, result_rx) = std::sync::mpsc::channel();

            let core = self.core.clone();
            let events = self.events.clone();
            let generations = self.pairing_generation.clone();
            let pairing = self.pairing.clone();
            let handle = self.runtime.handle.spawn(async move {
                let decide_events = events.clone();
                let decide_generations = generations.clone();
                let decide = move |prompt: &PairingPrompt| {
                    let view = PairingPromptView::from(prompt.clone());
                    if decide_generations.load(Ordering::SeqCst) == generation {
                        decide_events.push(ConversationEvent::PairingPrompt {
                            prompt: view.clone(),
                        });
                    }
                    let _ = prompt_tx.send(view);
                    // Pairing runs on our multi-thread runtime; block_in_place
                    // keeps the worker pool available while the user decides.
                    tokio::task::block_in_place(|| {
                        decision_rx
                            .recv_timeout(PAIRING_DECISION_TIMEOUT)
                            .unwrap_or_else(|_| PairingDecision::reject())
                    })
                };
                let result = core.connect_standalone(&invitation, decide).await;
                let result = result.map_err(MobileError::from);
                let current = generations.load(Ordering::SeqCst) == generation;
                if current {
                    {
                        let mut state = pairing
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if state.generation == generation {
                            state.running = false;
                            state.decision = None;
                        }
                    }
                    match &result {
                        Ok(outcome) => events.push(ConversationEvent::PairingFinished {
                            host_id: Some(outcome.host_id.clone()),
                            code: None,
                        }),
                        Err(error) => events.push(ConversationEvent::PairingFinished {
                            host_id: None,
                            code: Some(error.code().to_owned()),
                        }),
                    }
                }
                let _ = result_tx.send(result);
            });
            state.abort = Some(handle.abort_handle());
            (generation, prompt_rx, result_rx)
        };

        match prompt_rx.recv_timeout(PAIRING_PROMPT_TIMEOUT) {
            Ok(prompt) => Ok(prompt),
            // The task ended before any prompt: report its typed failure.
            Err(RecvTimeoutError::Disconnected) => {
                let result = match result_rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(_)) => Err(MobileError::internal()),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(MobileError::internal()),
                };
                if result.is_err() {
                    self.clear_pairing(generation);
                }
                result
            }
            // No prompt reached the caller, so this attempt must not keep
            // blocking a retry: fence its generation and abort the Core task.
            // A prompt that races this timeout is rejected, never delivered,
            // and the abandoned attempt reports no pairing event.
            Err(RecvTimeoutError::Timeout) => {
                let abort = {
                    let mut state = self.lock_pairing();
                    if state.generation == generation {
                        self.next_pairing_generation();
                        state.running = false;
                        state.decision = None;
                        state.abort.take()
                    } else {
                        None
                    }
                };
                if let Some(abort) = abort {
                    abort.abort();
                }
                Err(MobileError::timeout())
            }
        }
    }

    /// Answer the outstanding pairing prompt.
    ///
    /// `expectedFingerprint` is checked by Core against the confirmed Host
    /// fingerprint; a mismatch aborts pairing without sending the final step.
    pub fn pairing_decide(
        &self,
        approved: bool,
        expected_fingerprint: Option<String>,
    ) -> Result<(), MobileError> {
        if let Some(expected) = &expected_fingerprint {
            validate_fingerprint(expected)?;
        }
        let sender = {
            let mut state = self.lock_pairing();
            state.decision.take()
        };
        let Some(sender) = sender else {
            return Err(MobileError::conflict());
        };
        let decision = if approved {
            match expected_fingerprint {
                Some(expected) => PairingDecision::expect(expected),
                None => PairingDecision::approve(),
            }
        } else {
            PairingDecision::reject()
        };
        sender.send(decision).map_err(|_| MobileError::conflict())
    }

    /// Reject a pending prompt and abort the pairing task.
    pub fn pairing_cancel(&self) {
        let abort = {
            let mut state = self.lock_pairing();
            if !state.running {
                return;
            }
            self.next_pairing_generation();
            state.running = false;
            state.decision = None;
            state.abort.take()
        };
        if let Some(abort) = abort {
            abort.abort();
        }
        self.events.push(ConversationEvent::PairingFinished {
            host_id: None,
            code: Some(CODE_CANCELLED.to_owned()),
        });
    }

    // -----------------------------------------------------------------
    // Hosts and sessions
    // -----------------------------------------------------------------

    /// List paired Hosts from Access. Unpaired calls fail with `not_paired`.
    pub fn hosts(&self) -> Result<Vec<HostSummary>, MobileError> {
        let hosts = self.run(QUERY_TIMEOUT, self.core.hosts())?;
        Ok(hosts.into_iter().map(HostSummary::from).collect())
    }

    /// List sessions for one Host, falling back to the local cache when the
    /// Host is offline. `source` is not exposed; callers refresh with
    /// `cachedSessions` when they need an explicit cache-only view.
    pub fn sessions(&self, host_id: String) -> Result<Vec<SessionSummaryView>, MobileError> {
        validate_id(&host_id)?;
        let list = self.run(QUERY_TIMEOUT, self.core.sessions(&host_id))?;
        Ok(list.sessions.into_iter().map(Into::into).collect())
    }

    /// Source capabilities for one Host-qualified session.
    ///
    /// The session is resolved through the Host's session list. A session that
    /// is not present fails with `not_found`; a Source that did not report
    /// capabilities fails with `unavailable`. No defaults are fabricated.
    pub fn capabilities(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<SessionCapabilitiesView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let list = self.run(QUERY_TIMEOUT, self.core.sessions(&host_id))?;
        let session = list
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .ok_or_else(MobileError::not_found)?;
        session
            .source_capabilities
            .map(SessionCapabilitiesView::from)
            .ok_or_else(MobileError::unavailable)
    }

    /// Committed messages cached for one Host-qualified session.
    ///
    /// Local-only: this never opens a channel. Order is Core's committed order;
    /// an empty cache returns an empty list and an unpaired profile fails with
    /// `not_paired`. The same projection is used for live `Message` events.
    pub fn cached_messages(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<Vec<MessageView>, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let messages = self.core.cached_messages(&key).map_err(MobileError::from)?;
        Ok(messages.iter().map(message_view).collect())
    }

    /// Cached sessions for one Host without a network call.
    pub fn cached_sessions(&self, host_id: String) -> Result<Vec<SessionSummaryView>, MobileError> {
        validate_id(&host_id)?;
        let sessions = self
            .core
            .cached_sessions(&host_id)
            .map_err(MobileError::from)?;
        Ok(sessions.into_iter().map(Into::into).collect())
    }

    /// The authoritative device connection for one Host.
    ///
    /// `None` means it was never opened; that is different from `offline`.
    pub fn host_connection(&self, host_id: String) -> Option<HostConnectionView> {
        validate_id(&host_id).ok()?;
        self.core.connection_snapshot(&host_id).map(connection_view)
    }

    /// Start the app-level probe: connect every Host and stream transitions.
    ///
    /// The connection is Host-scoped and survives session switches. Events are
    /// unscoped, so they are delivered with no session open and are not dropped
    /// when a watch is replaced.
    pub fn start_host_monitor(&self, host_ids: Vec<String>) {
        let mut hosts = Vec::new();
        for host_id in host_ids {
            if validate_id(&host_id).is_ok() && !hosts.contains(&host_id) {
                hosts.push(host_id);
            }
        }
        if let Some(abort) = self.monitor.lock().expect("monitor").take() {
            abort.abort();
        }
        if hosts.is_empty() {
            return;
        }
        let core = self.core.clone();
        let events = self.events.clone();
        let scope_hosts = hosts.clone();
        let task = self.runtime.handle.spawn(async move {
            let mut changes = core.subscribe_connections();
            for host_id in &scope_hosts {
                let core = core.clone();
                let host_id = host_id.clone();
                tokio::spawn(async move {
                    // A failure is reported through the snapshot; the probe
                    // itself must not abort the monitor.
                    let _ = core.connect_host(&host_id).await;
                });
            }
            loop {
                match changes.recv().await {
                    Ok(snapshot) if scope_hosts.contains(&snapshot.host_id) => {
                        push_host_connection(&events, &snapshot);
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        *self.monitor.lock().expect("monitor") = Some(task.abort_handle());
        *self.monitored.lock().expect("monitored") = hosts;
    }

    /// Stop the probe and close the Host connections it opened.
    pub fn stop_host_monitor(&self) {
        if let Some(abort) = self.monitor.lock().expect("monitor").take() {
            abort.abort();
        }
        let hosts = std::mem::take(&mut *self.monitored.lock().expect("monitored"));
        for host_id in hosts {
            self.core.disconnect_host(&host_id);
        }
    }

    // -----------------------------------------------------------------
    // Session management
    // -----------------------------------------------------------------

    /// Whether the Host advertises native session creation.
    ///
    /// A peer that predates the capability reports `false`; the caller must
    /// hide or disable creation instead of submitting a lifecycle command.
    pub fn session_create_supported(&self, host_id: String) -> Result<bool, MobileError> {
        validate_id(&host_id)?;
        self.run(QUERY_TIMEOUT, self.core.session_create_supported(&host_id))
    }

    /// Create one Host session. Every optional input is forwarded unchanged;
    /// Core and the Host choose defaults, validate combinations and report the
    /// actual returned session identity.
    pub fn create_session(
        &self,
        host_id: String,
        input: crate::session_management::SessionCreateInputView,
    ) -> Result<crate::session_management::SessionCreateResultView, MobileError> {
        validate_id(&host_id)?;
        let input = crate::session_management::create_input(input)?;
        self.run(
            COMMAND_TIMEOUT,
            crate::session_management::create_session(&self.core, &host_id, input),
        )
    }

    /// Read one Host-qualified session summary.
    pub fn session_details(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<crate::session_management::SessionDetailView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = crate::session_management::key(&host_id, &session_id);
        self.run(
            QUERY_TIMEOUT,
            crate::session_management::session_details(&self.core, &host_id, &key),
        )
    }

    /// Fork the latest complete native turn and resolve the child directory.
    pub fn fork_session_outcome(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<crate::session_management::ForkResultView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = crate::session_management::key(&host_id, &session_id);
        self.run(
            COMMAND_TIMEOUT,
            crate::session_management::fork_session_outcome(&self.core, &key),
        )
    }

    /// Read the current source model catalog. Offline values are never cached.
    pub fn model_catalog(
        &self,
        host_id: String,
    ) -> Result<crate::session_management::ModelCatalogView, MobileError> {
        validate_id(&host_id)?;
        self.run(
            QUERY_TIMEOUT,
            crate::session_management::model_catalog(&self.core, &host_id),
        )
    }

    /// Read the passive session model observation without activating the
    /// session.
    pub fn read_session_model(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<crate::session_management::SessionModelView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = crate::session_management::key(&host_id, &session_id);
        self.run(
            QUERY_TIMEOUT,
            crate::session_management::read_session_model(&self.core, &key),
        )
    }

    /// Submit exactly one native model selection. No prompt is sent.
    pub fn select_session_model(
        &self,
        host_id: String,
        session_id: String,
        provider: String,
        model: String,
        effort: Option<String>,
    ) -> Result<crate::session_management::ModelSelectionResultView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&provider)?;
        validate_id(&model)?;
        if let Some(effort) = &effort {
            validate_id(effort)?;
        }
        let key = crate::session_management::key(&host_id, &session_id);
        self.run(
            COMMAND_TIMEOUT,
            crate::session_management::select_session_model(
                &self.core, &key, &provider, &model, effort,
            ),
        )
    }

    // -----------------------------------------------------------------
    // Watch and events
    // -----------------------------------------------------------------

    /// Start observing one Host-qualified session with Conversation and
    /// Questions interests on one Core observation.
    ///
    /// Replaces an existing watch; updates from a replaced watch are dropped.
    /// Pairing and command events are not scoped to the watch and survive.
    /// Returns the generation carried by this watch's events through `pollEvents`.
    pub fn start_watch(
        &self,
        host_id: String,
        session_id: String,
        baseline: WatchHistoryBaseline,
    ) -> Result<u64, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = SessionKey {
            host_id: host_id.clone(),
            session_id: session_id.clone(),
        };
        let interests = match baseline {
            WatchHistoryBaseline::Complete => ObservationInterests::both(),
            WatchHistoryBaseline::Recent {
                max_messages,
                max_bytes,
            } => ObservationInterests::paged_conversation(max_messages, max_bytes),
        };
        let (scope, previous) = {
            let mut state = self.lock_watch();
            let generation = self.next_watch_generation();
            state.generation = generation;
            state.host_id = host_id.clone();
            state.session_id = session_id.clone();
            state.active = true;
            let scope = WatchEventScope::pending(generation, host_id.clone(), session_id.clone());
            self.events.begin_watch(&scope);
            let previous = (state.watch.take(), state.watch_stopped.take());
            (scope, previous)
        };
        let generation = scope.generation;
        if let Some(abort) = previous.0 {
            abort.abort();
        }
        // Wait briefly for the replaced Core observation to release its profile
        // lease and one-observation slot before attaching the replacement.
        if let Some(stopped) = previous.1 {
            let _ = stopped.recv_timeout(Duration::from_secs(2));
        }

        let mut state = self.lock_watch();
        if state.generation != generation
            || self.watch_generation.load(Ordering::SeqCst) != generation
        {
            return Ok(generation);
        }

        let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
        let core = self.core.clone();
        let events = self.events.clone();
        let generations = self.watch_generation.clone();
        let watch_state = self.watch.clone();
        let mut producer_scope = scope;
        let task_cancellation = Cancellation::new();
        let task = self.runtime.handle.spawn(async move {
            let _stopped = WatchStopSignal(Some(stopped_tx));
            let mut observation = core
                .observe_session(&key, interests, task_cancellation)
                .await;
            // A replacement aborts the old task asynchronously. Retry the
            // attach for a bounded window instead of surfacing a spurious
            // conflict to the caller.
            let mut attempts = 0;
            while matches!(observation, Err(ref error) if error.code() == "conflict")
                && attempts < 40
                && generations.load(Ordering::SeqCst) == generation
            {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(25)).await;
                observation = core
                    .observe_session(&key, interests, Cancellation::new())
                    .await;
            }
            let result = match observation {
                Ok(mut observation) => {
                    producer_scope =
                        producer_scope.with_observation(observation.observation_id().to_owned());
                    if !events.attach_watch(&producer_scope) {
                        observation.close().await;
                        return;
                    }
                    let mut seen_approvals = None;
                    while generations.load(Ordering::SeqCst) == generation {
                        let Some(event) = observation.next_event().await else {
                            break;
                        };
                        if event.session != key
                            || event.observation_id != observation.observation_id()
                            || event.generation != observation.generation()
                        {
                            continue;
                        }
                        match event.payload {
                            SessionEventPayload::Conversation(update) => {
                                for event in convert_update(&update, &mut seen_approvals) {
                                    events.push_watch(&producer_scope, event);
                                }
                            }
                            SessionEventPayload::Questions(view) => {
                                events.push_watch(
                                    &producer_scope,
                                    ConversationEvent::Questions {
                                        view: bounded_question_view(questions_view(&view)),
                                    },
                                );
                            }
                            SessionEventPayload::HistorySnapshot(snapshot) => {
                                let continuity = match snapshot.continuity {
                                    HistoryContinuity::Continuous => {
                                        HistoryContinuityView::Continuous
                                    }
                                    HistoryContinuity::Replacement => {
                                        HistoryContinuityView::Replacement
                                    }
                                };
                                events.push_watch(
                                    &producer_scope,
                                    ConversationEvent::HistorySnapshot {
                                        page: HistoryPageView::from(&snapshot.page),
                                        continuity,
                                    },
                                );
                            }
                            SessionEventPayload::HistoryUnavailable { code } => {
                                events.push_watch(
                                    &producer_scope,
                                    ConversationEvent::HistoryUnavailable {
                                        code: from_core_code(&code).code().into(),
                                    },
                                );
                            }
                        }
                    }
                    let error = observation.take_terminal_error();
                    observation.close().await;
                    error.map_or(Ok(()), Err)
                }
                Err(error) => Err(error),
            };
            if generations.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Err(error) = result {
                let error = MobileError::from(error);
                events.push_watch(
                    &producer_scope,
                    ConversationEvent::Error {
                        code: error.code().to_owned(),
                        message: error.message().to_owned(),
                    },
                );
            }
            events.end_watch(&producer_scope);
            let mut state = watch_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.generation == generation {
                state.active = false;
            }
        });

        state.watch = Some(task.abort_handle());
        state.watch_stopped = Some(stopped_rx);
        Ok(generation)
    }

    /// Stop observing and emit `watchEnded`.
    pub fn stop_watch(&self) {
        let (scope, abort) = {
            let mut state = self.lock_watch();
            if !state.active {
                return;
            }
            self.next_watch_generation();
            state.active = false;
            let scope = WatchEventScope::pending(
                state.generation,
                state.host_id.clone(),
                state.session_id.clone(),
            );
            (scope, state.watch.take())
        };
        if let Some(abort) = abort {
            abort.abort();
        }
        self.events.end_watch(&scope);
    }

    /// Drain queued events, waiting up to `timeoutMs` when the queue is empty.
    ///
    /// Returns an empty array on timeout. The timeout is capped at 30 seconds.
    pub fn poll_events(&self, timeout_ms: u64) -> Vec<ConversationEventEnvelope> {
        let timeout = Duration::from_millis(timeout_ms).min(MAX_POLL_TIMEOUT);
        self.events.poll(timeout)
    }

    // -----------------------------------------------------------------
    // Commands and approvals
    // -----------------------------------------------------------------

    /// Send text to a session with an explicit delivery choice.
    pub fn send(
        &self,
        host_id: String,
        session_id: String,
        text: String,
        delivery: DeliveryChoice,
    ) -> Result<CommandView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_text(&text)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let operation_id = Core::new_command_id();
        let operation = self.run(
            COMMAND_TIMEOUT,
            self.core
                .submit_message_operation(&operation_id, &key, &text, delivery.to_core()),
        )?;
        let handle = operation
            .receipt
            .ok_or_else(|| MobileError::from(CoreError::UnknownResult))?;
        Ok(self.publish_command(&key, handle))
    }

    /// Send with a stable operation identity chosen before submission.
    ///
    /// Core's existing durable operation record is reused, so recovery and
    /// duplicate submission use exactly the same persistence as `send`. An
    /// unknown receipt is reported as `receipt: None` rather than fabricated.
    pub fn send_with_operation_id(
        &self,
        host_id: String,
        session_id: String,
        text: String,
        delivery: DeliveryChoice,
        operation_id: String,
    ) -> Result<SendOperationView, MobileError> {
        validate_operation_id(&operation_id)?;
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_text(&text)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let operation = self.run(
            COMMAND_TIMEOUT,
            self.core
                .submit_message_operation(&operation_id, &key, &text, delivery.to_core()),
        )?;
        Ok(SendOperationView {
            operation_id,
            host_id: operation.target.host_id,
            session_id: operation.target.session_id,
            receipt: operation.receipt.map(Into::into),
            reply: None,
        })
    }

    /// Recover a previously started send, with its original Host and Session.
    pub fn recover_send(
        &self,
        operation_id: String,
        follow: bool,
    ) -> Result<crate::types::SendOperationView, MobileError> {
        validate_id(&operation_id)?;
        let operation = self.run(
            COMMAND_TIMEOUT,
            self.core.recover_send_operation(&operation_id),
        )?;
        let reply = if follow {
            Some(
                self.run(
                    COMMAND_TIMEOUT,
                    self.core.follow_send_operation(
                        operation.clone(),
                        COMMAND_TIMEOUT - Duration::from_secs(1),
                    ),
                )?
                .into(),
            )
        } else {
            None
        };
        Ok(crate::types::SendOperationView {
            operation_id,
            host_id: operation.target.host_id,
            session_id: operation.target.session_id,
            receipt: operation.receipt.map(Into::into),
            reply,
        })
    }

    /// List locally retained send operations. Local state only: this never
    /// performs a status lookup and never sends or resumes pairing.
    pub fn list_send_operations(&self) -> Result<Vec<RetainedOperationView>, MobileError> {
        let operations = self.core.send_operations().map_err(MobileError::from)?;
        Ok(operations.iter().map(retained_send_view).collect())
    }

    /// Recover one retained send and optionally follow its exact reply.
    ///
    /// With `follow == false` this performs only the status lookup Core's send
    /// recovery path needs, which is the automatic foreground/reconnect path.
    /// With `follow == true` it reads the confirmed reply reference and its
    /// canonical content within `timeout_ms`. A failed content read keeps the
    /// confirmation and reports `read_error`; it never resubmits.
    pub fn recover_send_with_content(
        &self,
        operation_id: String,
        follow: bool,
        timeout_ms: u64,
    ) -> Result<RetainedOperationView, MobileError> {
        validate_id(&operation_id)?;
        if timeout_ms == 0 || timeout_ms > MAX_FOLLOW_TIMEOUT_MS {
            return Err(MobileError::invalid_input());
        }
        let operation = self.run(
            COMMAND_TIMEOUT,
            self.core.recover_send_operation(&operation_id),
        )?;
        let mut view = retained_send_view(&operation);
        if follow {
            let follow_up = self.run(
                COMMAND_TIMEOUT,
                self.core.follow_send_operation_with_content(
                    operation,
                    Duration::from_millis(timeout_ms),
                ),
            )?;
            view.read_error = follow_up.read_error_code().map(str::to_owned);
            view.reply = Some(follow_up.reply.into());
            view.content = follow_up.message.as_ref().map(message_view);
        }
        Ok(view)
    }

    /// List retained non-send operations: durable control identities and any
    /// retained Standalone pairing FINISH. Local state only.
    pub fn list_pending_commands(&self) -> Result<Vec<RetainedOperationView>, MobileError> {
        let mut views = Vec::new();
        if let Some((operation_id, host_id)) = self
            .core
            .pending_standalone_pairing()
            .map_err(MobileError::from)?
        {
            views.push(RetainedOperationView {
                operation_id,
                kind: "pairing".into(),
                host_id: Some(host_id),
                session_id: None,
                status: "submitted".into(),
                receipt: None,
                reply: None,
                content: None,
                read_error: None,
            });
        }
        for pending in self.core.pending_commands().map_err(MobileError::from)? {
            let receipt = CommandView {
                request_id: pending.request_id.clone(),
                command_id: pending.command_id.clone(),
                status: pending.status.as_str().to_owned(),
                message_id: None,
                delivery_id: None,
            };
            views.push(RetainedOperationView {
                operation_id: pending.request_id,
                kind: "control".into(),
                host_id: Some(pending.host_id),
                session_id: None,
                status: operation_status_name(pending.status).to_owned(),
                receipt: Some(receipt),
                reply: None,
                content: None,
                read_error: None,
            });
        }
        views.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
        Ok(views)
    }

    /// Manually recover one retained control command or Standalone pairing
    /// operation under its original identity. This is never called by the
    /// automatic refresh and never starts a new pairing attempt.
    pub fn recover_pending_command(
        &self,
        operation_id: String,
    ) -> Result<RetainedOperationView, MobileError> {
        validate_id(&operation_id)?;
        if self
            .core
            .has_standalone_pairing_operation(&operation_id)
            .map_err(MobileError::from)?
        {
            let outcome = self.run(
                COMMAND_TIMEOUT,
                self.core.recover_standalone_pairing(&operation_id),
            )?;
            return Ok(RetainedOperationView {
                operation_id,
                kind: "pairing".into(),
                host_id: Some(outcome.host_id),
                session_id: None,
                status: "confirmed".into(),
                receipt: None,
                reply: None,
                content: None,
                read_error: None,
            });
        }
        let pending = self
            .core
            .pending_commands()
            .map_err(MobileError::from)?
            .into_iter()
            .find(|command| command.request_id == operation_id);
        let handle = self.run(
            COMMAND_TIMEOUT,
            self.core.recover_pending_command(&operation_id),
        )?;
        Ok(RetainedOperationView {
            operation_id,
            kind: "control".into(),
            host_id: pending.map(|command| command.host_id),
            session_id: None,
            status: operation_status_name(handle.status).to_owned(),
            receipt: Some(handle.into()),
            reply: None,
            content: None,
            read_error: None,
        })
    }

    /// Read one bounded latest-first page of committed history.
    ///
    /// `cache == true` serves the encrypted local cache without a network call;
    /// otherwise the page comes from the Host. `continuation` is the opaque
    /// token from a previous page and is only valid for the original
    /// Host/session scope and projection. A stale, expired or foreign token is
    /// `stale_history` and never yields content from another traversal.
    pub fn history_page(
        &self,
        host_id: String,
        session_id: String,
        cache: bool,
        continuation: Option<String>,
        max_messages: u32,
        max_bytes: u32,
    ) -> Result<HistoryPageView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let session = SessionKey {
            host_id,
            session_id,
        };
        let mut request = HistoryPageRequest::new(session);
        if cache {
            request = request.cache();
        }
        if let Some(continuation) = continuation {
            if continuation.is_empty() || continuation.len() > 8192 {
                return Err(MobileError::stale_history());
            }
            let continuation = continuation
                .parse::<HistoryContinuation>()
                .map_err(|_| MobileError::stale_history())?;
            request = request.continuation(continuation);
        }
        request.max_messages = max_messages;
        request.max_bytes = max_bytes;
        self.run(QUERY_TIMEOUT, self.core.history_page(&request))
            .map(|page| HistoryPageView::from(&page))
    }

    /// Read one exact scoped committed message from Core history.
    ///
    /// The reference is reconstructed from the typed fields, including the
    /// opaque `source_locator`; a malformed or foreign reference is a typed
    /// error and never falls back to a different message.
    pub fn read_history_message(
        &self,
        reference: MessageReferenceView,
    ) -> Result<MessageView, MobileError> {
        validate_id(&reference.session.host_id)?;
        validate_id(&reference.session.session_id)?;
        let reference = reference
            .to_core()
            .map_err(|()| MobileError::invalid_input())?;
        let message = self.run(QUERY_TIMEOUT, self.core.history_message(&reference))?;
        Ok(message_view(&message))
    }

    /// Start a verified attachment download into an app-owned destination.
    ///
    /// The transfer runs on this object's runtime; progress and the terminal
    /// verified path are observed through `attachmentDownloadProgress` and
    /// `takeAttachmentResult`. Incomplete staging data is removed by Core on
    /// failure or cancellation, and an existing final file is preserved unless
    /// `overwrite` is true.
    pub fn begin_attachment_download(
        &self,
        request: AttachmentDownloadRequestView,
    ) -> Result<String, MobileError> {
        validate_id(&request.reference.session.host_id)?;
        validate_id(&request.reference.session.session_id)?;
        validate_id(&request.reference.attachment_ref)?;
        if request.destination.is_empty() || request.destination.len() > MAX_PATH_BYTES {
            return Err(MobileError::invalid_input());
        }
        if let Some(name) = request.file_name.as_deref() {
            if name.is_empty() || name.len() > 512 {
                return Err(MobileError::invalid_input());
            }
        }
        let reference = request
            .reference
            .to_core()
            .map_err(|()| MobileError::invalid_input())?;
        let mut download =
            DownloadRequest::new(reference, Path::new(&request.destination).to_path_buf());
        download.file_name = request.file_name;
        download.overwrite = request.overwrite;

        let id = format!(
            "attachment-{:08x}",
            self.attachment_transfers
                .next
                .fetch_add(1, Ordering::SeqCst)
        );
        let state = Arc::new(AttachmentTransferState::new(id.clone()));
        self.attachment_transfers.insert(Arc::clone(&state))?;

        let core = self.core.clone();
        let task_state = Arc::clone(&state);
        self.runtime.handle.spawn(async move {
            let progress_state = Arc::clone(&task_state);
            let cancellation = task_state.cancellation.clone();
            let outcome = core
                .download_attachment(download, &cancellation, move |progress| {
                    progress_state.set_progress(progress);
                })
                .await;
            let terminal = match outcome {
                Ok(download) => AttachmentTerminal {
                    state: "succeeded",
                    download: Some(AttachmentDownloadView::from(download)),
                    error: None,
                },
                Err(error) => {
                    let mapped = MobileError::from(error);
                    AttachmentTerminal {
                        state: if mapped.code() == crate::error::CODE_CANCELLED {
                            "cancelled"
                        } else {
                            "failed"
                        },
                        download: None,
                        error: Some(mapped),
                    }
                }
            };
            task_state.finish(terminal);
        });
        Ok(id)
    }

    /// Latest bounded progress for one app-started transfer, if it is known.
    pub fn attachment_download_progress(
        &self,
        download_id: String,
    ) -> Option<AttachmentProgressView> {
        self.attachment_transfers.get(&download_id).map(|state| {
            state
                .progress
                .lock()
                .map(|value| *value)
                .unwrap_or(AttachmentProgressView {
                    received: 0,
                    total: 0,
                })
        })
    }

    /// Wait up to `waitMs` for one transfer to reach a terminal state.
    ///
    /// A timeout returns the current `running` view rather than an error, so the
    /// caller can keep rendering progress and poll again. Terminal failures are
    /// returned as typed `failed`/`cancelled` views with stable codes.
    pub fn take_attachment_result(
        &self,
        download_id: String,
        wait_ms: u64,
    ) -> Result<AttachmentTransferView, MobileError> {
        if wait_ms > MAX_ATTACHMENT_WAIT_MS {
            return Err(MobileError::invalid_input());
        }
        let state = self
            .attachment_transfers
            .get(&download_id)
            .ok_or_else(MobileError::not_found)?;
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            if let Some(terminal) = state
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                self.attachment_transfers.remove(&download_id);
                return Ok(state.view(Some(terminal)));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(state.view(None));
            }
            let guard = state
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.is_some() {
                continue;
            }
            let _ = state
                .notify
                .wait_timeout(guard, deadline.saturating_duration_since(now));
        }
    }

    /// Request cancellation of one transfer. Returns false for an unknown id.
    ///
    /// Cancellation is cooperative: Core removes incomplete staging data and
    /// preserves existing finals; the terminal view becomes `cancelled`.
    pub fn cancel_attachment_download(&self, download_id: String) -> bool {
        match self.attachment_transfers.get(&download_id) {
            Some(state) => {
                state.cancellation.cancel();
                true
            }
            None => false,
        }
    }

    /// Inspect selected paths and local service and authorization state.
    pub fn status(&self) -> Result<crate::types::ClientStatusView, MobileError> {
        self.run(QUERY_TIMEOUT, self.core.status()).map(Into::into)
    }

    /// The default Host recorded for this client instance, if any.
    ///
    /// The record lives in this instance's Core data directory, so another
    /// instance never sees or overwrites it.
    pub fn default_host(&self) -> Result<Option<String>, MobileError> {
        self.core.default_host().map_err(MobileError::from)
    }

    /// Persist `host_id` as the default Host after Core confirms it is
    /// authorized and reachable. Only the selected instance is affected.
    pub fn select_default_host(&self, host_id: String) -> Result<(), MobileError> {
        validate_id(&host_id)?;
        self.run(QUERY_TIMEOUT, self.core.select_default_host(&host_id))
    }

    /// The Cloud app session identity retained for this instance, when signed in.
    pub fn current_cloud_session_id(&self) -> Result<Option<String>, MobileError> {
        self.core
            .current_cloud_session_id()
            .map_err(MobileError::from)
    }

    /// Interrupt the running turn for a session.
    pub fn stop(&self, host_id: String, session_id: String) -> Result<CommandView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let handle = self.run(COMMAND_TIMEOUT, self.core.interrupt(&key))?;
        Ok(self.publish_command(&key, handle))
    }

    // -----------------------------------------------------------------
    // Pending queue
    // -----------------------------------------------------------------

    /// Read the ordered pending queue from the Source.
    ///
    /// `available == false` is not an empty queue: the Source did not confirm a
    /// readable listing. Queue contents are never cached here.
    pub fn pending_queue(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<PendingQueueView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let queue = self.run(QUERY_TIMEOUT, self.core.pending_queue(&key))?;
        Ok(PendingQueueView::from(queue))
    }

    /// Apply one targeted source-owned queue action.
    ///
    /// When the fresh listing still contains the item, the action must be one
    /// the Source advertises for it. If the item disappeared in the same
    /// window, the mutation is still submitted so the Host's native receipt
    /// owns the consumed-before-mutation outcome.
    pub fn update_pending_queue(
        &self,
        host_id: String,
        session_id: String,
        item_id: String,
        action: QueueAction,
        text: String,
    ) -> Result<QueueMutationView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&item_id)?;
        match action {
            QueueAction::Edit => validate_text(&text)?,
            QueueAction::Remove | QueueAction::Promote if !text.is_empty() => {
                return Err(MobileError::invalid_input());
            }
            QueueAction::Remove | QueueAction::Promote => {}
        }
        let key = SessionKey {
            host_id: host_id.clone(),
            session_id: session_id.clone(),
        };
        if let Ok(queue) = self.run(QUERY_TIMEOUT, self.core.pending_queue(&key)) {
            if queue.available {
                if let Some(item) = queue.items.iter().find(|item| item.item_id == item_id) {
                    if !action_is_advertised(&PendingQueueItemView::from(item.clone()), action) {
                        return Err(MobileError::invalid_input());
                    }
                }
            }
        }
        let handle = self.run(
            COMMAND_TIMEOUT,
            self.core
                .update_pending_queue(&key, &item_id, action.to_core(), &text),
        )?;
        let receipt = self.publish_command(&key, handle);
        Ok(QueueMutationView {
            operation_id: receipt.command_id.clone(),
            host_id,
            session_id,
            item_id,
            action: action.as_str().to_owned(),
            receipt,
        })
    }

    // -----------------------------------------------------------------
    // Wait
    // -----------------------------------------------------------------

    /// Start one background state-only wait and return its attempt identity.
    ///
    /// The conversation watch is independent: cancelling this attempt aborts
    /// only the wait task.
    pub fn begin_wait(
        &self,
        host_id: String,
        session_id: String,
        timeout_ms: u64,
    ) -> Result<String, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        if timeout_ms == 0 || timeout_ms > MAX_WAIT_TIMEOUT_MS {
            return Err(MobileError::invalid_input());
        }
        let key = SessionKey {
            host_id,
            session_id,
        };
        Ok(self
            .wait_attempts
            .begin(self.core.clone(), &self.runtime.handle, key, timeout_ms))
    }

    /// Wait up to `wait_ms` for one attempt to settle.
    ///
    /// `None` means the native attempt is still running; the same attempt
    /// identity remains valid for a later take.
    pub fn take_wait_result(
        &self,
        wait_id: String,
        wait_ms: u64,
    ) -> Result<Option<WaitResultView>, MobileError> {
        validate_id(&wait_id)?;
        self.wait_attempts.take(&wait_id, wait_ms)
    }

    /// Abort only this wait attempt. Returns whether an unsettled attempt was
    /// cancelled.
    pub fn cancel_wait(&self, wait_id: String) -> Result<bool, MobileError> {
        validate_id(&wait_id)?;
        Ok(self.wait_attempts.cancel(&wait_id))
    }

    /// List pending approvals for one session.
    pub fn approvals(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<Vec<ApprovalView>, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let approvals = self.run(
            QUERY_TIMEOUT,
            self.core.list_approvals(&host_id, &session_id),
        )?;
        Ok(approvals.into_iter().map(ApprovalView::from).collect())
    }

    /// Read one approval's exact scoped state by identity.
    ///
    /// The Host is never guessed: the caller's original scope is used and the
    /// returned state/version/digest/expiry is authoritative for the card.
    pub fn get_approval(
        &self,
        host_id: String,
        approval_id: String,
    ) -> Result<ApprovalView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&approval_id)?;
        let approval = self.run(
            QUERY_TIMEOUT,
            self.core.get_approval(&host_id, &approval_id),
        )?;
        Ok(ApprovalView::from(approval))
    }

    /// Resolve exactly the approval version and digest the user reviewed.
    ///
    /// `digestHex` must be the lowercase or uppercase hex returned in
    /// `ApprovalView.digestHex`.
    pub fn resolve_approval(
        &self,
        host_id: String,
        session_id: String,
        approval_id: String,
        version: i64,
        digest_hex: String,
        allow: bool,
    ) -> Result<CommandView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&approval_id)?;
        if version <= 0 {
            return Err(MobileError::invalid_input());
        }
        let digest = decode_digest(&digest_hex)?;
        let handle = if allow {
            self.run(
                COMMAND_TIMEOUT,
                self.core.approve(&host_id, &approval_id, &digest, version),
            )?
        } else {
            self.run(
                COMMAND_TIMEOUT,
                self.core.reject(&host_id, &approval_id, &digest, version),
            )?
        };
        Ok(CommandView::from(handle))
    }

    // -----------------------------------------------------------------
    // Questions
    // -----------------------------------------------------------------

    /// One authoritative question page through Core's shared reader.
    ///
    /// `cursor` is Core's opaque continuation cursor; pass an empty string for
    /// the first page. Reading never acquires a native answer claim.
    pub fn list_questions(
        &self,
        host_id: String,
        session_id: String,
        cursor: String,
    ) -> Result<QuestionPageView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        if cursor.len() > MAX_TEXT_BYTES {
            return Err(MobileError::invalid_input());
        }
        let key = SessionKey {
            host_id,
            session_id,
        };
        let page = self.run(QUERY_TIMEOUT, self.core.read_questions(&key, &cursor))?;
        Ok(question_page_view(&page))
    }

    /// One authoritative question batch through the shared reader.
    pub fn get_question(
        &self,
        host_id: String,
        session_id: String,
        batch_id: String,
    ) -> Result<QuestionBatchView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&batch_id)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let batch = self.run(QUERY_TIMEOUT, self.core.read_question(&key, &batch_id))?;
        Ok(question_batch_view(&batch))
    }

    /// Submit exactly the reviewed batch guard and answers.
    ///
    /// The binding never reconstructs or "repairs" a guard: Core revalidates
    /// the session, owner epoch, version, digest, phase and answerability. A
    /// failed validation returns a typed error without a native submission.
    pub fn answer_question(
        &self,
        host_id: String,
        session_id: String,
        batch: QuestionBatchView,
        answers: Vec<QuestionAnswerView>,
    ) -> Result<CommandView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&batch.batch_id)?;
        if batch.session_id != session_id || answers.is_empty() {
            return Err(MobileError::invalid_input());
        }
        let digest = decode_digest(&batch.digest_hex)?;
        if digest.len() != 32 || batch.source_epoch.is_empty() || batch.version == 0 {
            return Err(MobileError::invalid_input());
        }
        let key = SessionKey {
            host_id,
            session_id,
        };
        let guard = question_guard(&batch);
        let answers: Vec<QuestionAnswer> = answers.into_iter().map(Into::into).collect();
        let handle = self.run(
            COMMAND_TIMEOUT,
            self.core.answer_question(&key, &guard, answers),
        )?;
        Ok(self.publish_command(&key, handle))
    }

    /// Ask Core to cancel an in-flight open answer by its original command.
    /// `false` does not establish that no native answer occurred.
    pub fn cancel_question_answer(
        &self,
        host_id: String,
        session_id: String,
        command_id: String,
    ) -> Result<bool, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&command_id)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        self.run(
            QUERY_TIMEOUT,
            self.core.cancel_question_answer(&key, &command_id),
        )
    }

    /// Cancel an in-flight open answer and return the original command identity
    /// alongside the request outcome. A `false` request does not establish that
    /// no native answer occurred; the caller retains the identity for recovery.
    pub fn request_question_answer_cancel(
        &self,
        host_id: String,
        session_id: String,
        command_id: String,
    ) -> Result<QuestionCancelReceiptView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        validate_id(&command_id)?;
        let key = SessionKey {
            host_id,
            session_id,
        };
        let cancellation_requested = self.run(
            QUERY_TIMEOUT,
            self.core.cancel_question_answer(&key, &command_id),
        )?;
        Ok(QuestionCancelReceiptView {
            command_id,
            cancellation_requested,
        })
    }

    // -----------------------------------------------------------------
    // Drafts
    // -----------------------------------------------------------------

    /// Persist an encrypted draft for one explicit Host and session scope.
    /// Empty text clears the draft.
    pub fn save_draft(
        &self,
        host_id: String,
        session_id: String,
        text: String,
    ) -> Result<(), MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        if text.len() > MAX_TEXT_BYTES {
            return Err(MobileError::invalid_input());
        }
        self.core
            .save_draft(&host_id, &session_id, &text)
            .map_err(MobileError::from)
    }

    /// Load the draft for one Host-qualified session, if any.
    pub fn load_draft(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<Option<String>, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        self.core
            .load_draft(&host_id, &session_id)
            .map_err(MobileError::from)
    }

    /// Clear the draft for one Host-qualified session.
    pub fn clear_draft(&self, host_id: String, session_id: String) -> Result<(), MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        self.core
            .clear_draft(&host_id, &session_id)
            .map_err(MobileError::from)
    }

    // -----------------------------------------------------------------
    // Diagnostics and lifecycle
    // -----------------------------------------------------------------

    /// Redacted diagnostic preview. Reads local state only.
    pub fn diagnostics_preview(&self) -> Result<DiagnosticsReportView, MobileError> {
        let json = self.core.diagnostics_preview().map_err(MobileError::from)?;
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|_| MobileError::internal())?;
        let redaction_fields = string_array(&value, "redactions");
        let last_failure_code = value
            .get("last_failure")
            .and_then(|failure| failure.get("code"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Ok(DiagnosticsReportView {
            json,
            redaction_fields,
            last_failure_code,
            app_version: self.app_version.clone(),
            core_version: CORE_VERSION.to_owned(),
        })
    }

    /// Clear Core's bounded diagnostic journal for this instance.
    ///
    /// The app calls this when the device state is destroyed; the journal is
    /// diagnostic state, not user content. Core owns the retention policy.
    pub fn clear_diagnostics(&self) -> Result<(), MobileError> {
        self.core.clear_diagnostics().map_err(MobileError::from)
    }

    /// Write a redacted diagnostic bundle to `path` after user confirmation.
    pub fn export_diagnostics(&self, path: String) -> Result<DiagnosticsReportView, MobileError> {
        if path.is_empty() || path.len() > MAX_PATH_BYTES {
            return Err(MobileError::invalid_input());
        }
        let manifest = self
            .core
            .export_diagnostics(Path::new(&path))
            .map_err(MobileError::from)?;
        let json = std::fs::read_to_string(&path).map_err(|_| MobileError::internal())?;
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|_| MobileError::internal())?;
        let last_failure_code = value
            .get("last_failure")
            .and_then(|failure| failure.get("events"))
            .and_then(|events| events.as_array())
            .and_then(|events| events.last())
            .and_then(|event| event.get("code"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Ok(DiagnosticsReportView {
            json,
            redaction_fields: manifest.redactions,
            last_failure_code,
            app_version: self.app_version.clone(),
            core_version: CORE_VERSION.to_owned(),
        })
    }

    /// Forget the local device identity and pairing material.
    pub fn forget_device(&self) -> Result<(), MobileError> {
        self.stop_watch();
        self.core.forget_device().map_err(MobileError::from)
    }

    /// Forget the local identity only. Remote Cloud or Standalone grants stay
    /// intact, so the same device record can be recovered later.
    pub fn forget_local_device(&self) -> Result<(), MobileError> {
        self.stop_watch();
        self.core.forget_local_device().map_err(MobileError::from)
    }

    /// Revoke this device's grants while keeping the profile identity.
    pub fn revoke(&self) -> Result<(), MobileError> {
        self.run(COMMAND_TIMEOUT, self.core.revoke())?;
        self.stop_watch();
        Ok(())
    }

    /// Revoke this device's grant for exactly one Cloud Host and return the
    /// resulting server state. Other Host grants stay intact.
    pub fn revoke_cloud_host(&self, host_id: String) -> Result<String, MobileError> {
        validate_id(&host_id)?;
        let outcome = self.run(COMMAND_TIMEOUT, self.core.revoke_cloud(&host_id))?;
        Ok(match outcome.state {
            whalerein_core::api::CloudRevocationState::Revoked => "revoked",
            whalerein_core::api::CloudRevocationState::RevocationPending => "revocation_pending",
        }
        .to_owned())
    }

    // MARK: - Cloud account

    /// Begin the native iOS authorization-code flow. Core owns the attempt,
    /// PKCE material and Cloud state; only the hosted URL crosses this seam.
    pub fn cloud_auth_start(&self) -> Result<CloudAuthAttemptView, MobileError> {
        let attempt = self.run(QUERY_TIMEOUT, self.core.cloud_auth_start())?;
        Ok(CloudAuthAttemptView {
            url: attempt.url,
            attempt_id: attempt.attempt_id,
        })
    }

    /// Begin the Android authorization-code flow. The Cloud routes are the
    /// Android-scoped ones and the callback is the registered HTTPS App Link.
    pub fn cloud_auth_start_android(&self) -> Result<CloudAuthAttemptView, MobileError> {
        let attempt = self.run(QUERY_TIMEOUT, self.core.cloud_auth_start_android())?;
        Ok(CloudAuthAttemptView {
            url: attempt.url,
            attempt_id: attempt.attempt_id,
        })
    }

    /// Recover a lost exchange response with the pending attempt credential and
    /// the scoped `/me`. This never replays the authorization code.
    pub fn cloud_auth_recover(&self) -> Result<CloudAccountView, MobileError> {
        let account = self.run(QUERY_TIMEOUT, self.core.cloud_auth_recover())?;
        Ok(CloudAccountView::from(account))
    }

    /// Complete the exact pending attempt from the callback URL. Only this
    /// path establishes an account session for the native flow.
    pub fn cloud_auth_complete(
        &self,
        callback_url: String,
    ) -> Result<CloudAccountView, MobileError> {
        if callback_url.is_empty() || callback_url.len() > MAX_CALLBACK_BYTES {
            return Err(MobileError::invalid_input());
        }
        let account = self.run(QUERY_TIMEOUT, self.core.cloud_auth_complete(&callback_url))?;
        Ok(account.into())
    }

    /// Drop the pending attempt. A late callback can no longer complete it.
    pub fn cloud_auth_cancel(&self) -> Result<(), MobileError> {
        self.run(QUERY_TIMEOUT, self.core.cloud_auth_cancel())
    }

    /// Locally retained verified account plus pending remote sign-out state.
    pub fn cloud_status(&self) -> Result<CloudAccountStatusView, MobileError> {
        let account = self
            .core
            .account_status()
            .map_err(MobileError::from)?
            .map(CloudAccountView::from);
        let logout_pending = self
            .core
            .account_logout_pending()
            .map_err(MobileError::from)?;
        Ok(CloudAccountStatusView {
            account,
            logout_pending,
        })
    }

    /// Verify the current session against Cloud and refresh its expiry.
    pub fn cloud_refresh_status(&self) -> Result<CloudAccountView, MobileError> {
        let account = self.run(QUERY_TIMEOUT, self.core.refresh_account_status())?;
        Ok(account.into())
    }

    /// End this Cloud app session. `false` means the remote sign-out is still
    /// pending and will be retried on the next account request.
    pub fn cloud_sign_out(&self) -> Result<bool, MobileError> {
        let complete = self.run(COMMAND_TIMEOUT, self.core.logout())?;
        self.stop_watch();
        Ok(complete)
    }

    /// Account app sessions. Only stable session metadata is returned.
    pub fn cloud_account_sessions(&self) -> Result<Vec<CloudAccountSessionView>, MobileError> {
        let sessions = self.run(QUERY_TIMEOUT, self.core.cloud_account_sessions())?;
        Ok(sessions.into_iter().map(Into::into).collect())
    }

    /// Revoke one account app session and return its resulting state.
    pub fn cloud_revoke_session(&self, session_id: String) -> Result<String, MobileError> {
        validate_id(&session_id)?;
        let state = self.run(
            COMMAND_TIMEOUT,
            self.core.revoke_cloud_account_session(&session_id),
        )?;
        Ok(state.as_str().to_owned())
    }

    /// Revoke every account app session and return the resulting state.
    pub fn cloud_revoke_all_sessions(&self) -> Result<String, MobileError> {
        let state = self.run(
            COMMAND_TIMEOUT,
            self.core.revoke_all_cloud_account_sessions(),
        )?;
        Ok(state.as_str().to_owned())
    }

    /// The account's Cloud computer directory.
    pub fn cloud_hosts(&self) -> Result<Vec<CloudDirectoryHostView>, MobileError> {
        let hosts = self.run(QUERY_TIMEOUT, self.core.account_hosts())?;
        Ok(hosts.into_iter().map(Into::into).collect())
    }

    /// Connect one explicitly selected eligible Cloud computer. Core confirms
    /// the grant and persists recovery state before this returns.
    pub fn connect_cloud(&self, host_id: String) -> Result<CloudConnectResultView, MobileError> {
        validate_id(&host_id)?;
        let outcome = self.run(COMMAND_TIMEOUT, self.core.connect_cloud(&host_id))?;
        Ok(outcome.into())
    }
}

impl MobileCore {
    fn lock_pairing(&self) -> std::sync::MutexGuard<'_, PairingState> {
        self.pairing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_watch(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.watch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn next_pairing_generation(&self) -> u64 {
        self.pairing_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn next_watch_generation(&self) -> u64 {
        self.watch_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Queue a command event under its own session identity.
    fn publish_command(
        &self,
        session: &SessionKey,
        handle: whalerein_core::api::CommandHandle,
    ) -> CommandView {
        let view = CommandView::from(handle);
        self.events.push(ConversationEvent::CommandStatus {
            session: SessionKeyView::from(session),
            command: view.clone(),
        });
        view
    }

    fn clear_pairing(&self, generation: u64) {
        let mut state = self.lock_pairing();
        if state.generation == generation {
            state.running = false;
            state.decision = None;
        }
    }

    /// Block the calling thread for one Core request with a bounded timeout.
    ///
    /// Must not be called from inside a Tokio runtime; all FFI entry points are
    /// synchronous, so this is only reachable from the calling thread.
    fn run<F, T>(&self, timeout: Duration, future: F) -> Result<T, MobileError>
    where
        F: std::future::Future<Output = Result<T, CoreError>>,
    {
        match self
            .runtime
            .handle
            .block_on(async move { tokio::time::timeout(timeout, future).await })
        {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(MobileError::from(error)),
            Err(_) => Err(MobileError::timeout()),
        }
    }
}

impl Drop for MobileCore {
    fn drop(&mut self) {
        self.wait_attempts.cancel_all();
        self.attachment_transfers.cancel_all();
        self.watch_generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = self.lock_watch();
            state.active = false;
            if let Some(abort) = state.watch.take() {
                abort.abort();
            }
        }
        if let Some(abort) = self.monitor.lock().expect("monitor").take() {
            abort.abort();
        }
        self.pairing_generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = self.lock_pairing();
            state.running = false;
            state.decision = None;
            if let Some(abort) = state.abort.take() {
                abort.abort();
            }
        }
        self.runtime.shutdown();
    }
}

/// Push one authoritative Host transition on the app-level queue.
fn push_host_connection(events: &EventQueue, snapshot: &whalerein_core::api::ConnectionSnapshot) {
    events.push(ConversationEvent::HostConnection {
        host_id: snapshot.host_id.clone(),
        state: snapshot.state.as_str().to_owned(),
        code: snapshot.code.clone(),
        since_unix_ms: snapshot.since_unix_ms,
    });
}

fn connection_view(snapshot: whalerein_core::api::ConnectionSnapshot) -> HostConnectionView {
    HostConnectionView {
        host_id: snapshot.host_id,
        state: snapshot.state.as_str().to_owned(),
        code: snapshot.code,
        since_unix_ms: snapshot.since_unix_ms,
    }
}

fn retained_send_view(operation: &whalerein_core::api::SendOperation) -> RetainedOperationView {
    let status = operation
        .receipt
        .as_ref()
        .map(|receipt| operation_status_name(receipt.status))
        .unwrap_or("submitted");
    RetainedOperationView {
        operation_id: operation.operation_id.clone(),
        kind: "send".into(),
        host_id: Some(operation.target.host_id.clone()),
        session_id: Some(operation.target.session_id.clone()),
        status: status.to_owned(),
        receipt: operation.receipt.clone().map(Into::into),
        reply: None,
        content: None,
        read_error: None,
    }
}

fn validate_operation_id(value: &str) -> Result<(), MobileError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<(), MobileError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.chars().any(|character| character.is_control())
    {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn validate_text(text: &str) -> Result<(), MobileError> {
    if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn validate_fingerprint(fingerprint: &str) -> Result<(), MobileError> {
    if fingerprint.len() != MAX_FINGERPRINT_BYTES
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn decode_digest(digest_hex: &str) -> Result<Vec<u8>, MobileError> {
    let digest = hex::decode(digest_hex).map_err(|_| MobileError::invalid_input())?;
    if digest.len() != 32 {
        return Err(MobileError::invalid_input());
    }
    Ok(digest)
}

fn string_array(value: &serde_json::Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::BlockView;

    fn payloads(events: Vec<ConversationEventEnvelope>) -> Vec<ConversationEvent> {
        events.into_iter().map(|entry| entry.event).collect()
    }

    fn queue_with(capacity: usize) -> EventQueue {
        // The production capacity is fixed; tests fill it directly.
        let queue = EventQueue::new();
        {
            let mut state = queue.lock();
            for index in 0..capacity {
                state.events.push_back(ConversationEventEnvelope::unscoped(
                    ConversationEvent::HostConnection {
                        host_id: format!("h{index}"),
                        state: "connected".into(),
                        code: None,
                        since_unix_ms: 0,
                    },
                ));
            }
        }
        queue
    }

    #[test]
    fn poll_returns_promptly_when_empty() {
        let queue = EventQueue::new();
        let started = Instant::now();
        assert!(queue.poll(Duration::from_millis(50)).is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn poll_wakes_on_push() {
        let queue = Arc::new(EventQueue::new());
        let producer = queue.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            producer.push(ConversationEvent::WatchEnded);
        });
        let events = payloads(queue.poll(Duration::from_secs(5)));
        thread.join().unwrap();
        assert_eq!(events, vec![ConversationEvent::WatchEnded]);
    }

    #[test]
    fn overflow_drops_oldest_and_reports_the_count() {
        let queue = queue_with(EVENT_QUEUE_CAPACITY);
        for index in 0..3 {
            queue.push(ConversationEvent::HostConnection {
                host_id: format!("new{index}"),
                state: "connected".into(),
                code: None,
                since_unix_ms: 0,
            });
        }
        let events = payloads(queue.poll(Duration::ZERO));
        assert_eq!(
            events.first(),
            Some(&ConversationEvent::DroppedEvents { count: 3 })
        );
        assert_eq!(events.len(), EVENT_QUEUE_CAPACITY + 1);
        match events.get(1) {
            Some(ConversationEvent::HostConnection { host_id, .. }) => assert_eq!(host_id, "h3"),
            other => panic!("unexpected first retained event: {other:?}"),
        }
        match events.last() {
            Some(ConversationEvent::HostConnection { host_id, .. }) => assert_eq!(host_id, "new2"),
            other => panic!("unexpected last event: {other:?}"),
        }
        // The loss counter is reported once.
        assert!(queue.poll(Duration::ZERO).is_empty());
    }

    #[test]
    fn overflow_notice_keeps_the_watch_scope_and_is_discarded_on_replacement() {
        let queue = EventQueue::new();
        let first = scope(1, "host-1", "session-1").with_observation("first".into());
        let second = scope(2, "host-1", "session-1").with_observation("second".into());
        queue.begin_watch(&first);
        for _ in 0..=EVENT_QUEUE_CAPACITY {
            queue.push_watch(&first, preview("synthetic"));
        }
        let events = queue.poll(Duration::ZERO);
        assert_eq!(
            events.first(),
            Some(&ConversationEventEnvelope::scoped(
                first.clone(),
                ConversationEvent::DroppedEvents { count: 1 },
            )),
        );

        for _ in 0..=EVENT_QUEUE_CAPACITY {
            queue.push_watch(&first, preview("synthetic"));
        }
        queue.begin_watch(&second);
        assert!(queue.poll(Duration::ZERO).is_empty());
    }

    #[test]
    fn queue_never_exceeds_capacity() {
        let queue = queue_with(EVENT_QUEUE_CAPACITY);
        queue.push(ConversationEvent::WatchEnded);
        assert_eq!(queue.lock().events.len(), EVENT_QUEUE_CAPACITY);
    }

    fn scope(generation: u64, host_id: &str, session_id: &str) -> WatchEventScope {
        WatchEventScope::pending(generation, host_id.into(), session_id.into())
    }

    fn message(id: &str, text: &str) -> ConversationEvent {
        ConversationEvent::Message {
            origin: None,
            preview_prefix: None,
            message: MessageView {
                delivery_id: None,
                message_id: id.into(),
                role: "assistant".into(),
                blocks: vec![BlockView {
                    block_id: "text".into(),
                    kind: "text".into(),
                    text: text.into(),
                    tool_name: None,
                    tool_activity: None,
                    notice: None,
                    attachment: None,
                }],
                created_at_unix_ms: 1,
            },
        }
    }

    fn preview(text: &str) -> ConversationEvent {
        ConversationEvent::Preview {
            attempt: crate::types::AttemptRefView {
                live_epoch: "epoch".into(),
                attempt_id: "attempt-old".into(),
            },
            block_id: "block-old".into(),
            kind: "text".into(),
            text: text.into(),
            replace: false,
            incomplete: true,
        }
    }

    #[test]
    fn replacement_drops_queued_old_watch_output_but_keeps_independent_events() {
        let queue = EventQueue::new();
        let old = scope(1, "host-1", "session-1");
        queue.begin_watch(&old);
        queue.push_watch(&old, message("old-message", "old committed text"));
        queue.push_watch(&old, preview("old preview text"));
        queue.push(ConversationEvent::PairingFinished {
            host_id: Some("host-1".into()),
            code: None,
        });
        queue.push(ConversationEvent::CommandStatus {
            session: SessionKeyView::from(&SessionKey {
                host_id: "host-1".into(),
                session_id: "session-1".into(),
            }),
            command: CommandView {
                request_id: "request-1".into(),
                command_id: "command-1".into(),
                status: "succeeded".into(),
                message_id: None,
                delivery_id: None,
            },
        });
        queue.push(ConversationEvent::HostConnection {
            host_id: "host-1".into(),
            state: "connected".into(),
            code: None,
            since_unix_ms: 1,
        });
        queue.end_watch(&old);

        let new = scope(2, "host-1", "session-2");
        queue.begin_watch(&new);
        // A late callback or terminal from the replaced watch must not be
        // accepted and must not clear the replacement's scope.
        queue.end_watch(&old);
        queue.push_watch(&old, message("late-old", "late old text"));
        queue.push_watch(&new, message("new-message", "new committed text"));
        queue.push_watch(&new, preview("new preview text"));

        let batch = queue.poll(Duration::ZERO);
        assert_eq!(batch.len(), 5);
        assert!(batch[..3].iter().all(|entry| entry.scope.is_none()));
        assert!(batch[3..]
            .iter()
            .all(|entry| entry.scope.as_ref() == Some(&new)));
        let events = payloads(batch);
        assert!(matches!(
            events.first(),
            Some(ConversationEvent::PairingFinished { .. })
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            ConversationEvent::CommandStatus { command, .. } if command.command_id == "command-1"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ConversationEvent::HostConnection { host_id, .. } if host_id == "host-1"
        )));
        assert!(events.iter().any(
            |event| matches!(event, ConversationEvent::Message { message, .. } if message.message_id == "new-message")
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            ConversationEvent::Preview { text, .. } if text == "new preview text"
        )));
        assert!(
            !events.iter().any(|event| matches!(
                event,
                ConversationEvent::Message { message, .. } if message.message_id == "old-message"
            )),
            "queued old-watch message leaked into the new watch: {events:?}"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                ConversationEvent::Preview { text, .. } if text == "old preview text"
            )),
            "queued old-watch preview leaked into the new watch: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ConversationEvent::WatchEnded)),
            "the replaced watch's pending watchEnded survived"
        );

        // The new watch keeps working and ends normally.
        queue.end_watch(&new);
        assert_eq!(
            queue.poll(Duration::ZERO),
            vec![ConversationEventEnvelope::scoped(
                new.clone(),
                ConversationEvent::WatchEnded
            )]
        );
        // Nothing from the ended watch is accepted afterwards.
        queue.push_watch(&new, message("after-end", "after end"));
        assert!(queue.poll(Duration::ZERO).is_empty());
    }

    #[test]
    fn queued_events_retain_producer_session_and_observation_identity() {
        let queue = EventQueue::new();
        let pending = scope(7, "host-1", "session-1");
        queue.begin_watch(&pending);
        queue.push_watch(
            &pending,
            ConversationEvent::HostConnection {
                host_id: "host-1".into(),
                state: "connected".into(),
                code: None,
                since_unix_ms: 0,
            },
        );
        let observed = pending.with_observation("obs-42".into());
        assert!(queue.attach_watch(&observed));
        queue.push_watch(&observed, message("m-1", "producer scoped text"));
        let foreign = pending.with_observation("foreign-obs".into());
        queue.push_watch(&foreign, preview("foreign observation"));
        // A different Core observation identity cannot be relabelled as the
        // active watch; the binding generation is the replacement fence.
        let stale = scope(8, "host-1", "session-1").with_observation("obs-old".into());
        queue.push_watch(&stale, preview("relabelled stale output"));
        let events = queue.poll(Duration::ZERO);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].scope.as_ref(), Some(&pending));
        assert_eq!(events[1].scope.as_ref(), Some(&observed));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.event, ConversationEvent::Preview { .. })),
            "an old observation generation leaked: {events:?}"
        );
    }

    #[test]
    fn poll_spanning_replacement_keeps_the_same_session_reopen_producer() {
        let queue = Arc::new(EventQueue::new());
        let first = scope(1, "host-1", "session-1").with_observation("first-observation".into());
        let reopened =
            scope(2, "host-1", "session-1").with_observation("reopened-observation".into());
        queue.begin_watch(&first);
        let waiting = Arc::new(std::sync::Barrier::new(2));
        let poller = {
            let queue = Arc::clone(&queue);
            let waiting = Arc::clone(&waiting);
            std::thread::spawn(move || {
                queue.poll_waiting(Duration::from_secs(5), || {
                    waiting.wait();
                })
            })
        };
        waiting.wait();

        queue.begin_watch(&reopened);
        queue.end_watch(&first);
        queue.push_watch(&first, message("late", "synthetic old message"));
        let baseline = ConversationEvent::HistorySnapshot {
            page: HistoryPageView {
                source: "online".into(),
                projection: "projection".into(),
                complete: true,
                messages: Vec::new(),
                activities: Vec::new(),
                continuation: None,
            },
            continuity: HistoryContinuityView::Continuous,
        };
        queue.push_watch(&reopened, baseline.clone());

        let batch = poller.join().expect("read from blocked poll");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].scope.as_ref(), Some(&reopened));
        assert_eq!(batch[0].event, baseline);

        queue.end_watch(&reopened);
        assert_eq!(
            queue.poll(Duration::ZERO),
            vec![ConversationEventEnvelope::scoped(
                reopened,
                ConversationEvent::WatchEnded
            )],
        );
    }

    #[test]
    fn identical_session_ids_on_two_hosts_keep_their_watch_scopes_distinct() {
        let queue = EventQueue::new();
        let host_a = scope(1, "host-a", "session-1");
        queue.begin_watch(&host_a);
        queue.push_watch(&host_a, message("message-a", "content from host a"));

        // Same session id, authorized second Host: replacement drops host A's
        // queued message even though the session id is identical.
        let host_b = scope(2, "host-b", "session-1");
        queue.begin_watch(&host_b);
        queue.push_watch(&host_a, message("late-a", "late content from host a"));
        // Even a scope with the active generation but another Host is rejected.
        let forged = scope(2, "host-a", "session-1");
        queue.push_watch(&forged, message("forged", "forged content"));
        queue.push_watch(&host_b, message("message-b", "content from host b"));

        let events = payloads(queue.poll(Duration::ZERO));
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    ConversationEvent::Message { message, .. } => Some(message.message_id.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["message-b"]
        );
    }

    #[test]
    fn validation_rejects_empty_oversized_and_control_values() {
        assert!(validate_id("").is_err());
        assert!(validate_id(&"a".repeat(MAX_ID_BYTES + 1)).is_err());
        assert!(validate_id("a\u{0}b").is_err());
        assert!(validate_id("session-1").is_ok());
        assert!(validate_text(" ").is_err());
        assert!(validate_text(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(validate_text("hello").is_ok());
        assert!(validate_fingerprint("00").is_err());
        assert!(validate_fingerprint(&"ab".repeat(32)).is_ok());
        assert!(validate_fingerprint(&"zz".repeat(32)).is_err());
        assert!(decode_digest("00").is_err());
        assert!(decode_digest(&"ab".repeat(32)).is_ok());
    }

    #[test]
    fn delivery_choices_map_to_core_modes() {
        assert_eq!(
            DeliveryChoice::Steer.to_core(),
            whalerein_core::api::DeliveryMode::Steer
        );
        assert_eq!(
            DeliveryChoice::Queue.to_core(),
            whalerein_core::api::DeliveryMode::EnqueueAfterRun
        );
        assert_eq!(
            DeliveryChoice::Send.to_core(),
            whalerein_core::api::DeliveryMode::StartIfIdle
        );
    }

    #[test]
    fn cloud_records_map_typed_state_without_exposing_credentials() {
        use whalerein_core::api::{
            AccountHost, AccountIdentity, AccountSessionInfo, AccountSessionState,
            AccountWorkspace, PairingOutcome, SignedInAccount,
        };
        let account = CloudAccountView::from(SignedInAccount {
            account: AccountIdentity {
                id: "acct-1".into(),
                email: "user@example.test".into(),
                issuer: "https://login.whalerein.com/oidc".into(),
                subject: "subject-1".into(),
            },
            workspace: AccountWorkspace { id: "ws-1".into() },
        });
        assert_eq!(account.account_id, "acct-1");
        assert_eq!(account.email, "user@example.test");
        assert_eq!(account.issuer, "https://login.whalerein.com/oidc");
        assert_eq!(account.subject, "subject-1");
        assert_eq!(account.workspace_id, "ws-1");
        let status = CloudAccountStatusView {
            account: Some(account.clone()),
            logout_pending: true,
        };
        assert!(status.logout_pending);
        assert_eq!(status.account.as_ref().unwrap().workspace_id, "ws-1");

        let host = CloudDirectoryHostView::from(AccountHost {
            host_id: "host-1".into(),
            display_name: "Studio".into(),
            enrolled: true,
            online: false,
            presence: "away".into(),
            observation: "observed".into(),
            enrollment_state: "enrolled".into(),
            pairing_eligible: false,
            device_access: "granted".into(),
        });
        assert_eq!(host.host_id, "host-1");
        assert_eq!(host.display_name, "Studio");
        assert!(host.enrolled && !host.online);
        assert_eq!(host.enrollment_state, "enrolled");

        let session = CloudAccountSessionView::from(AccountSessionInfo {
            session_id: "sess-1".into(),
            instance_id: "ios".into(),
            expires_at: 1_780_000_000,
            state: AccountSessionState::Active,
        });
        assert_eq!(session.session_id, "sess-1");
        assert_eq!(session.instance_id, "ios");
        assert_eq!(session.state, "active");

        let connect = CloudConnectResultView::from(PairingOutcome {
            operation_id: Some("op-1".into()),
            host_id: "host-1".into(),
            device_id: "device-1".into(),
            fingerprint: "ab".repeat(32),
            access_url: "https://host-1.access.example/secret-token".into(),
        });
        assert_eq!(connect.operation_id.as_deref(), Some("op-1"));
        assert_eq!(connect.host_id, "host-1");
        assert_eq!(connect.device_id, "device-1");
        assert_eq!(connect.fingerprint, "ab".repeat(32));
        let rendered = format!("{account:?} {host:?} {session:?} {connect:?} {status:?}");
        for forbidden in [
            "credential",
            "revocation",
            "token",
            "secret-token",
            "code_verifier",
            "codeVerifier",
            "access_url",
            "accessUrl",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} leaked into {rendered}"
            );
        }
    }

    #[test]
    fn cloud_auth_attempt_exposes_only_the_url_and_attempt_identity() {
        let attempt = CloudAuthAttemptView {
            url: "https://login.whalerein.com/oidc/auth?state=opaque".into(),
            attempt_id: "ab".repeat(16),
        };
        assert_eq!(attempt.attempt_id.len(), 32);
        assert!(attempt.url.starts_with("https://"));
        let rendered = format!("{attempt:?}");
        assert!(!rendered.contains("credential"));
        assert!(!rendered.contains("verifier"));
    }

    #[test]
    fn revision_has_provenance_format() {
        let revision = core_revision();
        assert!(!revision.is_empty());
        assert!(revision.starts_with("git:"), "{revision}");
        assert!(revision.contains(" core:"), "{revision}");
        assert!(revision.ends_with(CORE_VERSION), "{revision}");
        assert_eq!(revision.matches(' ').count(), 1, "{revision}");
    }
}

#[cfg(test)]
mod platform_trust_tests {
    use super::*;

    #[test]
    fn platform_trust_input_is_bounded_and_nonempty() {
        assert!(platform_trust_pem(Vec::new()).is_err());
        assert!(platform_trust_pem(vec![b'x'; MAX_CA_PEM_BYTES + 1]).is_err());
        let pem = b"-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n".to_vec();
        assert_eq!(platform_trust_pem(pem.clone()).unwrap(), pem);
    }
}
