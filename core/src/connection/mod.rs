//! One device connection per Host.
//!
//! The device subscribes to exactly one topic per Host
//! (`whalerein/v1/h/<host>/d/<device>/event`); every session's frames arrive
//! there. A connection therefore belongs to the Host, not to a session:
//! sessions attach to it, read the frames tagged with their session id, and
//! detach without disturbing it.
//!
//! ```text
//! Connecting -> Syncing -> Connected
//! Connected -> Reconnecting (broker dropped) -> Connecting ...
//! any -> Offline { code } (terminal: TLS, credentials, revocation)
//! ```
//!
//! `Connected` requires the broker to acknowledge the control and live
//! subscriptions; reaching HTTPS alone never counts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use whalerein_proto::EventEnvelope;

use crate::access::{DeliveryClass, DeviceMqttClient, MqttConnectionEvent, MqttSettings};
use crate::api::{Cancellation, CoreError, CoreResult};
use crate::diagnostics::journal::Diagnostics;
use crate::runtime::{ConnectionState, Runtime};

/// Bounded bytes one observer may queue for live previews.
const LIVE_QUEUE_CAP: usize = 256 * 1024;

/// The authoritative connectivity of one Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSnapshot {
    pub host_id: String,
    pub state: ConnectionState,
    /// Present only for a terminal state so a caller can report the real
    /// reason instead of guessing "offline".
    pub code: Option<String>,
    pub since_unix_ms: u64,
}

impl ConnectionSnapshot {
    fn new(host_id: &str, state: ConnectionState) -> Self {
        Self {
            host_id: host_id.to_owned(),
            state,
            code: None,
            since_unix_ms: epoch_ms(),
        }
    }

    fn terminal(host_id: &str, code: &str) -> Self {
        Self {
            host_id: host_id.to_owned(),
            state: ConnectionState::Offline,
            code: Some(code.to_owned()),
            since_unix_ms: epoch_ms(),
        }
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Inputs a connection needs. Owned data only: a connection never holds the
/// `Core` that owns the registry, so the two cannot keep each other alive.
pub(crate) struct ConnectionInput {
    pub host_id: String,
    pub device_id: String,
    pub settings: MqttSettings,
    pub token: String,
    pub cloud_session: Option<String>,
    pub runtime: Arc<Runtime>,
    pub diagnostics: Arc<Diagnostics>,
    pub queue_bytes: usize,
}

/// One attached observer. The transport pushes frames into `control` and
/// `live`; dropping the subscription detaches it without touching the
/// connection.
pub(crate) struct Subscription {
    control: mpsc::Receiver<(DeliveryClass, EventEnvelope)>,
    live: mpsc::Receiver<(DeliveryClass, EventEnvelope)>,
    interrupted: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    live_overflow: Arc<AtomicBool>,
    error: Arc<Mutex<Option<CoreError>>>,
    queued: Arc<AtomicUsize>,
    live_queued: Arc<AtomicUsize>,
    guard: SubscriptionGuard,
}

/// An attached observer split into its receiver halves plus a detach guard.
pub(crate) struct SubscriptionParts {
    pub control: mpsc::Receiver<(DeliveryClass, EventEnvelope)>,
    pub live: mpsc::Receiver<(DeliveryClass, EventEnvelope)>,
    pub interrupted: Arc<AtomicBool>,
    pub overflow: Arc<AtomicBool>,
    pub live_overflow: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<CoreError>>>,
    pub queued: Arc<AtomicUsize>,
    pub live_queued: Arc<AtomicUsize>,
}

impl Subscription {
    /// Split the observer from its channels. The returned guard detaches the
    /// observer from the connection when dropped; the connection keeps running.
    pub(crate) fn into_parts(self) -> (SubscriptionParts, SubscriptionGuard) {
        let Subscription {
            control,
            live,
            interrupted,
            overflow,
            live_overflow,
            error,
            queued,
            live_queued,
            guard,
        } = self;
        (
            SubscriptionParts {
                control,
                live,
                interrupted,
                overflow,
                live_overflow,
                error,
                queued,
                live_queued,
            },
            guard,
        )
    }
}

pub(crate) struct SubscriptionGuard {
    subscriber_id: u64,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.subscribers
            .lock()
            .expect("connection subscribers")
            .retain(|entry| entry.id != self.subscriber_id);
    }
}

struct Subscriber {
    id: u64,
    control: mpsc::Sender<(DeliveryClass, EventEnvelope)>,
    live: mpsc::Sender<(DeliveryClass, EventEnvelope)>,
    interrupted: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    live_overflow: Arc<AtomicBool>,
    error: Arc<Mutex<Option<CoreError>>>,
    queued: Arc<AtomicUsize>,
    live_queued: Arc<AtomicUsize>,
    control_budget: usize,
    live_budget: usize,
}

/// The live device connection. Cheap to clone and shared by every holder.
pub(crate) struct HostConnection {
    host_id: String,
    runtime: Arc<Runtime>,
    snapshot: Arc<Mutex<ConnectionSnapshot>>,
    changes: watch::Sender<ConnectionSnapshot>,
    global: broadcast::Sender<ConnectionSnapshot>,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    stop: Arc<AtomicBool>,
    task: Mutex<Option<JoinHandle<()>>>,
    running: Arc<AtomicBool>,
    queue_bytes: usize,
}

impl HostConnection {
    fn start(input: ConnectionInput, global: broadcast::Sender<ConnectionSnapshot>) -> Arc<Self> {
        let initial = ConnectionSnapshot::new(&input.host_id, ConnectionState::Connecting);
        let (changes, _) = watch::channel(initial.clone());
        let connection = Arc::new(Self {
            host_id: input.host_id.clone(),
            runtime: input.runtime.clone(),
            snapshot: Arc::new(Mutex::new(initial)),
            changes,
            global,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            stop: Arc::new(AtomicBool::new(false)),
            task: Mutex::new(None),
            running: Arc::new(AtomicBool::new(true)),
            queue_bytes: input.queue_bytes,
        });
        let runner = connection.clone();
        let task = tokio::spawn(async move { runner.run(input).await });
        *connection.task.lock().expect("connection task") = Some(task);
        connection
    }

    pub(crate) fn snapshot(&self) -> ConnectionSnapshot {
        self.snapshot.lock().expect("connection snapshot").clone()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn update(&self, next: ConnectionSnapshot) {
        *self.snapshot.lock().expect("connection snapshot") = next.clone();
        let _ = self.changes.send(next.clone());
        let _ = self.global.send(next);
    }

    fn set_state(&self, state: ConnectionState) {
        let _ = self.runtime.set_state(&self.host_id, state);
        self.update(ConnectionSnapshot::new(&self.host_id, state));
    }

    /// Wait for the transport to become usable. A terminal state fails with
    /// the recorded reason instead of pretending a transient outage.
    pub(crate) async fn wait_ready(&self, cancellation: &Cancellation) -> CoreResult<()> {
        let mut changes = self.changes.subscribe();
        loop {
            let snapshot = changes.borrow_and_update().clone();
            match snapshot.state {
                ConnectionState::Connected => return Ok(()),
                ConnectionState::Offline => {
                    return Err(terminal_error(snapshot.code.as_deref()));
                }
                _ => {}
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                changed = changes.changed() => {
                    if changed.is_err() {
                        return Err(terminal_error(None));
                    }
                }
            }
        }
    }

    /// Attach one observer to the shared transport.
    pub(crate) fn subscribe(&self) -> Subscription {
        static NEXT_SUBSCRIBER: AtomicUsize = AtomicUsize::new(1);
        let id = NEXT_SUBSCRIBER.fetch_add(1, Ordering::SeqCst) as u64;
        let control_budget = self.queue_bytes;
        let live_budget = control_budget.min(LIVE_QUEUE_CAP);
        let (control_tx, control) = mpsc::channel(1024);
        let (live_tx, live) = mpsc::channel(1024);
        let interrupted = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let live_overflow = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let queued = Arc::new(AtomicUsize::new(0));
        let live_queued = Arc::new(AtomicUsize::new(0));
        self.subscribers
            .lock()
            .expect("connection subscribers")
            .push(Subscriber {
                id,
                control: control_tx,
                live: live_tx,
                interrupted: interrupted.clone(),
                overflow: overflow.clone(),
                live_overflow: live_overflow.clone(),
                error: error.clone(),
                queued: queued.clone(),
                live_queued: live_queued.clone(),
                control_budget,
                live_budget,
            });
        Subscription {
            control,
            live,
            interrupted,
            overflow,
            live_overflow,
            error,
            queued,
            live_queued,
            guard: SubscriptionGuard {
                subscriber_id: id,
                subscribers: self.subscribers.clone(),
            },
        }
    }

    fn interrupt_subscribers(&self, detail: &'static str) {
        for subscriber in self
            .subscribers
            .lock()
            .expect("connection subscribers")
            .iter()
        {
            subscriber.interrupted.store(true, Ordering::SeqCst);
            // An interruption is transient by construction: the transport is
            // retrying, and a terminal failure is reported by the next attach.
            *subscriber.error.lock().expect("subscriber error") =
                Some(CoreError::Unavailable(detail.into()));
        }
    }

    /// Deliver one frame to every observer. A full queue marks only that
    /// observer; the transport keeps running.
    fn fan_out(&self, class: DeliveryClass, envelope: &EventEnvelope) {
        let bytes = envelope.encoded_len();
        for subscriber in self
            .subscribers
            .lock()
            .expect("connection subscribers")
            .iter()
        {
            let (sender, counter, budget, overflow) = match class {
                DeliveryClass::Control => (
                    &subscriber.control,
                    &subscriber.queued,
                    subscriber.control_budget,
                    &subscriber.overflow,
                ),
                DeliveryClass::Live => (
                    &subscriber.live,
                    &subscriber.live_queued,
                    subscriber.live_budget,
                    &subscriber.live_overflow,
                ),
            };
            if counter.load(Ordering::SeqCst) + bytes > budget {
                overflow.store(true, Ordering::SeqCst);
                continue;
            }
            counter.fetch_add(bytes, Ordering::SeqCst);
            if sender.try_send((class, envelope.clone())).is_err() {
                counter.fetch_sub(bytes, Ordering::SeqCst);
                overflow.store(true, Ordering::SeqCst);
            }
        }
    }

    async fn run(self: Arc<Self>, input: ConnectionInput) {
        self.set_state(ConnectionState::Connecting);
        let mut client = DeviceMqttClient::with_diagnostics(
            input.settings,
            &input.host_id,
            &input.device_id,
            input.token,
            input.diagnostics,
        );
        if let Some(session) = &input.cloud_session {
            client = client.with_cloud_session(session.clone());
        }
        let stop = self.stop.clone();
        let runtime = self.runtime.clone();
        let owner = self.clone();
        let result = client
            .run(
                || stop.load(Ordering::SeqCst) || runtime.is_revoked(),
                |envelope| {
                    owner.fan_out(DeliveryClass::Control, envelope);
                    true
                },
                |envelope| {
                    owner.fan_out(DeliveryClass::Live, envelope);
                    true
                },
                |_| true,
                |event| match event {
                    MqttConnectionEvent::Connected => owner.set_state(ConnectionState::Syncing),
                    MqttConnectionEvent::Subscribed => owner.set_state(ConnectionState::Connected),
                    MqttConnectionEvent::Disconnected => {
                        owner.interrupt_subscribers("device connection was interrupted");
                        owner.set_state(ConnectionState::Reconnecting);
                    }
                },
            )
            .await;
        self.running.store(false, Ordering::SeqCst);
        match result {
            Ok(()) if self.stop.load(Ordering::SeqCst) => {
                self.interrupt_subscribers("the host connection was stopped");
                self.set_state(ConnectionState::Disconnected);
            }
            Ok(()) => {
                self.interrupt_subscribers("the host connection closed");
                self.set_state(ConnectionState::Offline);
            }
            Err(error) => {
                let error = CoreError::from(error);
                let code = error.code();
                self.interrupt_subscribers("the host connection closed");
                self.update(ConnectionSnapshot::terminal(&self.host_id, code));
                let _ = self
                    .runtime
                    .set_state(&self.host_id, ConnectionState::Offline);
            }
        }
    }
}

impl Drop for HostConnection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.lock().expect("connection task").take() {
            task.abort();
        }
    }
}

fn terminal_error(code: Option<&str>) -> CoreError {
    match code {
        Some("unauthorized" | "forbidden") => CoreError::Unauthorized,
        Some("revoked") => CoreError::Revoked,
        Some("not_paired") => CoreError::NotPaired,
        Some("protocol_version_incompatible") => CoreError::ProtocolIncompatible,
        Some(code) => CoreError::Unavailable(code.to_owned()),
        None => CoreError::Unavailable("host connection is not running".into()),
    }
}

/// The per-Core registry of host connections. A connection is created on first
/// demand and replaced once it has stopped.
pub(crate) struct HostConnections {
    entries: Mutex<HashMap<String, Arc<HostConnection>>>,
    changes: broadcast::Sender<ConnectionSnapshot>,
}

impl HostConnections {
    pub(crate) fn new() -> Self {
        let (changes, _) = broadcast::channel(64);
        Self {
            entries: Mutex::new(HashMap::new()),
            changes,
        }
    }

    /// Current snapshot, including a connection that has stopped.
    pub(crate) fn snapshot(&self, host_id: &str) -> Option<ConnectionSnapshot> {
        self.entries
            .lock()
            .expect("connection registry")
            .get(host_id)
            .map(|entry| entry.snapshot())
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ConnectionSnapshot> {
        self.changes.subscribe()
    }

    /// A running connection, if one exists.
    pub(crate) fn running(&self, host_id: &str) -> Option<Arc<HostConnection>> {
        let entries = self.entries.lock().expect("connection registry");
        entries
            .get(host_id)
            .filter(|entry| entry.is_running())
            .cloned()
    }

    /// Return a running connection, creating it when absent or stopped.
    pub(crate) fn connect(&self, input: ConnectionInput) -> Arc<HostConnection> {
        let mut entries = self.entries.lock().expect("connection registry");
        if let Some(existing) = entries.get(&input.host_id) {
            if existing.is_running() {
                return existing.clone();
            }
        }
        let host_id = input.host_id.clone();
        let connection = HostConnection::start(input, self.changes.clone());
        entries.insert(host_id, connection.clone());
        connection
    }

    pub(crate) fn disconnect(&self, host_id: &str) {
        // Keep the entry: "stopped" and "never opened" are different facts, and
        // the app needs the first one to render a stable status after stopping.
        let entries = self.entries.lock().expect("connection registry");
        let Some(connection) = entries.get(host_id) else {
            return;
        };
        connection.stop.store(true, Ordering::SeqCst);
        connection.interrupt_subscribers("the host connection was stopped");
        // The transport may be blocked in a long broker poll; dropping the task
        // closes the socket now instead of waiting for the keepalive.
        if let Some(task) = connection.task.lock().expect("connection task").take() {
            task.abort();
        }
        connection.running.store(false, Ordering::SeqCst);
        let snapshot = ConnectionSnapshot::new(host_id, ConnectionState::Disconnected);
        *connection.snapshot.lock().expect("connection snapshot") = snapshot.clone();
        let _ = self.changes.send(snapshot);
    }
}
