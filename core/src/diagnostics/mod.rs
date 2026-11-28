//! Redacted diagnostics, one immutable capture, detail mode and aggregate metrics.

pub mod export;
pub mod journal;

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::api::{
    CommandDiagnostics, CoreError, CoreResult, DiagnosticsHost, DiagnosticsSnapshot, DoctorReport,
};
use crate::cache::Cache;

/// In-memory metrics with bounded label values. Identifiers are never labels.
#[derive(Debug, Default)]
pub struct Metrics {
    values: Mutex<BTreeMap<(String, String), u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricSample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    fn increment(&self, name: &str, label: &str, value: &str) {
        let mut values = self.values.lock().expect("metrics mutex");
        *values
            .entry((name.to_owned(), format!("{label}={value}")))
            .or_insert(0) += 1;
    }

    pub fn record_connection(&self, state: &str) {
        self.increment("whalerein_connection_transitions_total", "state", state);
    }

    pub fn record_reconnect(&self) {
        self.increment("whalerein_reconnects_total", "kind", "scheduled");
    }

    pub fn record_error_code(&self, code: &str) {
        self.increment("whalerein_errors_total", "code", code);
    }

    #[cfg(test)]
    pub fn record_sync(&self, state: &str) {
        self.increment("whalerein_sync_transitions_total", "state", state);
    }

    #[cfg(test)]
    pub fn record_command_status(&self, status: &str) {
        self.increment("whalerein_command_status_total", "status", status);
    }

    pub fn snapshot(&self) -> Vec<MetricSample> {
        let values = self.values.lock().expect("metrics mutex");
        values
            .iter()
            .map(|((name, labels), value)| {
                let mut label_map = BTreeMap::new();
                if let Some((key, value)) = labels.split_once('=') {
                    label_map.insert(key.to_owned(), value.to_owned());
                }
                MetricSample {
                    name: name.clone(),
                    labels: label_map,
                    value: *value,
                }
            })
            .collect()
    }
}

/// Build the redacted doctor snapshot from disk. No host or device identifiers
/// are included. Process-local connection state is not part of the snapshot.
pub fn build_report(cache: &Cache, address: Option<String>) -> DoctorReport {
    let (non_terminal, unknown) = cache.command_counts().unwrap_or((0, 0));
    DoctorReport {
        schema: "whalerein/v1".into(),
        address,
        commands: CommandDiagnostics {
            non_terminal,
            unknown,
        },
    }
}

/// One authorized Host in a preview, identified only by a local ordinal.
///
/// `host_ref` joins this preview row to the local profile so an authorized
/// client can look up a directory name. It is skipped by serialization and
/// never reaches the captured document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewHost {
    pub label: String,
    #[serde(skip_serializing, default)]
    pub host_ref: String,
    pub connection_state: String,
}

/// Allowlisted facts read once when a diagnostic capture is taken.
///
/// The capture owns everything it needs; later process state cannot change the
/// serialized bytes.
pub struct CaptureInput<'a> {
    pub report: &'a DoctorReport,
    pub cache_warning: Option<&'static str>,
    pub failure: Option<&'a journal::FailureRecord>,
    pub hosts: &'a [PreviewHost],
    pub metrics: &'a [MetricSample],
    pub protocol_version: Option<u32>,
}

/// Maximum number of Host rows serialized into one capture.
const MAX_CAPTURE_HOSTS: usize = 32;
/// Maximum number of metric samples serialized into one capture.
const MAX_CAPTURE_METRICS: usize = 64;
/// Upper bound for the rendered preview summary, in characters.
const MAX_SUMMARY_CHARS: usize = 4096;
/// Bound for one summary value rendered into the preview.
const MAX_SUMMARY_VALUE_CHARS: usize = 160;

/// Serialize one immutable, redacted diagnostic capture from allowlisted facts.
///
/// This is the only report builder. Preview, save and share consume the
/// returned bytes; no caller receives a second projection of the same facts.
/// The document contains the schema/version, capture time and safe correlation
/// IDs, the Access address already exposed by doctor, command counts, ordinal
/// Host labels with in-process connection state, aggregate metrics, retained
/// failure stages and the field/redaction manifest. It never contains message
/// or draft text, tool parameters, credentials, invitation or device tokens,
/// private keys, directory names or Host/device identities.
pub fn capture(input: CaptureInput<'_>) -> CoreResult<DiagnosticsSnapshot> {
    let fields = crate::api::ExportManifest::fields();
    let redactions = crate::api::ExportManifest::redactions();
    let capture_id = crate::util::random_id(16);
    let capture_time_unix_ms = crate::util::now_unix_ms();
    let last_failure = input.failure.map(|record| {
        serde_json::json!({
            "trace_id": record.trace_id,
            "recorded_at_unix_ms": record.recorded_at_unix_ms,
            "code": record.events.last().map(|event| event.code.clone()),
            "events": record.events,
        })
    });
    let hosts = input
        .hosts
        .iter()
        .take(MAX_CAPTURE_HOSTS)
        .collect::<Vec<_>>();
    let host_mapping = input
        .hosts
        .iter()
        .take(MAX_CAPTURE_HOSTS)
        .map(|host| DiagnosticsHost {
            label: host.label.clone(),
            host_ref: host.host_ref.clone(),
            connection_state: host.connection_state.clone(),
        })
        .collect::<Vec<_>>();
    let metrics = input
        .metrics
        .iter()
        .take(MAX_CAPTURE_METRICS)
        .collect::<Vec<_>>();
    let document = serde_json::json!({
        "schema": input.report.schema,
        "core_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": input.protocol_version,
        "capture_id": capture_id,
        "captured_at_unix_ms": capture_time_unix_ms,
        "report": input.report,
        "cache_warning": input.cache_warning,
        "hosts": hosts,
        "metrics": metrics,
        "last_failure": last_failure,
        "fields": fields,
        "redactions": redactions,
    });
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|_| CoreError::Internal("diagnostic capture could not encode".into()))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| CoreError::Internal("diagnostic capture was not valid UTF-8".into()))?
        .to_owned();
    let summary = summarize(&bytes);
    Ok(DiagnosticsSnapshot {
        schema: input.report.schema.clone(),
        capture_id,
        capture_time_unix_ms,
        bytes,
        text,
        summary,
        redaction_fields: redactions,
        last_failure_code: input
            .failure
            .and_then(|record| record.events.last().map(|event| event.code.clone())),
        hosts: host_mapping,
    })
}

/// Render a bounded summary from the captured bytes, never from live state.
fn summarize(bytes: &[u8]) -> String {
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return "Diagnostic snapshot: the captured report could not be summarized.\n".into();
    };
    let mut out = String::new();
    let capture_id = document["capture_id"].as_str().unwrap_or("unknown");
    let captured_at = document["captured_at_unix_ms"]
        .as_i64()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    let schema = document["schema"].as_str().unwrap_or("whalerein/v1");
    let core_version = document["core_version"].as_str().unwrap_or("unknown");
    let protocol = document["protocol_version"]
        .as_u64()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    out.push_str(&format!("Diagnostic snapshot {capture_id}\n"));
    out.push_str(&format!(
        "Captured: {captured_at} (schema {schema}; Core {core_version}; protocol {protocol})\n"
    ));
    let address = document["report"]["address"]
        .as_str()
        .map(display_value)
        .unwrap_or_else(|| "(not paired)".into());
    out.push_str(&format!("Address: {address}\n"));
    let non_terminal = document["report"]["commands"]["non_terminal"]
        .as_u64()
        .unwrap_or(0);
    let unknown = document["report"]["commands"]["unknown"]
        .as_u64()
        .unwrap_or(0);
    out.push_str(&format!(
        "Commands: {non_terminal} unfinished, {unknown} unknown\n"
    ));
    if let Some(warning) = document["cache_warning"].as_str() {
        out.push_str(&format!(
            "Cache: {} (unfinished-command counts may be incomplete; cache-backed commands fail until it is rebuilt)\n",
            display_value(warning)
        ));
    }
    let hosts = document["hosts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    out.push_str(&format!("Hosts: {}\n", hosts.len()));
    for host in hosts {
        let label = host["label"].as_str().unwrap_or("host");
        let state = host["connection_state"].as_str().unwrap_or("unknown");
        out.push_str(&format!(
            "  - {}: {}\n",
            display_value(label),
            display_value(state)
        ));
    }
    let metric_count = document["metrics"].as_array().map(Vec::len).unwrap_or(0);
    out.push_str(&format!("Metrics: {metric_count} samples\n"));
    match document["last_failure"].as_object() {
        Some(failure) => {
            let code = failure["code"].as_str().unwrap_or("unknown");
            let trace = failure["trace_id"].as_str().unwrap_or("unknown");
            let stages = failure["events"].as_array().map(Vec::len).unwrap_or(0);
            out.push_str(&format!(
                "Last failure: {} (trace {}, {stages} stages)\n",
                display_value(code),
                display_value(trace)
            ));
            for event in failure["events"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let layer = event["layer"].as_str().unwrap_or("?");
                let stage = event["stage"].as_str().unwrap_or("?");
                let outcome = event["outcome"].as_str().unwrap_or("?");
                let code = event["code"].as_str().unwrap_or("?");
                out.push_str(&format!(
                    "  - {}/{}/{} ({})\n",
                    display_value(layer),
                    display_value(stage),
                    display_value(outcome),
                    display_value(code)
                ));
            }
        }
        None => out.push_str("Last failure: none\n"),
    }
    let fields = document["fields"].as_array().map(Vec::len).unwrap_or(0);
    let redactions = document["redactions"].as_array().map(Vec::len).unwrap_or(0);
    out.push_str(&format!(
        "Manifest: {fields} allowlisted fields, {redactions} redactions\n"
    ));
    if out.chars().count() > MAX_SUMMARY_CHARS {
        out = out.chars().take(MAX_SUMMARY_CHARS).collect();
        out.push_str("\n[summary truncated; the full report view has the captured bytes]\n");
    }
    out
}

/// Keep a terminal summary one bounded, control-free line per value.
fn display_value(value: &str) -> String {
    let mut bounded = value
        .chars()
        .take(MAX_SUMMARY_VALUE_CHARS)
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect::<String>();
    if value.chars().count() > MAX_SUMMARY_VALUE_CHARS {
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostInfo;

    fn cache() -> Cache {
        let cache = Cache::open_in_memory([1u8; 32]).unwrap();
        cache
            .put_hosts(&[HostInfo {
                host_id: "h-1".into(),
                online: true,
                protocol_version: 1,
                fingerprint: "f".into(),
            }])
            .unwrap();
        cache
    }

    #[test]
    fn metrics_labels_never_use_ids() {
        let metrics = Metrics::new();
        metrics.record_connection("connected");
        metrics.record_connection("connected");
        metrics.record_error_code("payload_digest_mismatch");
        metrics.record_reconnect();
        metrics.record_sync("synced");
        metrics.record_command_status("host_accepted");
        let samples = metrics.snapshot();
        assert!(!samples.is_empty());
        for sample in &samples {
            for value in sample.labels.values() {
                assert_ne!(value, "h-1");
                assert_ne!(value, "d-1");
                assert!(!value.contains("session"));
            }
        }
        let connections = samples
            .iter()
            .find(|sample| sample.name == "whalerein_connection_transitions_total")
            .unwrap();
        assert_eq!(connections.value, 2);
        assert_eq!(connections.labels.get("state").unwrap(), "connected");
    }

    #[test]
    fn report_contains_only_redacted_fields() {
        let cache = cache();
        cache
            .put_command("r-1", "c-1", crate::api::OnlineStatus::HostAccepted, None)
            .unwrap();
        cache
            .put_command("r-2", "c-2", crate::api::OnlineStatus::Unknown, None)
            .unwrap();
        let report = build_report(&cache, Some("https://access.test".into()));
        assert_eq!(report.address.as_deref(), Some("https://access.test"));
        assert_eq!(report.commands.non_terminal, 1);
        assert_eq!(report.commands.unknown, 1);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("h-1"));
        assert!(!json.contains("c-1"));
        assert!(!json.contains("connection"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn capture_is_the_only_builder_and_serializes_allowlisted_facts() {
        let report = build_report(&cache(), Some("https://access.test".into()));
        let failure = journal::FailureRecord {
            trace_id: "a".repeat(32),
            recorded_at_unix_ms: 7,
            events: vec![journal::HistoryEvent {
                time_unix_ms: 7,
                layer: "host".into(),
                stage: "query".into(),
                outcome: "failed".into(),
                code: "timeout".into(),
            }],
        };
        let hosts = vec![PreviewHost {
            label: "host-1".into(),
            host_ref: "local-host-ref-7d1".into(),
            connection_state: "connected".into(),
        }];
        let snapshot = capture(CaptureInput {
            report: &report,
            cache_warning: None,
            failure: Some(&failure),
            hosts: &hosts,
            metrics: &[],
            protocol_version: Some(1),
        })
        .unwrap();
        assert_eq!(snapshot.schema, "whalerein/v1");
        assert_eq!(snapshot.bytes, snapshot.text.as_bytes());
        assert_eq!(snapshot.last_failure_code.as_deref(), Some("timeout"));
        assert!(snapshot.text.contains("\"capture_id\""));
        assert!(snapshot.text.contains("\"captured_at_unix_ms\""));
        assert!(snapshot.text.contains("\"events\""));
        assert_eq!(
            snapshot.redaction_fields,
            crate::api::ExportManifest::redactions()
        );
        // The bounded summary is rendered from the same captured bytes.
        assert!(snapshot.summary.contains(&snapshot.capture_id));
        assert!(snapshot.summary.contains("https://access.test"));
        assert!(snapshot.summary.contains("host-1: connected"));
        assert!(snapshot.summary.contains("Last failure: timeout"));
        // The local Host join key never enters the captured document or its
        // rendered views; only the anonymous label does.
        assert_eq!(snapshot.hosts.len(), 1);
        assert_eq!(snapshot.hosts[0].label, "host-1");
        assert_eq!(snapshot.hosts[0].host_ref, "local-host-ref-7d1");
        assert!(!snapshot.text.contains("local-host-ref-7d1"));
        assert!(!std::str::from_utf8(&snapshot.bytes)
            .unwrap()
            .contains("local-host-ref-7d1"));
        assert!(!snapshot.summary.contains("local-host-ref-7d1"));
        assert!(!format!("{:?}", snapshot).contains("local-host-ref-7d1"));
        let document: serde_json::Value = serde_json::from_slice(&snapshot.bytes).unwrap();
        assert_eq!(
            document["fields"],
            serde_json::to_value(crate::api::ExportManifest::fields()).unwrap()
        );
        assert_eq!(
            document["redactions"],
            serde_json::to_value(crate::api::ExportManifest::redactions()).unwrap()
        );
    }

    #[test]
    fn capture_summary_stays_bounded_and_never_echoes_untrusted_controls() {
        let report = build_report(&cache(), None);
        let hosts = (0..MAX_CAPTURE_HOSTS + 5)
            .map(|index| PreviewHost {
                label: format!("host-{index}\u{1b}[31m"),
                host_ref: format!("local-ref-{index}"),
                connection_state: "connected".into(),
            })
            .collect::<Vec<_>>();
        let snapshot = capture(CaptureInput {
            report: &report,
            cache_warning: None,
            failure: None,
            hosts: &hosts,
            metrics: &[],
            protocol_version: None,
        })
        .unwrap();
        assert!(snapshot.summary.chars().count() <= MAX_SUMMARY_CHARS + 100);
        assert!(!snapshot.summary.contains('\u{1b}'));
        let document: serde_json::Value = serde_json::from_slice(&snapshot.bytes).unwrap();
        assert_eq!(
            document["hosts"].as_array().unwrap().len(),
            MAX_CAPTURE_HOSTS
        );
    }
}
