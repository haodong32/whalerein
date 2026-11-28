use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{DiagnosticExportMode, DiagnosticExportTarget, PairingDecision, SessionSummary};

#[tokio::test]
async fn doctor_export_omits_process_state() {
    let access = FakeAccess::start().await;
    access.host.lock().unwrap().host_id = "fixture-host-9c2".to_owned();
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let report = core.doctor().unwrap();
    assert!(report.address.is_some());
    let path = dir.path().join("diagnostic.json");
    let manifest = core.export_diagnostics(&path).unwrap();
    assert_eq!(manifest.fields, ExportManifest::fields());
    assert_eq!(manifest.redactions, ExportManifest::redactions());
    let bundle = std::fs::read_to_string(path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&bundle).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert_eq!(value["capture_id"], manifest.capture_id);
    assert_eq!(value["captured_at_unix_ms"], manifest.capture_time_unix_ms);
    // Only ordinal Host labels leave Core; the configured Host and device
    // identities, message content and the local directory stay out.
    let host_id = core.require_config().unwrap().host_id;
    assert!(!host_id.is_empty());
    assert!(!bundle.contains(&host_id));
    assert!(!bundle.contains(&core.require_config().unwrap().device_id));
    assert!(!bundle.contains(dir.path().to_string_lossy().as_ref()));
    assert_eq!(
        value["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|host| host["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["host-1"]
    );
}

#[tokio::test]
async fn diagnostics_preview_is_redacted_json_and_writes_no_file() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();

    let draft_marker = "PREVIEW-DRAFT-MARKER-91ad";
    let message_marker = "PREVIEW-MESSAGE-MARKER-4c7b";
    core.save_draft("host-1", "session-1", draft_marker)
        .unwrap();
    core.cache()
        .unwrap()
        .put_sessions(
            "host-1",
            &[SessionSummary::from(whalerein_proto::SessionSummary {
                session_id: "session-1".into(),
                title: message_marker.into(),
                ..Default::default()
            })],
            "remote",
            true,
        )
        .unwrap();
    core.save_failure("timeout").unwrap();
    core.runtime()
        .set_state("host-1", ConnectionState::Connecting)
        .unwrap();

    let before = directory_names(dir.path());
    let snapshot = core.diagnostics_snapshot().unwrap();
    let preview = core.diagnostics_preview().unwrap();
    assert_eq!(preview.as_bytes(), snapshot.bytes);
    assert_eq!(directory_names(dir.path()), before);

    let value: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert!(!value["core_version"].as_str().unwrap().is_empty());
    assert_eq!(value["protocol_version"], 1);
    assert_eq!(value["capture_id"], snapshot.capture_id);
    assert!(value["captured_at_unix_ms"].is_number());
    assert_eq!(value["report"]["address"], core.access_address().unwrap());
    assert!(value["report"]["commands"]["non_terminal"].is_number());
    assert_eq!(value["hosts"][0]["label"], "host-1");
    assert_eq!(value["hosts"][0]["connection_state"], "connecting");
    assert_eq!(value["last_failure"]["code"], "timeout");
    assert!(!value["last_failure"]["trace_id"]
        .as_str()
        .unwrap()
        .is_empty());
    assert!(!value["last_failure"]["events"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(value["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|sample| { sample["name"] == "whalerein_connection_transitions_total" }));
    assert_eq!(
        value["fields"],
        serde_json::to_value(ExportManifest::fields()).unwrap()
    );
    assert_eq!(
        value["redactions"],
        serde_json::to_value(ExportManifest::redactions()).unwrap()
    );
    assert!(snapshot.summary.contains("Last failure: timeout"));
    assert!(snapshot.summary.contains("host-1: connecting"));

    let device_id = core.require_config().unwrap().device_id;
    let store: std::sync::Arc<dyn crate::api::SecretStore> = secrets.clone();
    let token = crate::security::load_device_token(&store, &device_id)
        .unwrap()
        .unwrap();
    let private_key = store
        .load(&crate::security::keys::device_private_key(&device_id))
        .unwrap()
        .unwrap();
    for secret in [
        draft_marker.to_owned(),
        message_marker.to_owned(),
        token,
        hex::encode(private_key),
        access.pairing_uri(),
        "whalerein://pair".to_owned(),
    ] {
        assert!(!preview.contains(&secret), "preview leaked secrets");
    }
    assert!(!preview.contains("device_id"));
    assert!(!preview.contains("host_id"));
    assert!(!preview.contains(dir.path().to_string_lossy().as_ref()));
}

#[tokio::test]
async fn diagnostics_preview_without_a_profile_has_no_hosts_or_versions() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let preview = core.diagnostics_preview().unwrap();
    let value: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert!(value["protocol_version"].is_null());
    assert!(value["report"]["address"].is_null());
    assert_eq!(value["hosts"].as_array().unwrap().len(), 0);
    assert!(value["last_failure"].is_null());
    assert!(value["capture_id"].as_str().unwrap().len() >= 16);
    assert!(!dir.path().join("cache.sqlite3").exists());
}

#[tokio::test]
async fn one_capture_is_shared_by_preview_and_export_and_refresh_is_explicit() {
    let access = FakeAccess::start().await;
    access.host.lock().unwrap().host_id = "fixture-host-a71".to_owned();
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let host_id = core.require_config().unwrap().host_id;
    core.runtime()
        .set_state(&host_id, ConnectionState::Connecting)
        .unwrap();
    core.runtime()
        .set_state(&host_id, ConnectionState::Connected)
        .unwrap();

    let captured = core.diagnostics_snapshot().unwrap();
    // The local label-to-Host mapping keeps report order and never enters the
    // shareable bytes or views.
    assert_eq!(captured.hosts.len(), 1);
    assert_eq!(captured.hosts[0].label, "host-1");
    assert_eq!(captured.hosts[0].host_ref, host_id);
    assert!(!captured.text.contains(&host_id));
    assert!(!captured.summary.contains(&host_id));
    assert!(!String::from_utf8_lossy(&captured.bytes).contains(&host_id));
    assert!(!format!("{captured:?}").contains(&host_id));
    let document: serde_json::Value = serde_json::from_slice(&captured.bytes).unwrap();
    assert_eq!(
        document["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|host| host["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        captured
            .hosts
            .iter()
            .map(|host| host.label.as_str())
            .collect::<Vec<_>>()
    );
    assert!(document["hosts"][0].get("host_ref").is_none());
    let preview = core.diagnostics_preview().unwrap();
    assert_eq!(preview.as_bytes(), captured.bytes.as_slice());

    let path = dir.path().join("exported.json");
    let manifest = core
        .write_diagnostics_snapshot(&captured, &path, DiagnosticExportMode::CreateNew)
        .unwrap();
    assert_eq!(manifest.capture_id, captured.capture_id);
    assert_eq!(manifest.capture_time_unix_ms, captured.capture_time_unix_ms);
    assert_eq!(std::fs::read(&path).unwrap(), captured.bytes);
    assert_eq!(manifest.bytes, captured.bytes.len() as u64);

    // State changes after the preview do not rewrite the captured document.
    core.runtime()
        .set_state(&host_id, ConnectionState::Reconnecting)
        .unwrap();
    core.save_failure("timeout").unwrap();
    let unchanged = core.diagnostics_snapshot().unwrap();
    assert_eq!(unchanged.capture_id, captured.capture_id);
    assert_eq!(unchanged.bytes, captured.bytes);
    assert!(unchanged.text.contains("\"connected\""));

    // Only an explicit refresh creates a new capture.
    let refreshed = core.refresh_diagnostics().unwrap();
    assert_ne!(refreshed.capture_id, captured.capture_id);
    assert_ne!(refreshed.capture_time_unix_ms, i64::MIN);
    assert!(refreshed.text.contains("\"reconnecting\""));
    let after_refresh = core.diagnostics_snapshot().unwrap();
    assert_eq!(after_refresh.bytes, refreshed.bytes);
}

#[tokio::test]
async fn failed_capture_returns_an_error_and_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let captured = core.diagnostics_snapshot().unwrap();
    assert!(!captured.bytes.is_empty());

    core.force_diagnostic_capture_failure_for_tests(true);
    // A failed refresh drops the previous capture so it cannot be shared.
    assert!(core.refresh_diagnostics().is_err());
    assert!(core.diagnostics_snapshot().is_err());
    assert!(core.diagnostics_preview().is_err());

    let path = dir.path().join("must-not-exist.json");
    assert!(core.export_diagnostics(&path).is_err());
    assert!(!path.exists());

    core.force_diagnostic_capture_failure_for_tests(false);
    let recovered = core.diagnostics_snapshot().unwrap();
    assert!(!recovered.bytes.is_empty());
}

#[tokio::test]
async fn export_refuses_a_symlink_or_directory_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let snapshot = core.diagnostics_snapshot().unwrap();

    let directory = dir.path().join("reports");
    std::fs::create_dir(&directory).unwrap();
    assert_eq!(
        core.diagnostic_export_target(&directory)
            .unwrap_err()
            .code(),
        "invalid_argument"
    );
    assert!(core
        .write_diagnostics_snapshot(&snapshot, &directory, DiagnosticExportMode::ReplaceExisting)
        .is_err());
    assert!(directory.is_dir());

    #[cfg(unix)]
    {
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let link = dir.path().join("report.json");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert_eq!(
            core.diagnostic_export_target(&link).unwrap_err().code(),
            "invalid_argument"
        );
        assert!(core.export_diagnostics(&link).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    }
}

#[tokio::test]
async fn a_file_appearing_after_the_new_check_survives_exclusive_creation() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let snapshot = core.diagnostics_snapshot().unwrap();
    let path = dir.path().join("race.json");
    assert_eq!(
        core.diagnostic_export_target(&path).unwrap(),
        DiagnosticExportTarget::New
    );

    // Simulate a racing writer between the CLI target check and the write.
    std::fs::write(&path, b"racing writer").unwrap();
    let error = core
        .write_diagnostics_snapshot(&snapshot, &path, DiagnosticExportMode::CreateNew)
        .unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert_eq!(std::fs::read(&path).unwrap(), b"racing writer");

    // Explicit replacement consent is the only path that replaces it.
    core.write_diagnostics_snapshot(&snapshot, &path, DiagnosticExportMode::ReplaceExisting)
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), snapshot.bytes);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_staged_replace_preserves_the_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let snapshot = core.diagnostics_snapshot().unwrap();
    let path = dir.path().join("report.json");
    std::fs::write(&path, b"prior contents").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

    let error = core
        .write_diagnostics_snapshot(&snapshot, &path, DiagnosticExportMode::ReplaceExisting)
        .unwrap_err();
    assert_eq!(error.code(), "internal");
    assert_eq!(std::fs::read(&path).unwrap(), b"prior contents");

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    core.write_diagnostics_snapshot(&snapshot, &path, DiagnosticExportMode::ReplaceExisting)
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), snapshot.bytes);
}

#[cfg(unix)]
#[tokio::test]
async fn new_and_replaced_diagnostic_files_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let snapshot = core.diagnostics_snapshot().unwrap();

    let fresh = dir.path().join("fresh.json");
    core.write_diagnostics_snapshot(&snapshot, &fresh, DiagnosticExportMode::CreateNew)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let existing = dir.path().join("existing.json");
    std::fs::write(&existing, b"old").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
    core.write_diagnostics_snapshot(&snapshot, &existing, DiagnosticExportMode::ReplaceExisting)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn diagnostic_export_manifest_is_unchanged() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let path = dir.path().join("diagnostic.json");
    let manifest = core.export_diagnostics(&path).unwrap();
    assert_eq!(manifest.fields, ExportManifest::fields());
    assert_eq!(manifest.redactions, ExportManifest::redactions());
    assert!(manifest.bytes > 0);
    assert_eq!(manifest.path, path.display().to_string());
    assert!(!manifest.capture_id.is_empty());
    assert!(path.exists());
    let snapshot = core.diagnostics_snapshot().unwrap();
    assert_eq!(manifest.capture_id, snapshot.capture_id);
    assert_eq!(std::fs::read(&path).unwrap(), snapshot.bytes);
}

#[tokio::test]
async fn cache_rebuild_diagnostic_can_read_the_published_cache() {
    use crate::api::{DiagnosticEvent, MemorySecretStore};
    use std::sync::atomic::Ordering;
    use std::sync::{OnceLock, Weak};

    #[derive(Default)]
    struct CacheSink {
        core: OnceLock<Weak<Core>>,
        observed: AtomicBool,
    }

    impl DiagnosticSink for CacheSink {
        fn on_event(&self, event: &DiagnosticEvent) {
            if matches!(
                event.kind,
                DiagnosticKind::OperationStage {
                    stage: "cache_rebuild",
                    ..
                }
            ) {
                let core = self.core.get().unwrap().upgrade().unwrap();
                assert!(
                    core.cache.try_lock().is_ok(),
                    "callback ran with the cache lock held"
                );
                assert!(core.pending_commands().unwrap().is_empty());
                self.observed.store(true, Ordering::Relaxed);
            }
        }
    }

    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("client.toml");
    Core::write_config(
        &config,
        &ClientConfig::new(ClientService::Standalone, "instance"),
    )
    .unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let paired = Core::open(&config, secrets.clone()).unwrap();
    paired
        .connect_standalone(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let cache_path = paired.options.data_dir.join("cache.sqlite3");
    drop(paired);
    std::fs::write(cache_path, b"invalid SQLite header").unwrap();

    let sink = Arc::new(CacheSink::default());
    let reopened =
        Arc::new(Core::open_with_diagnostic_sink(&config, secrets, sink.clone()).unwrap());
    sink.core.set(Arc::downgrade(&reopened)).unwrap();
    assert!(reopened.pending_commands().unwrap().is_empty());
    assert!(sink.observed.load(Ordering::Relaxed));
}

#[tokio::test]
async fn an_unopenable_cache_produces_a_degraded_doctor_report_instead_of_failing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    drop(core);
    // A cache path that cannot be opened as a database, and cannot be
    // quarantined/rebuild as file-level corruption either, so the degraded
    // report path is exercised deterministically.
    let cache_path = dir.path().join("cache.sqlite3");
    std::fs::remove_file(&cache_path).ok();
    std::fs::create_dir(&cache_path).unwrap();

    let reopened = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let (report, warning) = reopened.doctor_with_cache_status().unwrap();
    assert_eq!(warning, Some("cache_unavailable"));
    assert_eq!(report.commands.non_terminal, 0);
    assert_eq!(report.commands.unknown, 0);
    assert!(report.address.is_some());
    // `doctor` remains usable without the richer status API.
    assert!(reopened.doctor().is_ok());
    // A command that genuinely needs the cache still fails explicitly.
    assert_eq!(
        reopened.pending_commands().unwrap_err().code(),
        "cache_error"
    );

    // Diagnostics that render the report carry the warning explicitly.
    let preview: serde_json::Value =
        serde_json::from_str(&reopened.diagnostics_preview().unwrap()).unwrap();
    assert_eq!(preview["cache_warning"], "cache_unavailable");
    let path = dir.path().join("degraded.json");
    reopened.export_diagnostics(&path).unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(bundle["cache_warning"], "cache_unavailable");
    assert_eq!(bundle["report"]["commands"]["non_terminal"], 0);
}

#[cfg(unix)]
#[tokio::test]
async fn diagnostic_export_refuses_a_symlink_without_touching_its_target() {
    use std::os::unix::fs::PermissionsExt;

    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let target = dir.path().join("victim.txt");
    std::fs::write(&target, b"keep me").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = dir.path().join("diagnostic-link.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let error = core.export_diagnostics(&link).unwrap_err();
    assert_eq!(error.code(), "invalid_argument");
    assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644,
        "the symlink target permissions must stay untouched"
    );
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

fn directory_names(path: &std::path::Path) -> Vec<String> {
    let mut names = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}
