use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{PairingDecision, SessionSummary};

#[tokio::test]
async fn doctor_export_omits_process_state() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let report = core.doctor().unwrap();
    assert!(report.address.is_some());
    let path = dir.path().join("diagnostic.json");
    let manifest = core.export_diagnostics(&path).unwrap();
    assert_eq!(
        manifest.fields,
        vec![
            "address".to_owned(),
            "commands.non_terminal".to_owned(),
            "commands.unknown".to_owned(),
            "last_failure".to_owned(),
        ]
    );
    let bundle = std::fs::read_to_string(path).unwrap();
    assert!(!bundle.contains("handshake"));
    assert!(!bundle.contains("reconnect"));
    assert!(!bundle.contains("host-1"));
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
    let preview = core.diagnostics_preview().unwrap();
    assert_eq!(directory_names(dir.path()), before);

    let value: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert!(!value["core_version"].as_str().unwrap().is_empty());
    assert_eq!(value["protocol_version"], 1);
    assert_eq!(value["report"]["address"], core.access_address().unwrap());
    assert!(value["report"]["commands"]["non_terminal"].is_number());
    assert_eq!(value["hosts"][0]["label"], "host-1");
    assert_eq!(value["hosts"][0]["connection_state"], "connecting");
    assert_eq!(value["last_failure"]["code"], "timeout");
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
    assert!(!dir.path().join("cache.sqlite3").exists());
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
    assert!(path.exists());
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
