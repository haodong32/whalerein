use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{OnlineStatus, PairingDecision};

#[tokio::test]
async fn lost_host_channel_does_not_revoke_device() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    let old_channel = access.host.lock().unwrap().channel_id();
    access.host.lock().unwrap().drop_channel();

    let result = core.sync_session("session-1").await;
    assert_eq!(result.unwrap(), 1);
    assert_ne!(access.host.lock().unwrap().channel_id(), old_channel);
    assert!(core.is_paired());
    assert!(!core.runtime().is_revoked());
    assert!(!secrets.is_empty());
}

#[tokio::test]
async fn host_authorization_errors_keep_pairing_without_access_confirmation() {
    for code in [
        whalerein_proto::ErrorCode::Unauthorized,
        whalerein_proto::ErrorCode::Forbidden,
    ] {
        for unavailable in [false, true] {
            let access = FakeAccess::start().await;
            let dir = tempfile::tempdir().unwrap();
            let (core, secrets) = core(dir.path());
            core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
                .await
                .unwrap();
            core.sessions("host-1").await.unwrap();
            {
                let mut host = access.host.lock().unwrap();
                host.query_error = Some(code);
                host.fail_hosts = unavailable;
            }
            let error = core.sessions("host-1").await.unwrap_err();
            assert_eq!(error.code(), crate::api::error_code_name(code));
            assert!(core.is_paired());
            assert!(!core.runtime().is_revoked());
            assert!(!secrets.is_empty());
        }
    }
}

#[tokio::test]
async fn completes_pairing_session_control_diagnostics_and_revocation_workflow() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());

    let outcome = core
        .pair(&access.pairing_uri(), |prompt| {
            assert_eq!(prompt.host_id, "host-1");
            assert_eq!(prompt.fingerprint.len(), 64);
            PairingDecision::approve()
        })
        .await
        .unwrap();
    assert_eq!(outcome.host_id, "host-1");
    assert_eq!(outcome.access_url, access.url().as_str());
    assert!(core.is_paired());
    assert!(!secrets.is_empty());

    // Private key and token never enter configuration.
    let config = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    assert!(!config.contains("device-token"));
    assert!(!config.contains("private"));
    assert!(config.contains(&outcome.device_id));

    let hosts = core.hosts().await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].host_id, "host-1");

    let sessions = core.sessions("host-1").await.unwrap();
    assert_eq!(sessions.source, "remote");
    assert!(sessions.complete);
    assert_eq!(sessions.sessions[0].session_id, "session-1");
    assert_eq!(sessions.sessions[0].title, "test session");

    let current = core.get_connection_state("host-1");
    assert_eq!(current, ConnectionState::Connected);
    let channel = access.host.lock().unwrap().channel_id();
    assert!(channel.is_some());

    let handle = core.send_message("session-1", "hello").await.unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    assert_eq!(handle.message_id.as_deref(), Some("message-1"));
    assert!(!handle.request_id.is_empty());
    assert!(!handle.command_id.is_empty());

    let session = core.get_session("session-1").await.unwrap();
    assert_eq!(session.title, "test session");

    let approval = core.approve("approval-1", &[0xab; 32], 3).await.unwrap();
    assert_eq!(approval.status, OnlineStatus::HostAccepted);

    let report = core.doctor().unwrap();
    assert_eq!(report.address.as_deref(), Some(access.url().as_str()));
    assert_eq!(report.commands.non_terminal, 1);
    assert_eq!(report.commands.unknown, 0);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("session-1"));
    assert!(!json.contains("host-1"));

    let manifest = core
        .export_diagnostics(&dir.path().join("diagnostic.json"))
        .unwrap();
    assert!(manifest.bytes > 0);
    let bundle = std::fs::read_to_string(dir.path().join("diagnostic.json")).unwrap();
    assert!(bundle.contains("redactions"));
    assert!(!bundle.contains("hello"));
    assert!(!bundle.contains("session-1"));

    let revoked = core.revoke().await.unwrap();
    assert_eq!(revoked, outcome.device_id);
    assert!(access.host.lock().unwrap().revoked);
    assert!(!core.is_paired());
    assert!(!secrets.is_empty());
    assert!(!dir.path().join("config.json").exists());
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(matches!(core.hosts().await, Err(CoreError::NotPaired)));
}

#[tokio::test]
async fn pairing_rejection_does_not_finish_or_store_material() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let error = core
        .pair(&access.pairing_uri(), |_| PairingDecision::reject())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::PairingCancelled));
    assert!(!core.is_paired());
    assert!(!secrets.is_empty());
    assert!(!dir.path().join("config.json").exists());
}

#[tokio::test]
async fn fingerprint_mismatch_does_not_finish() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let error = core
        .pair(&access.pairing_uri(), |_| {
            PairingDecision::expect("00".repeat(32))
        })
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::FingerprintMismatch { .. }));
    assert!(!core.is_paired());
    assert!(!secrets.is_empty());
}

#[tokio::test]
async fn host_offline_is_reported_without_fabricating_success() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().host_online = false;
    let error = core.sessions("host-1").await.unwrap_err();
    assert!(matches!(error, CoreError::HostOffline));
    assert_eq!(error.exit_code(), 5);
}

#[tokio::test]
async fn second_pairing_is_a_conflict() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let error = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)));
}

#[tokio::test]
async fn unpaired_snapshot_calls_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    assert!(matches!(core.hosts().await, Err(CoreError::NotPaired)));
    assert!(matches!(
        core.sessions("host-1").await,
        Err(CoreError::NotPaired)
    ));
    assert!(matches!(
        core.send_message("session-1", "hello").await,
        Err(CoreError::NotPaired)
    ));
}

#[tokio::test]
async fn explicit_sync_delivers_typed_committed_messages() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(core.sync_session("session-1").await.unwrap(), 1);
    let key = core.session_key("session-1").unwrap();
    assert_eq!(core.cached_messages(&key).unwrap().len(), 1);
}
