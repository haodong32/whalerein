//! Bridges Core's existing redacted diagnostic stream to the app timeline.
//!
//! Core owns the event vocabulary, redaction, levels and trace identity. This
//! module only projects one `DiagnosticEvent` into the flat, enum-agnostic
//! [`DiagnosticEventView`] that Swift consumes. It never mirrors Core's
//! non-exhaustive `DiagnosticKind` enum, so a new Core event still crosses the
//! boundary as a stable `event` string plus allowlisted scalar fields.

use std::collections::HashMap;
use std::panic::{catch_unwind, RefUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use whalerein_core::api::{DiagnosticEvent, DiagnosticSink};

/// Upper bound for an opaque request identifier carried by Core events.
const MAX_REQUEST_ID_CHARS: usize = 64;

/// Scalar Core event fields that may cross into the app's timeline.
///
/// These are the fields Core already serializes for its own journal. Bodies,
/// payloads, tokens, message text and tool parameters are deliberately absent.
const ALLOWLISTED_FIELDS: &[&str] = &[
    "status",
    "elapsed_ms",
    "layer",
    "stage",
    "outcome",
    "code",
    "polls",
    "bytes",
    "limit",
    "retry_in_ms",
    "reason",
    "reason_code",
    "request_kind",
    "request_id",
    "parts",
    "capability",
];

/// One redacted Core diagnostic event, ready for the app's correlated timeline.
///
/// `time_unix_ms`, `trace_id`, `level` and `event` are Core-provided.
/// `fields` holds only [`ALLOWLISTED_FIELDS`] with scalar values stringified;
/// unknown or structured event data is never mirrored.
#[derive(uniffi::Record, Clone, Debug)]
pub struct DiagnosticEventView {
    pub time_unix_ms: i64,
    pub trace_id: String,
    /// `"debug"`, `"info"`, `"warn"` or `"error"`; Core's lowercase level name.
    pub level: String,
    /// Stable Core event name, for example `"operation_diagnostic"`.
    pub event: String,
    /// Allowlisted scalar fields only, stringified. No payloads, bodies,
    /// tokens, message text, tool parameters or identifiers beyond what Core
    /// already serializes.
    pub fields: HashMap<String, String>,
    /// Monotonic per-`MobileCore` delivery sequence, starting at 0.
    pub sequence: u64,
}

impl DiagnosticEventView {
    /// Project one serialized Core event.
    ///
    /// This is intentionally total: missing, unknown or non-scalar values are
    /// dropped rather than panicking on the producing Core thread.
    fn project(value: &serde_json::Value, sequence: u64) -> Self {
        let text = |name: &str| value.get(name).and_then(scalar_text).unwrap_or_default();
        let mut fields = HashMap::new();
        for name in ALLOWLISTED_FIELDS {
            if let Some(field) = value.get(*name).and_then(scalar_text) {
                if field.is_empty() {
                    continue;
                }
                let field = if *name == "request_id" {
                    truncate_chars(field, MAX_REQUEST_ID_CHARS)
                } else {
                    field
                };
                fields.insert((*name).to_owned(), field);
            }
        }
        Self {
            time_unix_ms: value
                .get("time_unix_ms")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
            trace_id: text("trace_id"),
            level: text("level"),
            event: text("event"),
            fields,
            sequence,
        }
    }
}

/// Foreign-implemented diagnostic receiver (Swift adapter).
///
/// Core calls this synchronously on the producing thread; implementations must
/// return promptly and queue any slow work on their own bounded queue. A panic
/// or thrown error is swallowed by the binding and never changes a Core result.
#[uniffi::export(with_foreign)]
pub trait MobileDiagnosticSink: Send + Sync {
    fn on_event(&self, event: DiagnosticEventView);
}

/// Marks the foreign object as safe to call inside `catch_unwind`.
///
/// The foreign implementation is outside Rust's unwind-safety analysis; the
/// adapter catches every panic at the boundary, so treating it as unwind-safe
/// is sound here.
struct UnwindSafeSink(Arc<dyn MobileDiagnosticSink>);

impl RefUnwindSafe for UnwindSafeSink {}

/// Adapts the foreign boundary to Core's synchronous `DiagnosticSink`.
pub(crate) struct DiagnosticSinkAdapter {
    sink: UnwindSafeSink,
    sequence: AtomicU64,
}

impl DiagnosticSinkAdapter {
    pub(crate) fn new(sink: Arc<dyn MobileDiagnosticSink>) -> Self {
        Self {
            sink: UnwindSafeSink(sink),
            sequence: AtomicU64::new(0),
        }
    }
}

impl DiagnosticSink for DiagnosticSinkAdapter {
    fn on_event(&self, event: &DiagnosticEvent) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        // Serialization of this struct cannot fail in practice; keep the
        // fallback total so a sink callback never panics inside Core.
        let value = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
        let view = DiagnosticEventView::project(&value, sequence);
        let sink = &self.sink;
        let _ = catch_unwind(move || sink.0.on_event(view));
    }
}

fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn truncate_chars(mut text: String, max_chars: usize) -> String {
    if let Some((index, _)) = text.char_indices().nth(max_chars) {
        text.truncate(index);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_keeps_only_allowlisted_diagnostic_fields() {
        let synthetic = serde_json::json!({
            "time_unix_ms": 1234,
            "trace_id": "trace-1",
            "level": "warn",
            "event": "http_completed",
            "status": 503,
            "elapsed_ms": 42,
            "layer": "access",
            "stage": "request",
            "outcome": "failed",
            "code": "unavailable",
            "request_id": "r".repeat(100),
            "unknown_field": "x".repeat(4096),
            "payload": { "body": "y".repeat(4096) },
        });
        let view = DiagnosticEventView::project(&synthetic, 7);
        assert_eq!(view.time_unix_ms, 1234);
        assert_eq!(view.trace_id, "trace-1");
        assert_eq!(view.level, "warn");
        assert_eq!(view.event, "http_completed");
        assert_eq!(view.sequence, 7);
        assert_eq!(view.fields["status"], "503");
        assert_eq!(view.fields["elapsed_ms"], "42");
        assert_eq!(view.fields["layer"], "access");
        assert_eq!(view.fields["request_id"].chars().count(), 64);
        for key in view.fields.keys() {
            assert!(
                ALLOWLISTED_FIELDS.contains(&key.as_str()),
                "unexpected field {key}"
            );
        }
        assert!(!view.fields.contains_key("unknown_field"));
        assert!(!view.fields.contains_key("payload"));
        assert!(!view.fields.contains_key("time_unix_ms"));
    }
}
