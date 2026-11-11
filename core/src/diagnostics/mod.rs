//! Redacted diagnostics, detail mode and aggregate metrics.

pub mod journal;

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::api::{CommandDiagnostics, DoctorReport, ExportManifest};
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

pub fn render_export(report: &DoctorReport) -> ExportManifest {
    ExportManifest {
        schema: report.schema.clone(),
        path: String::new(),
        fields: ExportManifest::fields(),
        redactions: ExportManifest::redactions(),
        bytes: 0,
    }
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
    fn export_manifest_lists_redactions() {
        let report = build_report(&cache(), None);
        let manifest = render_export(&report);
        assert!(manifest.redactions.contains(&"private keys".into()));
        assert_eq!(
            manifest.fields,
            vec![
                "address".to_owned(),
                "commands.non_terminal".to_owned(),
                "commands.unknown".to_owned(),
                "last_failure".to_owned(),
            ]
        );
    }
}
