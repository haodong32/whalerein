use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::OnlineStatus;
use crate::api::PairingDecision;

#[tokio::test]
async fn lost_submit_receipts_are_recoverable_after_reopening_without_another_native_action() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(matches!(
        core.send_message("session-1", "fixture").await,
        Err(CoreError::Unavailable(_))
    ));
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].host_id, "host-1");
    assert_eq!(pending[0].status, OnlineStatus::Unknown);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    assert_eq!(reopened.pending_commands().unwrap(), pending);
    access.host.lock().unwrap().drop_command_receipts = false;
    let recovered = reopened
        .recover_pending_command(&pending[0].request_id)
        .await
        .unwrap();
    assert_eq!(recovered.request_id, pending[0].request_id);
    assert_eq!(recovered.command_id, pending[0].command_id);
    assert_eq!(recovered.status, OnlineStatus::Succeeded);
    assert_eq!(recovered.message_id.as_deref(), Some("message-1"));
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(reopened.pending_commands().unwrap().is_empty());
    assert!(matches!(
        reopened.recover_pending_command("missing").await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn recovery_rejects_a_changed_host_key_before_msg3_and_preserves_the_paired_profile() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let pending = core.pending_commands().unwrap();
    let config = std::fs::read(dir.path().join("config.json")).unwrap();
    let original = {
        let mut host = access.host.lock().unwrap();
        host.drop_channel();
        std::mem::replace(
            &mut host.identity,
            crate::security::StaticIdentity::generate(),
        )
    };
    let before = access.host.lock().unwrap().request_traces.len();
    assert!(matches!(
        core.recover_pending_command(&pending[0].request_id).await,
        Err(CoreError::FingerprintMismatch { .. })
    ));
    assert_eq!(
        access.host.lock().unwrap().request_traces.len(),
        before + 1,
        "Host key must be checked before sending the device's Noise msg3"
    );
    assert_eq!(core.pending_commands().unwrap(), pending);
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        config
    );
    assert!(core.is_paired());
    assert!(!secrets.is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    {
        let mut host = access.host.lock().unwrap();
        host.identity = original;
        host.drop_channel();
    }
    assert_eq!(
        core.recover_pending_command(&pending[0].request_id)
            .await
            .unwrap()
            .status,
        OnlineStatus::Succeeded
    );
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn recovery_refuses_another_device_or_access_and_preserves_the_intent() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let cache = core.cache().unwrap();
    cache
        .record_command_intent(
            "wrong-device",
            "c1",
            "host-1",
            "another-device",
            access.url().as_str(),
            core.current_config()
                .unwrap()
                .host_fingerprint
                .as_deref()
                .unwrap(),
        )
        .unwrap();
    cache
        .record_command_intent(
            "wrong-access",
            "c2",
            "host-1",
            &paired.device_id,
            "https://another-access.test/",
            core.current_config()
                .unwrap()
                .host_fingerprint
                .as_deref()
                .unwrap(),
        )
        .unwrap();
    cache
        .record_command_intent(
            "wrong-pin",
            "c3",
            "host-1",
            &paired.device_id,
            access.url().as_str(),
            &"0".repeat(64),
        )
        .unwrap();
    for request_id in ["wrong-device", "wrong-access", "wrong-pin"] {
        assert!(matches!(
            core.recover_pending_command(request_id).await,
            Err(CoreError::Conflict(_))
        ));
    }
    assert_eq!(core.pending_commands().unwrap().len(), 3);
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    assert!(access.host.lock().unwrap().request_traces.is_empty());
}

#[tokio::test]
async fn recovery_rejects_mismatched_receipt_identity_and_domain_errors_without_erasing_intent() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let pending = core.pending_commands().unwrap();
    let entry = &pending[0];
    for field in 0..3 {
        {
            let mut host = access.host.lock().unwrap();
            let state = host.request_mut(&entry.request_id).unwrap();
            match field {
                0 => state.host_id = "wrong".into(),
                1 => state.device_id = "wrong".into(),
                _ => state.command_id = "wrong".into(),
            }
        }
        assert!(matches!(
            core.recover_pending_command(&entry.request_id).await,
            Err(CoreError::Security(_))
        ));
        assert_eq!(core.pending_commands().unwrap(), pending);
        let mut host = access.host.lock().unwrap();
        let state = host.request_mut(&entry.request_id).unwrap();
        state.host_id = entry.host_id.clone();
        state.device_id = entry.device_id.clone();
        state.command_id = entry.command_id.clone();
    }
    access.host.lock().unwrap().query_error =
        Some(whalerein_proto::ErrorCode::UnsupportedCapability);
    assert!(core
        .recover_pending_command(&entry.request_id)
        .await
        .is_err());
    assert_eq!(core.pending_commands().unwrap(), pending);
    assert!(core.is_paired());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    access.host.lock().unwrap().query_error = None;
    assert_eq!(
        core.recover_pending_command(&entry.request_id)
            .await
            .unwrap()
            .status,
        OnlineStatus::Succeeded
    );
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}
