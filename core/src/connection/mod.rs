//! One device connection per Host.
//!
//! The device subscribes to exactly one topic per Host
//! (`whalerein/v1/h/<host>/d/<device>/event`); every session's frames arrive
//! there. A connection therefore belongs to the Host, not to a session:
//! sessions attach to it, read the frames tagged with their session id, and
//! detach without disturbing it.
//!
//! ```text
//! Connecting -> Connected (broker session and subscriptions acknowledged)
//! Connected -> Reconnecting (broker dropped) -> Connecting ...
//! any -> Offline { code } (terminal: TLS, credentials, revocation)
//! ```
//!
//! `Connected` requires the broker to acknowledge the control and live
//! subscriptions; reaching HTTPS alone never counts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use prost::Message;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use whalerein_proto::{EventEnvelope, HostRequest, HostResult, RequestStateNotice};

use crate::access::{
    DeliveryClass, DeviceMqttClient, MqttConnectionEvent, MqttSettings, PublisherSlot, QueryReply,
};
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

/// One query waiter. It is keyed by request identity inside a specific
/// connection generation; a reply from another generation, device or Host can
/// never complete it.
struct QueryWaiter {
    host_id: String,
    device_id: String,
    generation: u64,
    request_id: String,
    channel_id: String,
    expires_at_unix_ms: i64,
    sender: oneshot::Sender<CoreResult<HostResult>>,
}

// Also unregister when an outer deadline or a dropped caller cancels the future.
struct QueryRegistration<'a> {
    waiters: &'a QueryWaiters,
    id: u64,
}

impl Drop for QueryRegistration<'_> {
    fn drop(&mut self) {
        self.waiters
            .entries
            .lock()
            .expect("query waiters")
            .remove(&self.id);
    }
}

/// Per-connection-generation registry of pending query replies.
#[derive(Default)]
struct QueryWaiters {
    next_id: AtomicU64,
    entries: Mutex<HashMap<u64, QueryWaiter>>,
}

impl QueryWaiters {
    fn register(
        &self,
        host_id: &str,
        device_id: &str,
        generation: u64,
        request: &HostRequest,
    ) -> (
        QueryRegistration<'_>,
        oneshot::Receiver<CoreResult<HostResult>>,
    ) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.entries.lock().expect("query waiters").insert(
            id,
            QueryWaiter {
                host_id: host_id.to_owned(),
                device_id: device_id.to_owned(),
                generation,
                request_id: request.request_id.clone(),
                channel_id: request.channel_id.clone(),
                expires_at_unix_ms: request.expires_at_unix_ms,
                sender,
            },
        );
        (QueryRegistration { waiters: self, id }, receiver)
    }

    /// Complete the exact waiter. A reply only completes a waiter from the
    /// same connection generation with the same Host, device, request and
    /// channel identity. Returns whether a waiter was completed.
    fn dispatch(&self, reply: &QueryReply) -> bool {
        let result = &reply.result;
        let waiter = {
            let mut entries = self.entries.lock().expect("query waiters");
            let found = entries.iter().find_map(|(id, waiter)| {
                (waiter.generation == reply.connection_generation
                    && waiter.host_id == result.host_id
                    && waiter.device_id == result.device_id
                    && waiter.request_id == result.request_id
                    && waiter.channel_id == result.channel_id)
                    .then_some(*id)
            });
            found.and_then(|id| entries.remove(&id))
        };
        let Some(waiter) = waiter else {
            return false;
        };
        if crate::util::now_unix_ms() > waiter.expires_at_unix_ms {
            let _ = waiter.sender.send(Err(CoreError::Timeout));
            return true;
        }
        let _ = waiter.sender.send(Ok(reply.result.clone()));
        true
    }

    /// Fail every waiter, for example because the broker session ended. Safe
    /// reads may reissue with their remaining caller deadline.
    fn fail_all(&self, detail: &'static str) {
        let entries = std::mem::take(&mut *self.entries.lock().expect("query waiters"));
        for (_, waiter) in entries {
            let _ = waiter
                .sender
                .send(Err(CoreError::Unavailable(detail.into())));
        }
    }

    /// Fail only waiters bound to a superseded connection generation.
    fn fail_before(&self, generation: u64) {
        let mut entries = self.entries.lock().expect("query waiters");
        let stale: Vec<u64> = entries
            .iter()
            .filter(|(_, waiter)| waiter.generation < generation)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(waiter) = entries.remove(&id) {
                let _ = waiter.sender.send(Err(CoreError::Unavailable(
                    "device connection was replaced".into(),
                )));
            }
        }
    }
}

/// A signal delivered to one command-notice waiter.
#[derive(Debug)]
pub(crate) enum NoticeEvent {
    /// A matching result notice. It is a hint; the authoritative read decides.
    Notice(Box<RequestStateNotice>),
    /// The shared connection completed a (re)subscribe.
    Reconnected,
}

struct NoticeWaiter {
    id: u64,
    host_id: String,
    device_id: String,
    request_id: String,
    command_id: String,
    sender: mpsc::Sender<NoticeEvent>,
}

/// Command notices routed by exact request identity through the shared owner.
/// Dropping the guard removes only this waiter; sibling waits are unaffected.
#[derive(Default)]
struct NoticeWaiters {
    next_id: AtomicU64,
    entries: Mutex<Vec<NoticeWaiter>>,
}

pub(crate) struct NoticeGuard {
    id: u64,
    waiters: Arc<NoticeWaiters>,
}

#[cfg(test)]
pub(crate) fn test_notice_guard() -> NoticeGuard {
    let waiters = Arc::new(NoticeWaiters::default());
    let id = waiters.next_id.fetch_add(1, Ordering::SeqCst);
    NoticeGuard { id, waiters }
}

impl Drop for NoticeGuard {
    fn drop(&mut self) {
        self.waiters
            .entries
            .lock()
            .expect("notice waiters")
            .retain(|waiter| waiter.id != self.id);
    }
}

impl NoticeWaiters {
    fn register(
        self: &Arc<Self>,
        host_id: &str,
        device_id: &str,
        request_id: &str,
        command_id: &str,
    ) -> (mpsc::Receiver<NoticeEvent>, NoticeGuard) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel(16);
        self.entries
            .lock()
            .expect("notice waiters")
            .push(NoticeWaiter {
                id,
                host_id: host_id.to_owned(),
                device_id: device_id.to_owned(),
                request_id: request_id.to_owned(),
                command_id: command_id.to_owned(),
                sender,
            });
        (
            receiver,
            NoticeGuard {
                id,
                waiters: self.clone(),
            },
        )
    }

    fn dispatch(&self, notice: &RequestStateNotice) {
        let entries = self.entries.lock().expect("notice waiters");
        let mut blocked = Vec::new();
        for waiter in entries.iter() {
            if waiter.request_id == notice.request_id
                && waiter.host_id == notice.host_id
                && waiter.device_id == notice.device_id
                && waiter.command_id == notice.command_id
                && waiter
                    .sender
                    .try_send(NoticeEvent::Notice(Box::new(notice.clone())))
                    .is_err()
            {
                // A full or closed queue drops only this notice; the waiter
                // stays installed and its reserved final read still decides.
                blocked.push(waiter.id);
            }
        }
        drop(entries);
        let _ = blocked;
    }

    fn reconnected(&self) {
        let entries = self.entries.lock().expect("notice waiters");
        for waiter in entries.iter() {
            let _ = waiter.sender.try_send(NoticeEvent::Reconnected);
        }
    }
}

/// The live device connection. Cheap to clone and shared by every holder.
pub(crate) struct HostConnection {
    host_id: String,
    device_id: String,
    runtime: Arc<Runtime>,
    changes: watch::Sender<ConnectionSnapshot>,
    global: broadcast::Sender<ConnectionSnapshot>,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    stop: Arc<AtomicBool>,
    task: Mutex<Option<JoinHandle<()>>>,
    running: Arc<AtomicBool>,
    queue_bytes: usize,
    /// Broker session generation. It advances on every CONNACK and fences
    /// waiters created under an older session.
    generation: AtomicU64,
    queries: QueryWaiters,
    notices: Arc<NoticeWaiters>,
    publisher: Arc<PublisherSlot>,
    /// The target connection stamp observed for the current generation. Core
    /// does not know its own broker connection id; it validates that every
    /// reply for a generation is addressed to the same target.
    target: Mutex<Option<(u64, String)>>,
}

impl HostConnection {
    fn start(input: ConnectionInput, global: broadcast::Sender<ConnectionSnapshot>) -> Arc<Self> {
        let initial = ConnectionSnapshot::new(&input.host_id, ConnectionState::Connecting);
        let (changes, _) = watch::channel(initial);
        let connection = Arc::new(Self {
            host_id: input.host_id.clone(),
            device_id: input.device_id.clone(),
            runtime: input.runtime.clone(),
            changes,
            global,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            stop: Arc::new(AtomicBool::new(false)),
            task: Mutex::new(None),
            running: Arc::new(AtomicBool::new(true)),
            queue_bytes: input.queue_bytes,
            generation: AtomicU64::new(0),
            queries: QueryWaiters::default(),
            notices: Arc::new(NoticeWaiters::default()),
            publisher: Arc::new(PublisherSlot::default()),
            target: Mutex::new(None),
        });
        let runner = connection.clone();
        let task = tokio::spawn(async move { runner.run(input).await });
        *connection.task.lock().expect("connection task") = Some(task);
        connection
    }

    pub(crate) fn subscribe_state(&self) -> watch::Receiver<ConnectionSnapshot> {
        self.changes.subscribe()
    }

    pub(crate) fn snapshot(&self) -> ConnectionSnapshot {
        self.changes.borrow().clone()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub(crate) fn host_id(&self) -> &str {
        &self.host_id
    }

    pub(crate) fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The current broker session generation. Zero means no session yet.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.snapshot().state == ConnectionState::Connected
    }

    /// Publish one query-lane request and wait for its correlated reply.
    ///
    /// The waiter is registered before publication; the shared owner validates
    /// the reply identity and the original application deadline. A connection
    /// replacement fails the waiter with a retryable transport error so safe
    /// reads can reissue with the remaining caller deadline.
    pub(crate) async fn query(
        &self,
        request: &HostRequest,
        cancellation: &Cancellation,
    ) -> CoreResult<HostResult> {
        if request.host_id != self.host_id || request.device_id != self.device_id {
            return Err(CoreError::InvalidInput(
                "query identity does not match this host connection".into(),
            ));
        }
        let remaining = request
            .expires_at_unix_ms
            .saturating_sub(crate::util::now_unix_ms());
        if remaining <= 0 {
            return Err(CoreError::Timeout);
        }
        let exchange = async {
            self.wait_ready(cancellation).await?;
            let publisher = self
                .publisher
                .get()
                .ok_or_else(|| CoreError::Unavailable("MQTT query lane is not ready".into()))?;
            let generation = self.generation();
            if generation == 0 {
                return Err(CoreError::Unavailable(
                    "MQTT query lane is not ready".into(),
                ));
            }
            let (_registration, receiver) =
                self.queries
                    .register(&self.host_id, &self.device_id, generation, request);
            publisher.publish(request).await.map_err(CoreError::from)?;
            receiver
                .await
                .map_err(|_| CoreError::Unavailable("device connection was interrupted".into()))?
        };
        let result = tokio::select! {
            result = exchange => result,
            _ = cancellation.cancelled() => Err(CoreError::Cancelled),
            _ = tokio::time::sleep(Duration::from_millis(remaining as u64)) => Err(CoreError::Timeout),
        };
        // The application deadline is authoritative even when the broker
        // delivered the reply late.
        match result {
            Ok(_) if crate::util::now_unix_ms() > request.expires_at_unix_ms => {
                Err(CoreError::Timeout)
            }
            other => other,
        }
    }

    /// Register this exact command identity for shared-connection notices.
    /// Attach before submitting so a result published early is not lost.
    pub(crate) fn register_notice(
        &self,
        request_id: &str,
        command_id: &str,
    ) -> (mpsc::Receiver<NoticeEvent>, NoticeGuard, bool) {
        let ready = self.is_connected();
        let (receiver, guard) =
            self.notices
                .register(&self.host_id, &self.device_id, request_id, command_id);
        (receiver, guard, ready)
    }

    fn dispatch_reply(&self, reply: QueryReply) -> bool {
        let result = &reply.result;
        if result.host_id != self.host_id || result.device_id != self.device_id {
            return true;
        }
        if result.relay_version != crate::access::transport::RELAY_EPOCH_V5 {
            return true;
        }
        // The reply must be addressed to this connection's session. Core does
        // not know its own broker reference, so it accepts the first stamp for
        // a generation and rejects a different one for the same generation.
        let Some(target) = reply.target.as_deref().filter(|value| !value.is_empty()) else {
            return true;
        };
        {
            let mut observed = self.target.lock().expect("query target");
            if observed.as_ref().is_some_and(|(generation, expected)| {
                *generation == reply.connection_generation && expected != target
            }) {
                return true;
            }
            if self.queries.dispatch(&reply) {
                *observed = Some((reply.connection_generation, target.to_owned()));
            }
        }
        true
    }

    fn update(&self, next: ConnectionSnapshot) {
        self.changes.send_replace(next.clone());
        let _ = self.global.send(next);
    }

    fn set_state(&self, state: ConnectionState) {
        let _ = self.runtime.set_state(&self.host_id, state);
        self.update(ConnectionSnapshot::new(&self.host_id, state));
    }

    /// Wait for the transport to become usable. A terminal state fails with
    /// the recorded reason instead of pretending a transient outage.
    pub(crate) async fn wait_ready(&self, cancellation: &Cancellation) -> CoreResult<()> {
        let mut changes = self.subscribe_state();
        loop {
            let snapshot = changes.borrow_and_update().clone();
            match snapshot.state {
                ConnectionState::Connected => return Ok(()),
                ConnectionState::Offline | ConnectionState::Disconnected => {
                    return Err(terminal_error(snapshot.code.as_deref()));
                }
                _ => {}
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(CoreError::Cancelled),
                changed = changes.changed() => {
                    if changed.is_err() {
                        return Err(terminal_error(None));
                    }
                }
            }
        }
    }

    pub(crate) fn control_queue_bytes(&self) -> usize {
        self.queue_bytes
    }

    /// Attach one observer to the shared transport. The guard detaches it when
    /// dropped; the connection keeps running.
    pub(crate) fn subscribe(&self) -> (SubscriptionParts, SubscriptionGuard) {
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
            SubscriptionGuard {
                subscriber_id: id,
                subscribers: self.subscribers.clone(),
            },
        )
    }

    fn interrupt_subscribers(&self, detail: &'static str) {
        self.queries.fail_all(detail);
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
                |notice| {
                    owner.notices.dispatch(notice);
                    true
                },
                |reply| owner.dispatch_reply(reply),
                |event| match event {
                    // The broker session exists but is not usable until every
                    // subscription is acknowledged. A new generation fails
                    // waiters bound to the replaced session.
                    MqttConnectionEvent::Connected { generation } => {
                        owner.generation.store(generation, Ordering::SeqCst);
                        owner.queries.fail_before(generation);
                        *owner.target.lock().expect("query target") = None;
                    }
                    MqttConnectionEvent::Subscribed { .. } => {
                        owner.notices.reconnected();
                        owner.set_state(ConnectionState::Connected);
                    }
                    MqttConnectionEvent::Disconnected => {
                        owner.interrupt_subscribers("device connection was interrupted");
                        owner.set_state(ConnectionState::Reconnecting);
                    }
                },
                Some(&owner.publisher),
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

/// Start one connection against an in-process test broker. The caller owns
/// the returned handle; dropping it stops the transport.
#[cfg(test)]
pub(crate) fn test_connection(
    settings: MqttSettings,
    host_id: &str,
    device_id: &str,
    token: &str,
) -> Arc<HostConnection> {
    let (global, _) = broadcast::channel(8);
    HostConnection::start(
        ConnectionInput {
            host_id: host_id.to_owned(),
            device_id: device_id.to_owned(),
            settings,
            token: token.to_owned(),
            cloud_session: None,
            runtime: Arc::new(Runtime::new()),
            diagnostics: Arc::new(Diagnostics::default()),
            queue_bytes: 64 * 1024,
        },
        global,
    )
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
        connection.update(snapshot);
    }
}
