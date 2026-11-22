//! Core diagnostic forwarding across the binding boundary.
//!
//! Core calls [`whalerein_core::api::DiagnosticSink::on_event`] synchronously
//! on the producing thread. A foreign callback can be slow or throw, so the
//! binding copies the already-redacted event into a bounded queue and delivers
//! it from one dedicated thread. Dropping the oldest queued event is reported
//! as an explicit `diagnostics_dropped` event with a count, never silently.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use whalerein_core::api::{
    DiagnosticContext as CoreDiagnosticContext, DiagnosticEvent, DiagnosticSink,
};

const QUEUE_CAPACITY: usize = 256;
const MAX_FIELDS: usize = 32;
const MAX_FIELD_KEY_BYTES: usize = 64;
const MAX_FIELD_VALUE_BYTES: usize = 512;

/// Redacted event copied across the boundary. `fields` holds only the scalar
/// values Core already serialized; the binding never adds message text, tool
/// arguments, credentials or paths.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticEventView {
    pub time_unix_ms: i64,
    pub trace_id: String,
    pub level: String,
    pub event: String,
    pub fields: HashMap<String, String>,
}

/// Foreign diagnostic receiver implemented by the app.
///
/// Calls may arrive on the binding's delivery thread and must return promptly;
/// throwing is caught by UniFFI and does not affect Core.
#[uniffi::export(with_foreign)]
pub trait CoreDiagnosticSink: Send + Sync {
    fn on_event(&self, event: DiagnosticEventView);
}

impl DiagnosticEventView {
    fn from_core(event: &DiagnosticEvent) -> Self {
        Self::from_value(serde_json::to_value(event).unwrap_or_default())
    }

    fn from_value(value: serde_json::Value) -> Self {
        let level = value
            .get("level")
            .and_then(|level| level.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let event_name = value
            .get("event")
            .and_then(|event| event.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let mut fields = HashMap::new();
        if let Some(object) = value.as_object() {
            for (key, value) in object {
                if fields.len() >= MAX_FIELDS {
                    break;
                }
                if matches!(
                    key.as_str(),
                    "time_unix_ms" | "trace_id" | "level" | "event"
                ) {
                    continue;
                }
                let Some(value) = scalar(value) else {
                    continue;
                };
                if key.is_empty() || key.len() > MAX_FIELD_KEY_BYTES {
                    continue;
                }
                fields.insert(key.clone(), truncate(value, MAX_FIELD_VALUE_BYTES));
            }
        }
        let time_unix_ms = value
            .get("time_unix_ms")
            .and_then(|time| time.as_i64())
            .unwrap_or_default();
        let trace_id = value
            .get("trace_id")
            .and_then(|trace| trace.as_str())
            .map(|trace| truncate(trace.to_owned(), MAX_FIELD_VALUE_BYTES))
            .unwrap_or_default();
        Self {
            time_unix_ms,
            trace_id,
            level,
            event: event_name,
            fields,
        }
    }
}

fn scalar(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn truncate(mut value: String, limit: usize) -> String {
    if value.len() > limit {
        let mut boundary = limit;
        while boundary > 0 && !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    value
}

struct Queue {
    events: VecDeque<DiagnosticEventView>,
    dropped: u64,
    stop: bool,
}

/// Bounded, non-blocking bridge between Core's synchronous sink and the
/// foreign receiver. One background thread drains the queue in order.
pub(crate) struct DiagnosticDispatcher {
    shared: Mutex<Queue>,
    ready: Condvar,
    sink: Arc<dyn CoreDiagnosticSink>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl DiagnosticDispatcher {
    pub(crate) fn start(sink: Arc<dyn CoreDiagnosticSink>) -> Arc<Self> {
        let dispatcher = Arc::new(Self {
            shared: Mutex::new(Queue {
                events: VecDeque::new(),
                dropped: 0,
                stop: false,
            }),
            ready: Condvar::new(),
            sink,
            thread: Mutex::new(None),
        });
        let worker = dispatcher.clone();
        let handle = std::thread::Builder::new()
            .name("whalerein-diagnostics".into())
            .spawn(move || worker.run())
            .ok();
        if let Ok(mut thread) = dispatcher.thread.lock() {
            *thread = handle;
        }
        dispatcher
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push(&self, event: DiagnosticEventView) {
        let mut queue = self.lock();
        Self::enqueue(&mut queue, event);
        drop(queue);
        self.ready.notify_one();
    }

    fn enqueue(queue: &mut Queue, event: DiagnosticEventView) {
        if queue.events.len() >= QUEUE_CAPACITY {
            queue.events.pop_front();
            queue.dropped = queue.dropped.saturating_add(1);
        }
        queue.events.push_back(event);
    }

    #[cfg(test)]
    fn fill_without_notifying(&self, count: usize) {
        let mut queue = self.lock();
        for index in 0..count {
            Self::enqueue(
                &mut queue,
                DiagnosticEventView {
                    time_unix_ms: 1,
                    trace_id: "trace".into(),
                    level: "info".into(),
                    event: format!("event-{index}"),
                    fields: HashMap::new(),
                },
            );
        }
    }

    fn run(&self) {
        loop {
            let batch = {
                let mut queue = self.lock();
                while queue.events.is_empty() && !queue.stop {
                    queue = self
                        .ready
                        .wait(queue)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                if queue.stop && queue.events.is_empty() {
                    return;
                }
                let dropped = std::mem::take(&mut queue.dropped);
                let mut batch: Vec<DiagnosticEventView> = queue.events.drain(..).collect();
                if dropped > 0 {
                    let mut fields = HashMap::new();
                    fields.insert("count".to_owned(), dropped.to_string());
                    batch.insert(
                        0,
                        DiagnosticEventView {
                            time_unix_ms: now_unix_ms(),
                            trace_id: String::new(),
                            level: "warn".into(),
                            event: "diagnostics_dropped".into(),
                            fields,
                        },
                    );
                }
                batch
            };
            for event in batch {
                self.sink.on_event(event);
            }
        }
    }
}

impl Drop for DiagnosticDispatcher {
    fn drop(&mut self) {
        {
            let mut queue = self.lock();
            queue.stop = true;
        }
        self.ready.notify_all();
        if let Ok(mut thread) = self.thread.lock() {
            if let Some(handle) = thread.take() {
                let _ = handle.join();
            }
        }
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

struct SinkBridge {
    dispatcher: Arc<DiagnosticDispatcher>,
}

impl DiagnosticSink for SinkBridge {
    fn on_event(&self, event: &DiagnosticEvent) {
        self.dispatcher.push(DiagnosticEventView::from_core(event));
    }
}

/// Opt-in diagnostic context reserved before Core opens.
///
/// Construct it with the app receiver, read `trace_id` for correlation, and
/// pass it to `MobileCore.newWithDiagnosticContext`. The context can be used
/// once; Core then owns delivery and the reservation keeps the dispatcher
/// alive until it is dropped.
#[derive(uniffi::Object)]
pub struct DiagnosticContext {
    dispatcher: Arc<DiagnosticDispatcher>,
    trace_id: String,
    core: Mutex<Option<CoreDiagnosticContext>>,
}

#[uniffi::export]
impl DiagnosticContext {
    #[uniffi::constructor]
    pub fn new(sink: Arc<dyn CoreDiagnosticSink>) -> Arc<Self> {
        let dispatcher = DiagnosticDispatcher::start(sink);
        let bridge = Arc::new(SinkBridge {
            dispatcher: dispatcher.clone(),
        });
        let core = CoreDiagnosticContext::new(bridge);
        let trace_id = core.trace_id().to_owned();
        Arc::new(Self {
            dispatcher,
            trace_id,
            core: Mutex::new(Some(core)),
        })
    }

    /// Trace identity reserved before configuration is loaded.
    pub fn trace_id(&self) -> String {
        self.trace_id.clone()
    }
}

impl DiagnosticContext {
    pub(crate) fn dispatcher(&self) -> Arc<DiagnosticDispatcher> {
        self.dispatcher.clone()
    }

    /// Take the reservation exactly once. A second use is rejected.
    pub(crate) fn take_core(&self) -> Option<CoreDiagnosticContext> {
        self.core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<DiagnosticEventView>>,
    }

    impl CoreDiagnosticSink for Recorder {
        fn on_event(&self, event: DiagnosticEventView) {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event);
        }
    }

    fn event(name: &str) -> DiagnosticEventView {
        DiagnosticEventView {
            time_unix_ms: 1,
            trace_id: "trace".into(),
            level: "info".into(),
            event: name.into(),
            fields: HashMap::new(),
        }
    }

    fn wait_for(recorder: &Recorder, count: usize) -> Vec<DiagnosticEventView> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = recorder
                .events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if events.len() >= count || Instant::now() >= deadline {
                return events;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn events_are_delivered_in_order_by_the_worker() {
        let recorder = Arc::new(Recorder::default());
        let dispatcher = DiagnosticDispatcher::start(recorder.clone());
        for index in 0..3 {
            dispatcher.push(event(&format!("event-{index}")));
        }
        let events = wait_for(&recorder, 3);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event, "event-0");
        assert_eq!(events[2].event, "event-2");
    }

    #[test]
    fn overflow_drops_the_oldest_and_reports_a_count() {
        let recorder = Arc::new(Recorder::default());
        let dispatcher = DiagnosticDispatcher::start(recorder.clone());
        // Fill past capacity while the worker is not signalled, so exactly the
        // oldest ten events are dropped before delivery starts.
        dispatcher.fill_without_notifying(QUEUE_CAPACITY + 10);
        dispatcher.ready.notify_one();
        let events = wait_for(&recorder, QUEUE_CAPACITY + 1);
        let dropped = events
            .iter()
            .find(|event| event.event == "diagnostics_dropped")
            .expect("overflow is reported explicitly");
        let count: u64 = dropped.fields["count"].parse().unwrap();
        assert_eq!(count, 10);
        assert!(events.last().is_some_and(|event| event
            .event
            .trim_start_matches("event-")
            .parse::<usize>()
            .is_ok()));
    }

    #[test]
    fn core_event_fields_are_allowlisted_scalars() {
        let value = serde_json::json!({
            "time_unix_ms": 7,
            "trace_id": "trace-1",
            "level": "info",
            "event": "configuration_loaded",
        });
        let view = DiagnosticEventView::from_value(value);
        assert_eq!(view.event, "configuration_loaded");
        assert_eq!(view.level, "info");
        assert_eq!(view.trace_id, "trace-1");
        assert!(view.fields.is_empty());

        let value = serde_json::json!({
            "time_unix_ms": 8,
            "trace_id": "trace-2",
            "level": "warn",
            "event": "http_transport_failed",
            "code": "timeout",
            "elapsed_ms": 12,
            "nested": {"ignored": true},
        });
        let view = DiagnosticEventView::from_value(value);
        assert_eq!(view.time_unix_ms, 8);
        assert_eq!(view.fields["code"], "timeout");
        assert_eq!(view.fields["elapsed_ms"], "12");
        assert!(!view.fields.contains_key("nested"));
    }

    #[test]
    fn a_reserved_context_uses_its_trace_id_once() {
        let recorder = Arc::new(Recorder::default());
        let context = DiagnosticContext::new(recorder);
        let trace = context.trace_id();
        assert!(!trace.is_empty());
        assert!(context.take_core().is_some());
        assert!(context.take_core().is_none());
    }
}
