use std::sync::{Arc, Mutex};

use whalerein_mobile::{
    write_client_config, CoreDiagnosticSink, DiagnosticEventView, MemorySecretStore,
    MobileClientService, MobileCore,
};

#[derive(Default)]
struct CapturingSink(Mutex<Vec<DiagnosticEventView>>);

impl CoreDiagnosticSink for CapturingSink {
    fn on_event(&self, event: DiagnosticEventView) {
        self.0.lock().unwrap().push(event);
    }
}

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

    // A local read produces at least one redacted core event. Delivery is
    // asynchronous, so give the dispatcher a bounded window.
    let _ = core.host_connection("host-1".into());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while sink.0.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let events = sink.0.lock().unwrap();
    assert!(!events.is_empty(), "the sink must observe core events");
    assert!(events.iter().all(|event| !event.event.is_empty()));
    assert!(events.iter().all(|event| event.level != "unknown"));
}

#[test]
fn clear_diagnostics_empties_the_core_journal() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(CapturingSink::default());
    let core = MobileCore::new_with_diagnostic_sink(
        write_config(dir.path()),
        MemorySecretStore::new(),
        "test-app".to_owned(),
        sink,
    )
    .expect("create MobileCore with diagnostic sink");

    core.clear_diagnostics().expect("clear diagnostics");
    // Clearing is idempotent and never changes the facade behaviour.
    core.clear_diagnostics().expect("clear diagnostics twice");
    assert!(core.host_connection("host-1".into()).is_none());
}

#[test]
fn panicking_diagnostic_sink_never_changes_core_results() {
    struct PanickingSink;
    impl CoreDiagnosticSink for PanickingSink {
        fn on_event(&self, _event: DiagnosticEventView) {
            panic!("sink failure must stay outside Core");
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let core = MobileCore::new_with_diagnostic_sink(
        write_config(dir.path()),
        MemorySecretStore::new(),
        "test-app".to_owned(),
        Arc::new(PanickingSink),
    )
    .expect("create MobileCore with panicking sink");
    assert!(core.host_connection("host-1".into()).is_none());
}
