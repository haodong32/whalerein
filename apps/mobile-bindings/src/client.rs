//! The `MobileCore` facade exported to Swift.
//!
//! All Swift-facing methods are synchronous. Async Core work runs on a
//! dedicated multi-thread Tokio runtime owned by this object; request/response
//! calls block the calling thread with a bounded timeout. Background pairing
//! and watching publish ordered events into a bounded queue consumed by
//! `pollEvents`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::task::AbortHandle;
use whalerein_core::api::{Core, CoreOptions, PairingDecision, PairingPrompt};
use whalerein_core::conversation::SessionKey;
use whalerein_core::CoreError;

use crate::error::{MobileError, CODE_CANCELLED};
use crate::secret::{MemorySecretStore, SecretStore, SecretStoreAdapter};
use crate::types::{
    convert_update, message_view, ApprovalView, CommandView, ConversationEvent, DeliveryChoice,
    DiagnosticsReportView, HostSummary, MessageView, PairingPromptView, SessionCapabilitiesView,
    SessionSummaryView,
};

/// Request/response budget for listing and local operations.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Request/response budget for operations Core may wait on until its TTL.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `pairingStart` waits for Core to produce a prompt.
const PAIRING_PROMPT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a pending pairing prompt waits for a decision before rejecting.
const PAIRING_DECISION_TIMEOUT: Duration = Duration::from_secs(120);
/// Upper bound for one `pollEvents` call; callers poll repeatedly.
const MAX_POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// Connection state is sampled while a watch is active.
const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(500);

const EVENT_QUEUE_CAPACITY: usize = 512;
const MAX_ID_BYTES: usize = 512;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_INVITATION_BYTES: usize = 16 * 1024;
const MAX_FINGERPRINT_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const MAX_APP_VERSION_BYTES: usize = 256;

/// Build-time Core package version, recorded by `build.rs`.
pub(crate) const CORE_VERSION: &str = env!("WHALERIN_CORE_VERSION");

/// Unified Core version plus git provenance for this binding build.
#[uniffi::export]
pub fn core_revision() -> String {
    env!("WHALERIN_MOBILE_REVISION").to_owned()
}

/// Bounded, drop-oldest event queue with an explicit loss counter.
///
/// A burst larger than the capacity keeps the newest events and reports how
/// many older ones were discarded on the next poll. Memory never grows past
/// `EVENT_QUEUE_CAPACITY`.
struct EventQueue {
    shared: Mutex<EventQueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct EventQueueState {
    events: std::collections::VecDeque<ConversationEvent>,
    dropped: u64,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            shared: Mutex::new(EventQueueState::default()),
            ready: Condvar::new(),
        }
    }

    fn push(&self, event: ConversationEvent) {
        let mut state = self.lock();
        if state.events.len() >= EVENT_QUEUE_CAPACITY {
            state.events.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
        state.events.push_back(event);
        drop(state);
        self.ready.notify_one();
    }

    fn poll(&self, timeout: Duration) -> Vec<ConversationEvent> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if !state.events.is_empty() || state.dropped > 0 {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, _) = self
                .ready
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
        let mut events = Vec::with_capacity(state.events.len() + 1);
        if state.dropped > 0 {
            events.push(ConversationEvent::DroppedEvents {
                count: state.dropped,
            });
            state.dropped = 0;
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

/// The currently active watch and its connection poller.
#[derive(Default)]
struct WatchState {
    generation: u64,
    active: bool,
    watch: Option<AbortHandle>,
    poller: Option<AbortHandle>,
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
}

#[uniffi::export]
impl MobileCore {
    /// Create a Core instance backed by `secretStore`.
    ///
    /// `dataDir` is the app's private directory. The Keychain implementation is
    /// supplied by Swift; secrets never enter this module.
    #[uniffi::constructor]
    pub fn new(
        data_dir: String,
        secret_store: Arc<dyn SecretStore>,
        app_version: String,
    ) -> Result<Arc<Self>, MobileError> {
        if data_dir.is_empty() || data_dir.len() > MAX_PATH_BYTES {
            return Err(MobileError::invalid_input());
        }
        if app_version.len() > MAX_APP_VERSION_BYTES {
            return Err(MobileError::invalid_input());
        }
        let runtime = RuntimeHost::start()?;
        let options = CoreOptions {
            data_dir: PathBuf::from(&data_dir),
            secrets: Arc::new(SecretStoreAdapter::new(secret_store)),
        };
        let core = Core::new(options).map_err(MobileError::from)?;
        Ok(Arc::new(Self {
            core,
            runtime,
            events: Arc::new(EventQueue::new()),
            app_version,
            pairing: Arc::new(Mutex::new(PairingState::default())),
            pairing_generation: Arc::new(AtomicU64::new(0)),
            watch: Arc::new(Mutex::new(WatchState::default())),
            watch_generation: Arc::new(AtomicU64::new(0)),
        }))
    }

    /// Create a Core instance with a process-local secret store.
    ///
    /// Intended for Simulator and Rust tests: secrets are lost on exit and are
    /// never written to disk. Production uses `new` with the Keychain store.
    #[uniffi::constructor]
    pub fn new_with_memory_secret_store(
        data_dir: String,
        app_version: String,
    ) -> Result<Arc<Self>, MobileError> {
        Self::new(data_dir, MemorySecretStore::new(), app_version)
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
                let result = core.pair(&invitation, decide).await;
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
            Err(RecvTimeoutError::Timeout) => Err(MobileError::timeout()),
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
        let core = self.scoped_host(&host_id)?;
        let list = self.run(QUERY_TIMEOUT, core.sessions(&host_id))?;
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
        let core = self.scoped_host(&host_id)?;
        let list = self.run(QUERY_TIMEOUT, core.sessions(&host_id))?;
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

    /// Current in-memory connection state: `disconnected`, `pairing`,
    /// `connecting`, `handshaking`, `syncing`, `connected`, `reconnecting`,
    /// `offline`, `revoked` or `failed`.
    pub fn connection_state(&self, host_id: String) -> String {
        self.core.get_connection_state(&host_id).as_str().to_owned()
    }

    // -----------------------------------------------------------------
    // Watch and events
    // -----------------------------------------------------------------

    /// Start observing one Host-qualified session.
    ///
    /// Replaces an existing watch; updates from a replaced watch are dropped.
    /// Events are delivered through `pollEvents`.
    pub fn start_watch(&self, host_id: String, session_id: String) -> Result<(), MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let key = SessionKey {
            host_id: host_id.clone(),
            session_id: session_id.clone(),
        };
        let previous = {
            let mut state = self.lock_watch();
            let generation = self.next_watch_generation();
            state.generation = generation;
            state.active = true;
            let previous = (state.watch.take(), state.poller.take());
            (generation, previous)
        };
        let (generation, previous) = previous;
        if let Some(abort) = previous.0 {
            abort.abort();
        }
        if let Some(abort) = previous.1 {
            abort.abort();
        }

        self.events.push(ConversationEvent::Connection {
            host_id: host_id.clone(),
            status: self.core.get_connection_state(&host_id).as_str().to_owned(),
        });

        let core = self.core.clone();
        let events = self.events.clone();
        let generations = self.watch_generation.clone();
        let watch_state = self.watch.clone();
        let task = self.runtime.handle.spawn(async move {
            let mut seen_approvals = None;
            let result = core
                .watch_session(&key, None, |update| {
                    if generations.load(Ordering::SeqCst) != generation {
                        return false;
                    }
                    for event in convert_update(update, &mut seen_approvals) {
                        events.push(event);
                    }
                    true
                })
                .await;
            if generations.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Err(error) = result {
                let error = MobileError::from(error);
                events.push(ConversationEvent::Error {
                    code: error.code().to_owned(),
                    message: error.message().to_owned(),
                });
            }
            events.push(ConversationEvent::WatchEnded);
            let mut state = watch_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.generation == generation {
                state.active = false;
            }
        });

        let core = self.core.clone();
        let events = self.events.clone();
        let generations = self.watch_generation.clone();
        let poller = self.runtime.handle.spawn(async move {
            let mut last = core.get_connection_state(&host_id).as_str().to_owned();
            loop {
                tokio::time::sleep(CONNECTION_POLL_INTERVAL).await;
                if generations.load(Ordering::SeqCst) != generation {
                    return;
                }
                let next = core.get_connection_state(&host_id).as_str().to_owned();
                if next != last {
                    last = next.clone();
                    events.push(ConversationEvent::Connection {
                        host_id: host_id.clone(),
                        status: next,
                    });
                }
            }
        });

        let mut state = self.lock_watch();
        if state.generation == generation {
            state.watch = Some(task.abort_handle());
            state.poller = Some(poller.abort_handle());
        }
        Ok(())
    }

    /// Stop observing and emit `watchEnded`.
    pub fn stop_watch(&self) {
        let abort = {
            let mut state = self.lock_watch();
            if !state.active {
                return;
            }
            self.next_watch_generation();
            state.active = false;
            (state.watch.take(), state.poller.take())
        };
        if let Some(abort) = abort.0 {
            abort.abort();
        }
        if let Some(abort) = abort.1 {
            abort.abort();
        }
        self.events.push(ConversationEvent::WatchEnded);
    }

    /// Drain queued events, waiting up to `timeoutMs` when the queue is empty.
    ///
    /// Returns an empty array on timeout. The timeout is capped at 30 seconds.
    pub fn poll_events(&self, timeout_ms: u64) -> Vec<ConversationEvent> {
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
        let core = self.scoped_host(&host_id)?;
        let handle = self.run(
            COMMAND_TIMEOUT,
            core.send_message_with_delivery(&session_id, &text, delivery.to_core()),
        )?;
        Ok(self.publish_command(handle))
    }

    /// Interrupt the running turn for a session.
    pub fn stop(&self, host_id: String, session_id: String) -> Result<CommandView, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let core = self.scoped_host(&host_id)?;
        let handle = self.run(COMMAND_TIMEOUT, core.interrupt(&session_id))?;
        Ok(self.publish_command(handle))
    }

    /// List pending approvals for one session.
    pub fn approvals(
        &self,
        host_id: String,
        session_id: String,
    ) -> Result<Vec<ApprovalView>, MobileError> {
        validate_id(&host_id)?;
        validate_id(&session_id)?;
        let core = self.scoped_host(&host_id)?;
        let approvals = self.run(QUERY_TIMEOUT, core.list_approvals(&session_id))?;
        Ok(approvals.into_iter().map(ApprovalView::from).collect())
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
        let core = self.scoped_host(&host_id)?;
        let handle = if allow {
            self.run(
                COMMAND_TIMEOUT,
                core.approve(&approval_id, &digest, version),
            )?
        } else {
            self.run(COMMAND_TIMEOUT, core.reject(&approval_id, &digest, version))?
        };
        Ok(CommandView::from(handle))
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

    /// Revoke this device's grants while keeping the profile identity.
    pub fn revoke(&self) -> Result<(), MobileError> {
        self.run(COMMAND_TIMEOUT, self.core.revoke())?;
        self.stop_watch();
        Ok(())
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

    /// Queue a `commandStatus` event for observers of the same watch.
    fn publish_command(&self, handle: whalerein_core::api::CommandHandle) -> CommandView {
        let view = CommandView::from(handle);
        self.events.push(ConversationEvent::CommandStatus {
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

    fn scoped_host(&self, host_id: &str) -> Result<Core, MobileError> {
        self.core
            .with_session_host(host_id)
            .map_err(MobileError::from)
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
        self.watch_generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = self.lock_watch();
            state.active = false;
            if let Some(abort) = state.watch.take() {
                abort.abort();
            }
            if let Some(abort) = state.poller.take() {
                abort.abort();
            }
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

    fn queue_with(capacity: usize) -> EventQueue {
        // The production capacity is fixed; tests fill it directly.
        let queue = EventQueue::new();
        {
            let mut state = queue.lock();
            for index in 0..capacity {
                state.events.push_back(ConversationEvent::Connection {
                    host_id: format!("h{index}"),
                    status: "connected".into(),
                });
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
        let events = queue.poll(Duration::from_secs(5));
        thread.join().unwrap();
        assert_eq!(events, vec![ConversationEvent::WatchEnded]);
    }

    #[test]
    fn overflow_drops_oldest_and_reports_the_count() {
        let queue = queue_with(EVENT_QUEUE_CAPACITY);
        for index in 0..3 {
            queue.push(ConversationEvent::Connection {
                host_id: format!("new{index}"),
                status: "connected".into(),
            });
        }
        let events = queue.poll(Duration::ZERO);
        assert_eq!(
            events.first(),
            Some(&ConversationEvent::DroppedEvents { count: 3 })
        );
        assert_eq!(events.len(), EVENT_QUEUE_CAPACITY + 1);
        match events.get(1) {
            Some(ConversationEvent::Connection { host_id, .. }) => assert_eq!(host_id, "h3"),
            other => panic!("unexpected first retained event: {other:?}"),
        }
        match events.last() {
            Some(ConversationEvent::Connection { host_id, .. }) => assert_eq!(host_id, "new2"),
            other => panic!("unexpected last event: {other:?}"),
        }
        // The loss counter is reported once.
        assert!(queue.poll(Duration::ZERO).is_empty());
    }

    #[test]
    fn queue_never_exceeds_capacity() {
        let queue = queue_with(EVENT_QUEUE_CAPACITY);
        queue.push(ConversationEvent::WatchEnded);
        assert_eq!(queue.lock().events.len(), EVENT_QUEUE_CAPACITY);
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
    fn revision_has_provenance_format() {
        let revision = core_revision();
        assert!(!revision.is_empty());
        assert!(revision.starts_with("git:"), "{revision}");
        assert!(revision.contains(" core:"), "{revision}");
        assert!(revision.ends_with(CORE_VERSION), "{revision}");
        assert_eq!(revision.matches(' ').count(), 1, "{revision}");
    }
}
