//! Format and write client diagnostics without blocking Core's event producer.

use std::sync::{mpsc, Arc, Mutex};
use std::thread::Builder;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use whalerein_core::api::{DiagnosticEvent, DiagnosticLevel, DiagnosticSink};

const QUEUE_CAPACITY: usize = 128;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

enum PendingEvent {
    Core(DiagnosticEvent),
    Cli(Value),
}

pub(crate) struct CliDiagnosticSink {
    minimum: DiagnosticLevel,
    events: Mutex<Option<mpsc::SyncSender<PendingEvent>>>,
}

impl CliDiagnosticSink {
    pub(crate) fn start(verbose: bool, json: bool) -> (Arc<Self>, mpsc::Receiver<()>) {
        let (events, queued) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (finished, completion) = mpsc::channel();
        let writer = Builder::new()
            .name("whalerein-diagnostics".into())
            .spawn(move || {
                write_events(queued, json);
                let _ = finished.send(());
            });
        let minimum = if verbose {
            DiagnosticLevel::Debug
        } else {
            DiagnosticLevel::Warn
        };
        let events = Mutex::new(writer.is_ok().then_some(events));
        (Arc::new(Self { minimum, events }), completion)
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Arc<Self> {
        Arc::new(Self {
            minimum: DiagnosticLevel::Warn,
            events: Mutex::new(None),
        })
    }

    pub(crate) fn cli_event(&self, trace_id: &str, event: impl FnOnce() -> Value) {
        self.enqueue(DiagnosticLevel::Info, || {
            let mut event = event();
            event["level"] = Value::from("info");
            event["trace_id"] = Value::from(trace_id);
            event["time_unix_ms"] = Value::from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            );
            PendingEvent::Cli(event)
        });
    }

    fn enqueue(&self, level: DiagnosticLevel, event: impl FnOnce() -> PendingEvent) {
        if level >= self.minimum {
            if let Some(events) = self.events.lock().expect("diagnostic queue lock").as_ref() {
                let _ = events.try_send(event());
            }
        }
    }

    pub(crate) fn finish(&self, completion: mpsc::Receiver<()>) {
        // Close the queue even if cancelled Core tasks still share the sink.
        self.events.lock().expect("diagnostic queue lock").take();
        let _ = completion.recv_timeout(SHUTDOWN_TIMEOUT);
    }
}

impl DiagnosticSink for CliDiagnosticSink {
    fn on_event(&self, event: &DiagnosticEvent) {
        self.enqueue(event.level, || PendingEvent::Core(event.clone()));
    }
}

fn write_events(queued: mpsc::Receiver<PendingEvent>, json: bool) {
    for event in queued {
        let value = match event {
            PendingEvent::Core(event) => match serde_json::to_value(event) {
                Ok(value) => value,
                Err(_) => continue,
            },
            PendingEvent::Cli(value) => value,
        };
        let line = if json {
            value.to_string()
        } else {
            render_text(&value)
        };
        if crate::stream::to_stderr(format!("{line}\n").as_bytes()).is_err() {
            break;
        }
    }
}

fn render_text(event: &Value) -> String {
    use std::fmt::Write;

    let level = event["level"]
        .as_str()
        .unwrap_or("info")
        .to_ascii_uppercase();
    let name = event["event"].as_str().unwrap_or("diagnostic");
    let mut line = format!("{level} {name}");
    for (key, value) in event.as_object().into_iter().flatten() {
        if !matches!(key.as_str(), "level" | "event") {
            let _ = write!(line, " {key}={value}");
        }
    }
    line
}
