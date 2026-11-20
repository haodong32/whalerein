//! Live diagnostic events and bounded history for the most recent failure.
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::api::{CoreError, CoreResult};
use crate::util::{now_unix_ms, random_id};

pub const FAILURE_FILE: &str = "last-failure.json";
const MAX_EVENTS: usize = 32;
const MAX_BYTES: u64 = 32 * 1024;
const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Redacted metadata produced by a single client instance.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DiagnosticEvent {
    pub time_unix_ms: i64,
    pub trace_id: String,
    pub level: DiagnosticLevel,
    #[serde(flatten)]
    pub kind: DiagnosticKind,
}

/// Structured event data. The serialized event names are stable identifiers.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
#[serde(tag = "event")]
pub enum DiagnosticKind {
    #[serde(rename = "configuration_loaded")]
    ConfigurationLoaded,
    #[serde(rename = "configuration_failed")]
    ConfigurationFailed { code: &'static str },
    #[serde(rename = "operation_diagnostic")]
    OperationStage {
        layer: &'static str,
        stage: &'static str,
        outcome: &'static str,
        code: String,
    },
    #[serde(rename = "http_completed")]
    HttpCompleted {
        status: u16,
        elapsed_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<&'static str>,
    },
    #[serde(rename = "http_transport_failed")]
    HttpTransportFailed { code: &'static str, elapsed_ms: u64 },
    #[serde(rename = "access_request_submitting")]
    AccessRequestSubmitting {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        request_kind: &'static str,
    },
    #[serde(rename = "access_request_accepted")]
    AccessRequestAccepted {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        status: &'static str,
    },
    #[serde(rename = "request_receipt_mismatch")]
    RequestReceiptMismatch {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "request_state_mismatch")]
    RequestStateMismatch {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "access_request_poll_failed")]
    AccessRequestPollFailed {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        code: &'static str,
        polls: u32,
    },
    #[serde(rename = "access_request_poll_recovered")]
    AccessRequestPollRecovered {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        polls: u32,
    },
    #[serde(rename = "access_request_observed")]
    AccessRequestObserved {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        status: &'static str,
        polls: u32,
        elapsed_ms: u64,
    },
    #[serde(rename = "access_request_unconfirmed")]
    AccessRequestUnconfirmed {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        code: &'static str,
        polls: u32,
        elapsed_ms: u64,
    },
    #[serde(rename = "mqtt_subscribed")]
    MqttSubscribed,
    #[serde(rename = "mqtt_disconnected")]
    MqttDisconnected {
        reason: &'static str,
        reason_code: i32,
        bytes: u64,
        limit: u64,
        retry_in_ms: Option<u64>,
    },
    #[serde(rename = "secure_channel_established")]
    SecureChannelEstablished,
    #[serde(rename = "capability_missing")]
    CapabilityMissing { capability: &'static str },
    #[serde(rename = "diagnostic_save_failed")]
    DiagnosticSaveFailed,
}

impl DiagnosticKind {
    fn level(&self) -> DiagnosticLevel {
        match self {
            Self::RequestReceiptMismatch { .. } | Self::RequestStateMismatch { .. } => {
                DiagnosticLevel::Error
            }
            Self::HttpTransportFailed { .. }
            | Self::AccessRequestPollFailed { .. }
            | Self::AccessRequestUnconfirmed { .. }
            | Self::MqttDisconnected { .. }
            | Self::DiagnosticSaveFailed => DiagnosticLevel::Warn,
            Self::HttpCompleted { .. }
            | Self::AccessRequestSubmitting { .. }
            | Self::CapabilityMissing { .. } => DiagnosticLevel::Debug,
            Self::ConfigurationLoaded
            | Self::ConfigurationFailed { .. }
            | Self::OperationStage { .. }
            | Self::AccessRequestAccepted { .. }
            | Self::AccessRequestPollRecovered { .. }
            | Self::AccessRequestObserved { .. }
            | Self::MqttSubscribed
            | Self::SecureChannelEstablished => DiagnosticLevel::Info,
        }
    }
}

/// Receives events synchronously on the thread that produces them. Calls may
/// arrive concurrently. Implementations should return promptly and hand off
/// slow work to their own bounded queue.
pub trait DiagnosticSink: Send + Sync {
    fn on_event(&self, event: &DiagnosticEvent);
}

/// Reserves the Core trace ID before a client is opened so its caller can
/// correlate initialization and runtime events.
pub struct DiagnosticContext(pub(crate) Arc<Diagnostics>);

impl DiagnosticContext {
    pub fn new(sink: Arc<dyn DiagnosticSink>) -> Self {
        Self(Arc::new(Diagnostics::new(Some(sink))))
    }

    pub fn trace_id(&self) -> &str {
        self.0.trace_id()
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct CapturingSink(pub Mutex<Vec<serde_json::Value>>);

#[cfg(test)]
impl DiagnosticSink for CapturingSink {
    fn on_event(&self, event: &DiagnosticEvent) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
    }
}

/// Log only canonical Core-generated opaque request IDs.
pub(crate) fn request_id_for_log(request_id: &str) -> Option<String> {
    valid_opaque_id(request_id).then(|| request_id.to_owned())
}

fn valid_opaque_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEvent {
    pub time_unix_ms: i64,
    pub layer: String,
    pub stage: String,
    pub outcome: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureRecord {
    pub trace_id: String,
    pub recorded_at_unix_ms: i64,
    pub events: Vec<HistoryEvent>,
}

pub(crate) struct Diagnostics {
    trace_id: String,
    events: Mutex<VecDeque<HistoryEvent>>,
    sink: Option<Arc<dyn DiagnosticSink>>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Diagnostics {
    pub fn new(sink: Option<Arc<dyn DiagnosticSink>>) -> Self {
        Self {
            trace_id: random_id(16),
            events: Mutex::new(VecDeque::new()),
            sink,
        }
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    // Callers pass stable vocabulary only; never errors' Display strings.
    pub fn record_stage(
        &self,
        layer: &'static str,
        stage: &'static str,
        outcome: &'static str,
        code: &str,
    ) {
        if !valid_stage_fields([layer, stage, outcome, code]) {
            return;
        }
        self.record(DiagnosticKind::OperationStage {
            layer,
            stage,
            outcome,
            code: code.into(),
        });
    }

    pub fn record(&self, kind: DiagnosticKind) {
        let time_unix_ms = now_unix_ms();
        if let DiagnosticKind::OperationStage {
            layer,
            stage,
            outcome,
            code,
        } = &kind
        {
            let mut events = self.events.lock().expect("diagnostic journal mutex");
            if events.len() == MAX_EVENTS {
                events.pop_front();
            }
            events.push_back(HistoryEvent {
                time_unix_ms,
                layer: (*layer).into(),
                stage: (*stage).into(),
                outcome: (*outcome).into(),
                code: code.clone(),
            });
        }
        if let Some(sink) = &self.sink {
            sink.on_event(&DiagnosticEvent {
                time_unix_ms,
                trace_id: self.trace_id.clone(),
                level: kind.level(),
                kind,
            });
        }
    }

    pub fn save_failure(&self, directory: &Path, code: &str) -> CoreResult<()> {
        self.record_stage("core", "operation", "failed", code);
        let record = FailureRecord {
            trace_id: self.trace_id.clone(),
            recorded_at_unix_ms: now_unix_ms(),
            events: self
                .events
                .lock()
                .expect("diagnostic journal mutex")
                .iter()
                .cloned()
                .collect(),
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|_| CoreError::Internal("diagnostic encoding failed".into()))?;
        let path = directory.join(FAILURE_FILE);
        // Create privately before writing; never expose the record during chmod.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options
            .open(&path)
            .map_err(|_| CoreError::Internal("diagnostic file could not be opened".into()))?;
        crate::api::config::set_private_permissions(&path)?;
        file.write_all(&bytes)
            .map_err(|_| CoreError::Internal("diagnostic file could not be written".into()))
    }
}

fn valid_stage_fields(fields: [&str; 4]) -> bool {
    fields.into_iter().all(|field| {
        !field.is_empty()
            && field.len() <= 48
            && field
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

/// Missing, corrupt, oversized and expired records do not break doctor/export.
pub fn load_failure(directory: &Path) -> Option<FailureRecord> {
    let path = directory.join(FAILURE_FILE);
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_BYTES {
        return None;
    }
    let record: FailureRecord = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let age = now_unix_ms().checked_sub(record.recorded_at_unix_ms)?;
    if !(0..=RETENTION_MS).contains(&age) {
        let _ = std::fs::remove_file(path);
        return None;
    }
    if !valid_opaque_id(&record.trace_id)
        || record.events.len() > MAX_EVENTS
        || record.events.iter().any(|event| {
            !valid_stage_fields([&event.layer, &event.stage, &event.outcome, &event.code])
        })
    {
        return None;
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_bounded_diagnostic_history_without_sensitive_details() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Arc::new(CapturingSink::default());
        let journal = Diagnostics::new(Some(sink.clone()));
        for _ in 0..100 {
            journal.record_stage("host", "query", "retrying", "unauthorized");
        }
        journal.record_stage("core", "token=secret", "failed", "secret message");
        journal.record(DiagnosticKind::HttpCompleted {
            status: 200,
            elapsed_ms: 3,
            code: None,
        });
        journal.save_failure(dir.path(), "timeout").unwrap();
        let record = load_failure(dir.path()).unwrap();
        assert_eq!(record.trace_id, journal.trace_id());
        assert_eq!(record.events.len(), MAX_EVENTS);
        assert!(record
            .events
            .iter()
            .all(|event| matches!(event.stage.as_str(), "query" | "operation")));
        assert_eq!(record.events.last().unwrap().code, "timeout");
        {
            let live = sink.0.lock().unwrap();
            assert_eq!(live.len(), 102);
            assert_eq!(live[100]["event"], "http_completed");
            assert_eq!(live[100]["level"], "debug");
            assert_eq!(live[101]["trace_id"], record.trace_id);
            assert_eq!(
                live[101]["time_unix_ms"],
                record.events.last().unwrap().time_unix_ms
            );
        }
        assert!(!std::fs::read_to_string(dir.path().join(FAILURE_FILE))
            .unwrap()
            .contains("secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join(FAILURE_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let export = dir.path().join("not-a-directory");
        std::fs::write(&export, b"x").unwrap();
        assert!(journal.save_failure(&export, "timeout").is_err());
    }

    #[test]
    fn ignores_untrusted_or_expired_history() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_failure(dir.path()).is_none());
        let path = dir.path().join(FAILURE_FILE);
        for bytes in [b"bad".to_vec(), vec![b'x'; MAX_BYTES as usize + 1]] {
            std::fs::write(&path, bytes).unwrap();
            assert!(load_failure(dir.path()).is_none());
        }
        let journal = Diagnostics::default();
        journal.save_failure(dir.path(), "timeout").unwrap();
        let mut record = load_failure(dir.path()).unwrap();
        record.trace_id = "invalid".into();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(load_failure(dir.path()).is_none());
        record.recorded_at_unix_ms = 0;
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(load_failure(dir.path()).is_none());
        assert!(!path.exists());
    }
}
