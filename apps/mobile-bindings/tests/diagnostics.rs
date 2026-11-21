//! Behavioral tests for the Core diagnostic bridge exposed by the binding.

use std::sync::{Arc, Mutex};

use whalerein_mobile::{
    write_client_config, DiagnosticEventView, MemorySecretStore, MobileClientService, MobileCore,
    MobileDiagnosticSink,
};

fn write_config(dir: &std::path::Path) -> String {
    let config = dir.join("config.toml").to_string_lossy().into_owned();
    write_client_config(
        config.clone(),
        MobileClientService::Standalone,
        "instance".into(),
        None,
        None,
    )
    .expect("write client config");
    config
}

#[derive(Default)]
struct CapturingSink {
    events: Mutex<Vec<DiagnosticEventView>>,
}

impl MobileDiagnosticSink for CapturingSink {
    fn on_event(&self, event: DiagnosticEventView) {
        self.events.lock().unwrap().push(event);
    }
}

struct PanickingSink;

impl MobileDiagnosticSink for PanickingSink {
    fn on_event(&self, _event: DiagnosticEventView) {
        panic!("foreign diagnostic sink panic");
    }
}

#[test]
fn diagnostic_sink_observes_core_events_with_stable_identifiers() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(CapturingSink::default());
    let core = MobileCore::new_with_diagnostic_sink(
        write_config(dir.path()),
        MemorySecretStore::new(),
        "test-app".to_owned(),
        sink.clone(),
    )
    .expect("create MobileCore with diagnostic sink");

    let events = sink.events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "Core must report at least its configuration event"
    );
    let configuration = events
        .iter()
        .find(|event| event.event == "configuration_loaded")
        .expect("configuration_loaded event");
    assert_eq!(configuration.level, "info");
    assert_eq!(configuration.sequence, 0);
    assert!(!configuration.trace_id.is_empty());
    assert!(configuration.time_unix_ms > 0);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.sequence, index as u64);
    }
    drop(events);

    // The sink does not change the normal facade behavior.
    assert_eq!(core.connection_state("host-1".into()), "disconnected");
}

#[test]
fn panicking_diagnostic_sink_never_changes_core_results() {
    let dir = tempfile::tempdir().unwrap();
    let core = MobileCore::new_with_diagnostic_sink(
        write_config(dir.path()),
        MemorySecretStore::new(),
        "test-app".to_owned(),
        Arc::new(PanickingSink),
    )
    .expect("a panicking sink must not fail construction");

    // The process is alive and every operation keeps its ordinary typed result.
    let error = core.hosts().unwrap_err();
    assert_eq!(error.code(), "not_paired");
    assert_eq!(core.connection_state("host-1".into()), "disconnected");
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
}
