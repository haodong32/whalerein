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
    let recovered = core
        .recover_pending_command(&entry.request_id)
        .await
        .unwrap();
    assert_eq!(recovered.status, OnlineStatus::Succeeded);
    assert_eq!(recovered.delivery_id.as_deref(), Some("delivery-1"));
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn lost_model_receipt_recovers_original_effect_without_resubmission() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core
        .select_session_model("session-1", "provider", "model", Some("high"))
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.drop_command_receipts = false;
        host.model_observed = Some("later-local-model".into());
    }
    let observed = reopened.read_session_model("session-1").await.unwrap();
    assert_eq!(observed.next.unwrap().model, "later-local-model");
    let recovered = reopened
        .recover_pending_command(&pending[0].request_id)
        .await
        .unwrap();
    let receipt = recovered.model_selection.unwrap();
    let selection = receipt.selected.unwrap();
    assert_eq!(selection.model, "model");
    assert_eq!(selection.effort.as_deref(), Some("high"));
    assert_eq!(receipt.default_persistence, "unconfirmed");
    assert_eq!(receipt.effect_scope, "session_and_source_default_attempt");
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn create_recovers_same_confirmed_receipt_after_lost_response_and_core_restart() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.session_create_supported = true;
        host.drop_command_receipts = true;
    }
    assert!(core
        .create_session("host-1", crate::api::CreateSessionInput::default())
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    access.host.lock().unwrap().drop_command_receipts = false;
    let recovered = reopened
        .recover_pending_command(&pending[0].request_id)
        .await
        .unwrap();
    assert_eq!(recovered.lifecycle_receipt.unwrap().session_id, "created-1");
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(reopened.pending_commands().unwrap().is_empty());
}

#[tokio::test]
async fn lost_question_answer_receipts_recover_without_another_source_call() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    let question = core.get_question("session-1", "batch-1").await.unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    let answer = crate::api::QuestionAnswer {
        id: "q".into(),
        selected: vec!["A".into()],
        custom: None,
    };
    assert!(core.answer_question(&question, vec![answer]).await.is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    access.host.lock().unwrap().drop_command_receipts = false;
    let recovered = reopened
        .recover_pending_command(&pending[0].request_id)
        .await
        .unwrap();
    assert_eq!(recovered.status, OnlineStatus::Succeeded);
    assert_eq!(
        recovered.question_receipt.unwrap().phase,
        "QUESTION_REPLY_QUEUED"
    );
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(reopened.pending_commands().unwrap().is_empty());
}

#[tokio::test]
async fn accepted_command_recovery_handshake_failure_remains_unknown_and_keeps_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.drop_channel_after_command = true;
        host.reject_channel_handshakes = true;
    }
    let error = core.send_message("session-1", "fixture").await.unwrap_err();
    assert_eq!(error.code(), "unknown");
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].request_id.is_empty());
    assert!(!pending[0].command_id.is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(core.is_paired());
}

#[tokio::test]
async fn authenticated_recovery_preserves_a_confirmed_native_failure() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.drop_channel_after_command = true;
        host.settled_command_status = whalerein_proto::OnlineStatus::Failed;
    }
    let error = core.send_message("session-1", "fixture").await.unwrap_err();
    assert!(!matches!(error, CoreError::UnknownResult));
    assert!(core.pending_commands().unwrap().is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn accepted_command_channel_reopen_failure_before_poll_remains_unknown() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    let handle = core
        .control(
            "host-1",
            RequestPayload {
                payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                    whalerein_proto::SendMessage {
                        session_id: "session-1".into(),
                        text: "fixture".into(),
                        delivery_mode: whalerein_proto::DeliveryMode::StartIfIdle as i32,
                    },
                )),
            },
        )
        .await
        .unwrap();
    assert_eq!(handle.status, OnlineStatus::HostAccepted);
    core.runtime.clear_channel("host-1");
    access.host.lock().unwrap().reject_channel_handshakes = true;
    let error = core
        .await_command_terminal("host-1", handle)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "unknown");
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn lifecycle_recovery_rejects_a_mismatched_access_request_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.session_create_supported = true;
        host.drop_command_receipts = true;
    }
    assert!(core
        .create_session("host-1", crate::api::CreateSessionInput::default())
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    {
        let mut host = access.host.lock().unwrap();
        let state = host.request_mut(&pending[0].request_id).unwrap();
        state.request_id = "different-fixture-request".into();
        state.status = whalerein_proto::OnlineStatus::Failed as i32;
        state.error_code = whalerein_proto::ErrorCode::Unavailable as i32;
    }
    let recovered = core.recover_pending_command(&pending[0].request_id).await;
    assert!(matches!(recovered, Err(CoreError::Security(_))));
    assert_eq!(core.pending_commands().unwrap(), pending);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn authenticated_lifecycle_result_outweighs_conflicting_access_status() {
    for misleading in [
        whalerein_proto::OnlineStatus::Failed,
        whalerein_proto::OnlineStatus::Rejected,
        whalerein_proto::OnlineStatus::HostOffline,
    ] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.session_create_supported = true;
            host.drop_command_receipts = true;
        }
        assert!(core
            .create_session("host-1", crate::api::CreateSessionInput::default())
            .await
            .is_err());
        let pending = core.pending_commands().unwrap();
        assert_eq!(pending.len(), 1);
        {
            let mut host = access.host.lock().unwrap();
            let state = host.request_mut(&pending[0].request_id).unwrap();
            state.status = misleading as i32;
            state.error_code = whalerein_proto::ErrorCode::Unavailable as i32;
            host.drop_command_receipts = false;
        }
        let recovered = core
            .recover_pending_command(&pending[0].request_id)
            .await
            .unwrap();
        assert_eq!(recovered.lifecycle_receipt.unwrap().session_id, "created-1");
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
        assert!(core.pending_commands().unwrap().is_empty());
    }
}

#[tokio::test]
async fn lifecycle_access_failure_without_authenticated_host_keeps_unknown_intent() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.session_create_supported = true;
        host.drop_command_receipts = true;
    }
    assert!(core
        .create_session("host-1", crate::api::CreateSessionInput::default())
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    {
        let mut host = access.host.lock().unwrap();
        let state = host.request_mut(&pending[0].request_id).unwrap();
        state.status = whalerein_proto::OnlineStatus::Failed as i32;
        host.drop_command_receipts = false;
        host.reject_channel_handshakes = true;
    }
    assert!(matches!(
        core.recover_pending_command(&pending[0].request_id).await,
        Err(CoreError::UnknownResult)
    ));
    assert_eq!(core.pending_commands().unwrap(), pending);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn lifecycle_authenticated_host_failure_settles_original_intent() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.session_create_supported = true;
        host.drop_command_receipts = true;
        host.settled_command_status = whalerein_proto::OnlineStatus::Failed;
    }
    assert!(core
        .create_session("host-1", crate::api::CreateSessionInput::default())
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    {
        let mut host = access.host.lock().unwrap();
        let state = host.request_mut(&pending[0].request_id).unwrap();
        state.status = whalerein_proto::OnlineStatus::Failed as i32;
        host.drop_command_receipts = false;
    }
    assert!(matches!(
        core.recover_pending_command(&pending[0].request_id).await,
        Err(CoreError::Host { .. })
    ));
    assert!(core.pending_commands().unwrap().is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn lifecycle_negative_receipt_recovers_authenticated_host_success_without_retry() {
    for misleading in [
        whalerein_proto::OnlineStatus::Rejected,
        whalerein_proto::OnlineStatus::HostOffline,
    ] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.session_create_supported = true;
            host.command_receipt_status_override = Some(misleading);
        }
        let handle = core
            .create_session("host-1", crate::api::CreateSessionInput::default())
            .await
            .unwrap();
        assert_eq!(handle.lifecycle_receipt.unwrap().session_id, "created-1");
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
        assert!(core.pending_commands().unwrap().is_empty());
    }
}

#[tokio::test]
async fn revocation_retains_only_opaque_unconfirmed_identity_and_clears_credentials() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core
        .send_message("session-1", "synthetic private fixture")
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    core.revoke().await.unwrap();
    assert!(!core.is_paired());
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_none());
    let identities = core.unconfirmed_revoked_commands().unwrap();
    assert_eq!(identities, vec![pending[0].request_id.clone()]);
    let bytes = std::fs::read(
        dir.path()
            .join("unconfirmed-commands")
            .join(format!("{}.json", identities[0])),
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for sensitive in [
        &pending[0].command_id,
        &pending[0].host_id,
        &pending[0].device_id,
        &pending[0].access_url,
        &pending[0].host_fingerprint,
    ] {
        assert!(!sensitive.is_empty());
        assert!(!text.contains(sensitive));
    }
    assert!(!text.contains("synthetic private fixture"));
    let reopened = Core::new(core.options.clone()).unwrap();
    assert_eq!(reopened.unconfirmed_revoked_commands().unwrap(), identities);
    reopened
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(matches!(
        reopened.recover_pending_command(&identities[0]).await,
        Err(CoreError::NotFound(_))
    ));
    assert_eq!(reopened.unconfirmed_revoked_commands().unwrap(), identities);
}

#[tokio::test]
async fn revocation_storage_failure_still_clears_credentials_and_reports_failure() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    std::fs::write(dir.path().join("unconfirmed-commands"), b"obstruction").unwrap();
    assert!(matches!(core.revoke().await, Err(CoreError::Internal(_))));
    assert!(!core.is_paired());
    assert!(core.revoked_identity_retention_unverified().unwrap());
    let reopened = Core::new(core.options.clone()).unwrap();
    assert!(reopened.revoked_identity_retention_unverified().unwrap());
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn revocation_loads_dormant_cache_before_preserving_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let request = core.pending_commands().unwrap()[0].request_id.clone();
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    reopened.revoke().await.unwrap();
    assert_eq!(
        reopened.unconfirmed_revoked_commands().unwrap(),
        vec![request]
    );
    assert!(!dir.path().join("cache.sqlite3").exists());
}

#[tokio::test]
async fn revoked_admission_fence_rejects_old_cache_and_survives_repair() {
    use whalerein_proto::{request_payload, Interrupt, RequestPayload};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let old_process = Core::new(core.options.clone()).unwrap();
    let channel = old_process.ensure_channel("host-1", false).await.unwrap();
    let service = old_process.command_service().unwrap();
    let payload = || RequestPayload {
        payload: Some(request_payload::Payload::Interrupt(Interrupt {
            session_id: "session-1".into(),
        })),
    };
    super::revoked_commands::write_marker(dir.path(), "confirmed").unwrap();
    assert!(matches!(
        service
            .control("host-1", &paired.device_id, &channel, payload())
            .await,
        Err(CoreError::Revoked)
    ));
    std::fs::remove_file(dir.path().join("admission-revoked")).unwrap();
    core.revoke().await.unwrap();
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(matches!(
        service
            .control("host-1", &paired.device_id, &channel, payload())
            .await,
        Err(CoreError::Revoked)
    ));
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
}

#[tokio::test]
async fn interrupted_revocation_resumes_snapshot_and_cleanup_on_reopen() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    super::revoked_commands::write_marker(dir.path(), "confirmed").unwrap();
    let options = core.options.clone();
    drop(core);
    let error = Core::new(options.clone()).err().unwrap();
    assert!(error.to_string().contains("unable to verify"));
    let reopened = Core::new(options).unwrap();
    assert!(!reopened.is_paired());
    assert!(reopened.revoked_identity_retention_unverified().unwrap());
    assert!(reopened.unconfirmed_revoked_commands().unwrap().is_empty());
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn revocation_fence_write_failure_does_not_revoke_without_durable_intent() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let request = core.pending_commands().unwrap()[0].request_id.clone();
    let original_cache = core.cache().unwrap();
    std::fs::create_dir(dir.path().join("admission-revoked")).unwrap();
    assert!(matches!(core.revoke().await, Err(CoreError::Internal(_))));
    assert!(core.is_paired());
    assert!(core.pending_commands().is_err());
    assert_eq!(
        original_cache.pending_commands().unwrap()[0].request_id,
        request
    );
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_some());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn pending_admission_rechecks_fence_after_lock_is_released() {
    use whalerein_proto::{request_payload, Interrupt, RequestPayload};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let channel = core.ensure_channel("host-1", false).await.unwrap();
    let service = core.command_service().unwrap();
    let held = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        started_tx.send(()).unwrap();
        runtime.block_on(service.control(
            "host-1",
            &paired.device_id,
            &channel,
            RequestPayload {
                payload: Some(request_payload::Payload::Interrupt(Interrupt {
                    session_id: "session-1".into(),
                })),
            },
        ))
    });
    started_rx.recv().unwrap();
    super::revoked_commands::write_marker(dir.path(), "confirmed").unwrap();
    drop(held);
    assert!(matches!(worker.join().unwrap(), Err(CoreError::Revoked)));
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn revocation_attempts_every_secret_delete_and_retries_after_reopen() {
    use crate::api::{MemorySecretStore, SecretError, SecretStore};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct FaultStore {
        inner: MemorySecretStore,
        failed: Mutex<Option<String>>,
        seen: Mutex<Vec<String>>,
    }
    impl SecretStore for FaultStore {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
            self.inner.load(key)
        }
        fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
            self.inner.save(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.seen.lock().unwrap().push(key.to_owned());
            if self.failed.lock().unwrap().as_deref() == Some(key) {
                Err(SecretError::Failed)
            } else {
                self.inner.delete(key)
            }
        }
    }
    for kind in ["token", "cache", "salt"] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultStore::default());
        let core = Core::new(CoreOptions {
            data_dir: dir.path().into(),
            secrets: store.clone(),
        })
        .unwrap();
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let keys = [
            crate::security::keys::device_token(&paired.device_id),
            crate::security::keys::cache_key(&paired.device_id),
            crate::security::keys::diagnostic_salt(&paired.device_id),
        ];
        let index = match kind {
            "token" => 0,
            "cache" => 1,
            _ => 2,
        };
        store.save(&keys[2], b"fixture").unwrap();
        *store.failed.lock().unwrap() = Some(keys[index].clone());
        store.seen.lock().unwrap().clear();
        assert!(core.revoke().await.is_err());
        for key in &keys {
            assert!(store.seen.lock().unwrap().contains(key));
        }
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(store.load(key).unwrap().is_some(), i == index);
        }
        assert!(!dir.path().join("cache.sqlite3").exists());
        assert!(!dir.path().join("config.json").exists());
        *store.failed.lock().unwrap() = None;
        let reopened = Core::new(core.options.clone()).unwrap();
        assert!(!reopened.is_paired());
        for key in &keys {
            assert!(store.load(key).unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn uncertain_revocation_reopens_without_false_revoked_and_can_retry() {
    for drop_after in [false, true] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.fail_revoke_before = !drop_after;
            host.drop_revoke_receipt = drop_after;
        }
        assert!(core.revoke().await.is_err());
        assert!(core.is_paired());
        assert!(core.authorization_verification_pending().unwrap());
        let reopened = Core::new(core.options.clone()).unwrap();
        assert!(reopened.is_paired());
        assert!(reopened.authorization_verification_pending().unwrap());
        {
            let mut host = access.host.lock().unwrap();
            host.fail_revoke_before = false;
            host.drop_revoke_receipt = false;
        }
        if drop_after {
            assert!(matches!(
                reopened.revoke().await,
                Err(CoreError::RevokedRetentionUnverified)
            ));
        } else {
            reopened.revoke().await.unwrap();
        }
        assert!(reopened.revoked_identity_retention_unverified().unwrap());
        assert!(!reopened.is_paired());
        assert!(!reopened.authorization_verification_pending().unwrap());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn revocation_reports_cache_sidecar_and_config_cleanup_errors_and_retries() {
    for target in [
        "cache.sqlite3-journal",
        "cache.sqlite3-wal",
        "cache.sqlite3-shm",
        "config.json",
    ] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let path = dir.path().join(target);
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::create_dir(&path).unwrap();
        assert!(core.clear_local_pairing(&paired.device_id).is_err());
        assert!(core.options.data_dir.join("admission-revoked").exists());
        assert!(!dir.path().join("cache.sqlite3").exists());
        std::fs::remove_dir(&path).unwrap();
        let reopened = Core::new(core.options.clone()).unwrap();
        assert!(!reopened.is_paired());
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn held_profile_lock_does_not_block_executor_and_wait_is_cancellable() {
    use whalerein_proto::{request_payload, Interrupt, RequestPayload};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let channel = core.ensure_channel("host-1", false).await.unwrap();
    let service = core.command_service().unwrap();
    let held = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = observed.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(75),
        service.control(
            "host-1",
            &paired.device_id,
            &channel,
            RequestPayload {
                payload: Some(request_payload::Payload::Interrupt(Interrupt {
                    session_id: "session-1".into(),
                })),
            },
        ),
    )
    .await;
    assert!(result.is_err());
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    drop(held);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    assert!(core.pending_commands().unwrap().is_empty());
}

#[tokio::test]
async fn confirmed_revocation_cleans_even_when_secret_load_fails() {
    use crate::api::{MemorySecretStore, SecretError, SecretStore};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct FaultStore {
        inner: MemorySecretStore,
        failed: Mutex<Option<String>>,
        deleted: Mutex<Vec<String>>,
    }
    impl SecretStore for FaultStore {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
            if self.failed.lock().unwrap().as_deref() == Some(key) {
                Err(SecretError::Failed)
            } else {
                self.inner.load(key)
            }
        }
        fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
            self.inner.save(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.deleted.lock().unwrap().push(key.to_owned());
            self.inner.delete(key)
        }
    }
    for kind in ["token", "cache"] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultStore::default());
        let core = Core::new(CoreOptions {
            data_dir: dir.path().into(),
            secrets: store.clone(),
        })
        .unwrap();
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        core.pending_commands().unwrap();
        let keys = [
            crate::security::keys::device_token(&paired.device_id),
            crate::security::keys::cache_key(&paired.device_id),
            crate::security::keys::diagnostic_salt(&paired.device_id),
        ];
        let marker = format!("pending:revoke:{}", "a".repeat(32));
        super::revoked_commands::write_marker(dir.path(), &marker).unwrap();
        *store.failed.lock().unwrap() = Some(keys[if kind == "token" { 0 } else { 1 }].clone());
        let result = core.clear_local_pairing_inner(&paired.device_id, false, Some(&marker));
        assert!(result.is_ok());
        for key in &keys {
            assert!(store.deleted.lock().unwrap().contains(key));
        }
        assert!(!dir.path().join("cache.sqlite3").exists());
        assert!(!dir.path().join("config.json").exists());
        *store.failed.lock().unwrap() = None;
        let reopened = Core::new(core.options.clone()).unwrap();
        assert!(!reopened.is_paired());
        for key in &keys {
            assert!(store.load(key).unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn remote_revocation_cleans_when_secret_load_breaks_after_delete() {
    use crate::api::test_support::FakeHost;
    use crate::api::{MemorySecretStore, SecretError, SecretStore};
    use std::sync::{Arc, Mutex};
    struct FaultStore {
        inner: MemorySecretStore,
        host: Arc<Mutex<FakeHost>>,
        failed: Mutex<Option<String>>,
    }
    impl SecretStore for FaultStore {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
            if self.host.lock().unwrap().revoked
                && self.failed.lock().unwrap().as_deref() == Some(key)
            {
                Err(SecretError::Failed)
            } else {
                self.inner.load(key)
            }
        }
        fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
            self.inner.save(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.inner.delete(key)
        }
    }
    for (kind, cold) in [("token", false), ("cache", false), ("cache", true)] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultStore {
            inner: MemorySecretStore::new(),
            host: access.host.clone(),
            failed: Mutex::new(None),
        });
        let core = Core::new(CoreOptions {
            data_dir: dir.path().into(),
            secrets: store.clone(),
        })
        .unwrap();
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        core.pending_commands().unwrap();
        let core = if cold {
            let options = core.options.clone();
            drop(core);
            Core::new(options).unwrap()
        } else {
            core
        };
        let keys = [
            crate::security::keys::device_token(&paired.device_id),
            crate::security::keys::cache_key(&paired.device_id),
            crate::security::keys::diagnostic_salt(&paired.device_id),
        ];
        *store.failed.lock().unwrap() = Some(keys[if kind == "token" { 0 } else { 1 }].clone());
        let result = core.revoke().await;
        result.unwrap();
        assert!(!core.revoked_identity_retention_unverified().unwrap());
        assert!(!dir.path().join("cache.sqlite3").exists());
        assert!(!dir.path().join("config.json").exists());
        *store.failed.lock().unwrap() = None;
        let reopened = Core::new(core.options.clone()).unwrap();
        assert!(!reopened.is_paired());
        for key in &keys {
            assert!(store.load(key).unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn stale_startup_fence_cannot_clear_new_grant_or_new_pending_intent() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.revoke().await.unwrap();
    let stale = Core::new(core.options.clone()).unwrap();
    let marker = super::revoked_commands::marker(dir.path())
        .unwrap()
        .unwrap();
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let new_pending = format!("pending:revoke:{}", "f".repeat(32));
    super::revoked_commands::write_marker(dir.path(), &new_pending).unwrap();
    stale
        .resume_unconfigured_revocation(&paired.device_id, &marker)
        .unwrap();
    assert!(stale.is_paired());
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_some());
    assert_eq!(
        super::revoked_commands::marker(dir.path())
            .unwrap()
            .as_deref(),
        Some(new_pending.as_str())
    );
}

#[tokio::test]
async fn restart_after_uncertain_snapshot_never_claims_retention_succeeded() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    super::revoked_commands::write_marker(dir.path(), "confirmed").unwrap();
    let options = core.options.clone();
    drop(core);
    secrets
        .delete(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap();
    std::fs::remove_file(dir.path().join("cache.sqlite3")).unwrap();
    let error = Core::new(options.clone()).err().unwrap();
    assert!(error.to_string().contains("unable to verify"));
    let reopened = Core::new(options).unwrap();
    assert!(!reopened.is_paired());
    assert!(reopened.revoked_identity_retention_unverified().unwrap());
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_unlinks_only_the_locked_profile_after_path_replacement() {
    use std::os::unix::fs::symlink;
    let access = FakeAccess::start().await;
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("profile");
    let moved = parent.path().join("moved");
    let outside = tempfile::tempdir().unwrap();
    let (core, _) = core(&original);
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(&original).unwrap();
    std::fs::write(outside.path().join("config.json"), b"external-config").unwrap();
    std::fs::write(outside.path().join("cache.sqlite3"), b"external-cache").unwrap();
    std::fs::rename(&original, &moved).unwrap();
    symlink(outside.path(), &original).unwrap();
    core.clear_local_pairing_credentials(&paired.device_id, guard.directory(), None)
        .unwrap();
    assert!(!moved.join("config.json").exists());
    assert_eq!(
        std::fs::read(outside.path().join("config.json")).unwrap(),
        b"external-config"
    );
    assert_eq!(
        std::fs::read(outside.path().join("cache.sqlite3")).unwrap(),
        b"external-cache"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_never_mutates_an_untrusted_open_sqlite_connection() {
    use std::sync::Arc;
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let external = Arc::new(
        crate::cache::Cache::open(&outside.path().join("foreign.sqlite3"), [0u8; 32]).unwrap(),
    );
    external
        .put_command(
            "fixture-request",
            "fixture-command",
            OnlineStatus::Unknown,
            None,
        )
        .unwrap();
    *core.cache.lock().unwrap() = Some(external.clone());
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    core.clear_local_pairing_credentials(&paired.device_id, guard.directory(), None)
        .unwrap();
    assert!(external.get_command("fixture-request").unwrap().is_some());
    assert!(!dir.path().join("config.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_does_not_modify_untrusted_wal_when_last_cache_reference_drops() {
    use std::sync::Arc;
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let path = outside.path().join("foreign.sqlite3");
    let external = Arc::new(crate::cache::Cache::open(&path, [0u8; 32]).unwrap());
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL;")
            .unwrap();
    }
    external
        .put_command(
            "fixture-request",
            "fixture-command",
            OnlineStatus::Unknown,
            None,
        )
        .unwrap();
    let wal = outside.path().join("foreign.sqlite3-wal");
    let before_db = std::fs::read(&path).unwrap();
    let before_wal = std::fs::read(&wal).unwrap();
    *core.cache.lock().unwrap() = Some(external);
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    core.clear_local_pairing_credentials(&paired.device_id, guard.directory(), None)
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before_db,
        "closing the last SQLite handle changed the external DB"
    );
    assert_eq!(
        std::fs::read(&wal).unwrap(),
        before_wal,
        "closing the last SQLite handle changed the external WAL"
    );
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[test]
fn sqlite_readonly_fd_remains_bound_after_path_replacement() {
    use std::os::fd::AsRawFd;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.sqlite3");
    let old = dir.path().join("old.sqlite3");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE fixture(value TEXT); INSERT INTO fixture VALUES ('original');",
            )
            .unwrap();
    }
    let file = std::fs::File::open(&path).unwrap();
    std::fs::rename(&path, &old).unwrap();
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE fixture(value TEXT); INSERT INTO fixture VALUES ('replacement');",
            )
            .unwrap();
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let fdpath = format!("/dev/fd/{}", file.as_raw_fd());
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let fdpath = format!("/proc/self/fd/{}", file.as_raw_fd());
    let connection =
        rusqlite::Connection::open_with_flags(fdpath, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let value: String = connection
        .query_row("SELECT value FROM fixture", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "original");
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn readonly_revocation_snapshot_rejects_profile_after_replacement() {
    use std::os::unix::fs::symlink;
    let access = FakeAccess::start().await;
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("profile");
    let moved = parent.path().join("moved");
    let outside = tempfile::tempdir().unwrap();
    let (core, _) = core(&original);
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    let foreign_path = outside.path().join("cache.sqlite3");
    {
        let foreign = crate::cache::Cache::open(&foreign_path, [0u8; 32]).unwrap();
        foreign
            .put_command(
                "foreign-request",
                "foreign-command",
                OnlineStatus::Unknown,
                None,
            )
            .unwrap();
    }
    let original_foreign = std::fs::read(&foreign_path).unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(&original).unwrap();
    std::fs::rename(&original, &moved).unwrap();
    symlink(outside.path(), &original).unwrap();
    assert!(cache
        .pending_commands_from_existing(guard.directory(), &original)
        .is_err());
    assert!(std::fs::read(&foreign_path).unwrap() == original_foreign);
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn readonly_revocation_snapshot_rejects_wal_or_journal() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    assert!(cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .is_ok());
    for name in [
        "cache.sqlite3-journal",
        "cache.sqlite3-wal",
        "cache.sqlite3-shm",
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, b"fixture").unwrap();
        assert!(cache
            .pending_commands_from_existing(guard.directory(), dir.path())
            .is_err());
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn readonly_revocation_snapshot_fails_closed_while_writer_holds_transaction() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let writer = rusqlite::Connection::open(dir.path().join("cache.sqlite3")).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE; INSERT INTO commands(request_id,command_id,status,updated_at_unix_ms) VALUES ('fixture','fixture','unknown',1);").unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    assert!(cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .is_err());
    writer.execute_batch("ROLLBACK").unwrap();
    assert!(cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .is_ok());
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn readonly_snapshot_reserves_database_against_concurrent_commit() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    let snapshot = cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .unwrap();
    let writer = rusqlite::Connection::open(dir.path().join("cache.sqlite3")).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_millis(50))
        .unwrap();
    assert!(writer.execute_batch("BEGIN IMMEDIATE; INSERT INTO commands(request_id,command_id,status,updated_at_unix_ms) VALUES ('fixture','fixture','unknown',1); COMMIT;").is_err());
    let _ = writer.execute_batch("ROLLBACK");
    drop(snapshot);
    writer.execute_batch("BEGIN IMMEDIATE; INSERT INTO commands(request_id,command_id,status,updated_at_unix_ms) VALUES ('fixture','fixture','unknown',1); COMMIT;").unwrap();
}

#[tokio::test]
async fn revocation_outcome_always_exposes_retention_state() {
    for make_cache in [false, true] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        if make_cache {
            core.pending_commands().unwrap();
        }
        let outcome = core.revoke().await.unwrap();
        assert_eq!(outcome.device_id, paired.device_id);
        assert_eq!(
            outcome.retention,
            if make_cache {
                RevocationRetention::Recorded
            } else {
                RevocationRetention::Unverified
            }
        );
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn cleanup_rejects_replaced_snapshot_database_before_unlink() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    let snapshot = cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .unwrap();
    std::fs::rename(
        dir.path().join("cache.sqlite3"),
        dir.path().join("original.sqlite3"),
    )
    .unwrap();
    std::fs::write(dir.path().join("cache.sqlite3"), b"replacement").unwrap();
    assert!(core
        .clear_local_pairing_credentials(&paired.device_id, guard.directory(), Some(&snapshot))
        .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("cache.sqlite3")).unwrap(),
        b"replacement"
    );
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn cleanup_reports_snapshot_database_with_remaining_hardlink() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    let snapshot = cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .unwrap();
    std::fs::hard_link(
        dir.path().join("cache.sqlite3"),
        dir.path().join("additional-link"),
    )
    .unwrap();
    assert!(core
        .clear_local_pairing_credentials(&paired.device_id, guard.directory(), Some(&snapshot))
        .is_err());
    assert!(dir.path().join("additional-link").exists());
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn pinned_snapshot_keeps_lock_across_name_check_and_other_connection_close() {
    use std::process::Command;
    fn external_writer_is_blocked(path: &std::path::Path) -> bool {
        let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=0.1)
try:
 c.execute('BEGIN IMMEDIATE')
 c.execute("INSERT INTO commands(request_id,command_id,status,updated_at_unix_ms) VALUES ('external','external','unknown',1)")
 c.commit()
 sys.exit(1)
except sqlite3.OperationalError as e:
 sys.exit(0 if 'locked' in str(e).lower() else 2)
"#;
        Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(path)
            .status()
            .unwrap()
            .success()
    }
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    let snapshot = cache
        .pending_commands_from_existing(guard.directory(), dir.path())
        .unwrap();
    let path = dir.path().join("cache.sqlite3");
    assert!(external_writer_is_blocked(&path));
    assert!(snapshot.matches_name(guard.directory()));
    assert!(external_writer_is_blocked(&path));
    drop(core.cache.lock().unwrap().take());
    assert!(external_writer_is_blocked(&path));
    drop(snapshot);
}

#[tokio::test]
async fn conflict_reopen_retries_secrets_even_when_configuration_is_unreadable() {
    use crate::api::{MemorySecretStore, SecretError, SecretStore};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct FaultStore {
        inner: MemorySecretStore,
        failed: Mutex<Option<String>>,
    }
    impl SecretStore for FaultStore {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
            self.inner.load(key)
        }
        fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
            self.inner.save(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            if self.failed.lock().unwrap().as_deref() == Some(key) {
                Err(SecretError::Failed)
            } else {
                self.inner.delete(key)
            }
        }
    }
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let core = Core::new(CoreOptions {
        data_dir: dir.path().into(),
        secrets: store.clone(),
    })
    .unwrap();
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let token = crate::security::keys::device_token(&paired.device_id);
    *store.failed.lock().unwrap() = Some(token.clone());
    let marker = format!("pending:revoke:{}", "a".repeat(32));
    super::revoked_commands::write_marker(dir.path(), &marker).unwrap();
    std::fs::remove_file(dir.path().join("config.json")).unwrap();
    std::fs::create_dir(dir.path().join("config.json")).unwrap();
    assert!(core
        .clear_local_pairing_inner(&paired.device_id, false, Some(&marker))
        .is_err());
    assert!(store.load(&token).unwrap().is_some());
    assert!(core.revocation_cleanup_requires_inspection().unwrap());
    *store.failed.lock().unwrap() = None;
    let error = Core::new(core.options.clone()).err().unwrap();
    assert!(error.to_string().contains("manual inspection"));
    assert!(store.load(&token).unwrap().is_none());
    assert!(dir.path().join("config.json").is_dir());
}

#[tokio::test]
async fn confirmed_recovery_with_unreadable_config_clears_secrets_for_every_phase() {
    for phase in [
        "confirmed",
        "confirmed-saved",
        "confirmed-lost",
        "confirmed-conflict",
    ] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        super::revoked_commands::write_marker(dir.path(), phase).unwrap();
        std::fs::remove_file(dir.path().join("config.json")).unwrap();
        std::fs::create_dir(dir.path().join("config.json")).unwrap();
        let options = core.options.clone();
        drop(core);
        let error = Core::new(options).err().unwrap();
        assert!(error.to_string().contains("manual inspection"));
        for key in [
            crate::security::keys::device_token(&paired.device_id),
            crate::security::keys::cache_key(&paired.device_id),
            crate::security::keys::diagnostic_salt(&paired.device_id),
        ] {
            assert!(secrets.load(&key).unwrap().is_none());
        }
        assert!(dir.path().join("config.json").is_dir());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn transient_external_writer_commit_or_rollback_preserves_identity() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    for commit in [false, true] {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let (core, secrets) = core(dir.path());
        let paired = core
            .pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        access.host.lock().unwrap().drop_command_receipts = true;
        assert!(core.send_message("session-1", "fixture").await.is_err());
        let identity = core.pending_commands().unwrap()[0].request_id.clone();
        assert!(core.cache().unwrap().has_revocation_provenance());
        let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=1)
c.execute('BEGIN IMMEDIATE')
c.execute('UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1')
print('ready',flush=True)
action=sys.stdin.readline().strip()
c.commit() if action=='commit' else c.rollback()
c.close()
"#;
        let mut writer = Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(script)
            .arg(dir.path().join("cache.sqlite3"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(writer.stdout.take().unwrap());
        let mut ready = String::new();
        reader.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");
        let worker = core.clone();
        let task = tokio::spawn(async move { worker.revoke().await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!task.is_finished());
        assert!(secrets
            .load(&crate::security::keys::device_token(&paired.device_id))
            .unwrap()
            .is_none());
        assert!(secrets
            .load(&crate::security::keys::cache_key(&paired.device_id))
            .unwrap()
            .is_none());
        assert!(dir.path().join("cache.sqlite3").exists());
        assert!(matches!(
            core.pending_commands(),
            Err(CoreError::RevokedRetentionUnverified)
        ));
        assert!(core.revoked_identity_retention_unverified().unwrap());
        assert!(secrets
            .load(&crate::security::keys::cache_key(&paired.device_id))
            .unwrap()
            .is_none());
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(if commit { b"commit\n" } else { b"rollback\n" })
            .unwrap();
        assert!(writer.wait().unwrap().success());
        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome.retention, RevocationRetention::Recorded);
        assert!(!core.revoked_identity_retention_unverified().unwrap());
        assert_eq!(core.unconfirmed_revoked_commands().unwrap(), vec![identity]);
        assert!(!dir.path().join("cache.sqlite3").exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_wait_then_new_process_reports_unverified_without_old_connection() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=1)
c.execute('BEGIN IMMEDIATE')
c.execute('UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1')
print('ready',flush=True)
sys.stdin.readline()
c.rollback()
c.close()
"#;
    let mut writer = Command::new("python3")
        .arg("-u")
        .arg("-c")
        .arg(script)
        .arg(dir.path().join("cache.sqlite3"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(writer.stdout.take().unwrap());
    let mut ready = String::new();
    reader.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "ready");
    let worker = core.clone();
    let task = tokio::spawn(async move { worker.revoke().await });
    // The worker must clear secrets while the database write stays blocked.
    // Poll instead of sleeping a fixed interval: a loaded CI runner may not
    // schedule the spawned task within the old 150 ms window.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let token = secrets
            .load(&crate::security::keys::device_token(&paired.device_id))
            .unwrap();
        let key = secrets
            .load(&crate::security::keys::cache_key(&paired.device_id))
            .unwrap();
        if token.is_none() && key.is_none() {
            break;
        }
        assert!(
            !task.is_finished(),
            "revocation finished against a locked database"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "revocation did not clear secrets"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!task.is_finished());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(dir.path().join("cache.sqlite3").exists());
    assert!(super::revoked_commands::marker(dir.path())
        .unwrap()
        .unwrap()
        .starts_with("confirmed:"));
    let error = Core::new(core.options.clone()).err().unwrap();
    assert!(error.to_string().contains("unable to verify"));
    assert!(!dir.path().join("cache.sqlite3").exists());
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    let _ = writer.wait();
    let reopened = Core::new(core.options.clone()).unwrap();
    assert!(!reopened.is_paired());
    assert!(reopened.revoked_identity_retention_unverified().unwrap());
    assert!(reopened.unconfirmed_revoked_commands().unwrap().is_empty());
    assert!(!dir.path().join("cache.sqlite3").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn transient_writer_deadline_clears_credentials_and_reports_unverified() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=1)
c.execute('BEGIN IMMEDIATE')
c.execute('UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1')
print('ready',flush=True)
sys.stdin.readline()
c.rollback()
c.close()
"#;
    let mut writer = Command::new("python3")
        .arg("-u")
        .arg("-c")
        .arg(script)
        .arg(dir.path().join("cache.sqlite3"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(writer.stdout.take().unwrap());
    let mut ready = String::new();
    reader.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "ready");
    let marker = format!("pending:revoke:{}", "a".repeat(32));
    super::revoked_commands::write_marker(dir.path(), &marker).unwrap();
    let result = core
        .clear_local_pairing_async_with_timeout(
            &paired.device_id,
            &marker,
            Duration::from_millis(150),
        )
        .await;
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    let _ = writer.wait();
    let error = result.unwrap_err();
    assert!(error.to_string().contains("unable to verify"));
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(core.revoked_identity_retention_unverified().unwrap());
    assert!(!dir.path().join("cache.sqlite3").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn restart_with_active_writer_and_no_connection_reports_unverified() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=1)
c.execute('BEGIN IMMEDIATE')
c.execute('UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1')
print('ready',flush=True)
sys.stdin.readline()
c.rollback()
c.close()
"#;
    let mut writer = Command::new("python3")
        .arg("-u")
        .arg("-c")
        .arg(script)
        .arg(dir.path().join("cache.sqlite3"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(writer.stdout.take().unwrap());
    let mut ready = String::new();
    reader.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "ready");
    super::revoked_commands::write_marker(dir.path(), "confirmed").unwrap();
    let error = Core::new(core.options.clone()).err().unwrap();
    assert!(error.to_string().contains("unable to verify"));
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(!dir.path().join("cache.sqlite3").exists());
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    let _ = writer.wait();
    let reopened = Core::new(core.options.clone()).unwrap();
    assert!(!reopened.is_paired());
    assert!(reopened.revoked_identity_retention_unverified().unwrap());
    assert!(reopened.unconfirmed_revoked_commands().unwrap().is_empty());
    assert!(!dir.path().join("cache.sqlite3").exists());
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android"
))]
#[tokio::test]
async fn busy_snapshot_must_not_release_existing_sqlite_process_lock() {
    use std::process::Command;
    fn outside_writer_blocked(path: &std::path::Path) -> bool {
        let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=0.1)
try:
 c.execute('BEGIN IMMEDIATE')
 c.execute("INSERT INTO commands(request_id,command_id,status,updated_at_unix_ms) VALUES ('outside','outside','unknown',1)")
 c.rollback()
 sys.exit(1)
except sqlite3.OperationalError as e:
 sys.exit(0 if 'locked' in str(e).lower() else 2)
"#;
        Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(path)
            .status()
            .unwrap()
            .success()
    }
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    core.cache()
        .unwrap()
        .put_command("fixture", "fixture", OnlineStatus::Unknown, None)
        .unwrap();
    let path = dir.path().join("cache.sqlite3");
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "BEGIN IMMEDIATE; UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1;",
        )
        .unwrap();
    let cache = core.cache().unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    assert!(outside_writer_blocked(&path));
    assert!(matches!(
        cache.pending_commands_from_existing(guard.directory(), dir.path()),
        Err(crate::cache::CacheError::Busy)
    ));
    assert!(outside_writer_blocked(&path));
    writer.execute_batch("ROLLBACK").unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_core_revocation_initializes_and_reuses_existing_cache_connection() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let identity = core.pending_commands().unwrap()[0].request_id.clone();
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    assert!(reopened.cache.lock().unwrap().is_none());
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    assert!(reopened.try_prepare_revocation_cache(guard.directory()));
    let existing = reopened.cache.lock().unwrap().clone().unwrap();
    let connection_id = existing.sqlite_connection_identity();
    drop(guard);
    let outcome = reopened.revoke().await.unwrap();
    assert_eq!(existing.sqlite_connection_identity(), connection_id);
    assert_eq!(outcome.retention, RevocationRetention::Recorded);
    assert_eq!(
        reopened.unconfirmed_revoked_commands().unwrap(),
        vec![identity]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_revocation_rejects_group_writable_profile_provenance() {
    use std::os::unix::fs::PermissionsExt;
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    assert_eq!(core.pending_commands().unwrap().len(), 1);
    let options = core.options.clone();
    drop(core);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
    let reopened = Core::new(options).unwrap();
    let outcome = reopened.revoke().await.unwrap();
    assert_eq!(outcome.device_id, paired.device_id);
    assert_eq!(outcome.retention, RevocationRetention::Unverified);
    assert!(reopened.unconfirmed_revoked_commands().unwrap().is_empty());
    assert!(!dir.path().join("cache.sqlite3").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn revoke_preflight_does_not_block_executor_on_cache_mutex() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let slot = core.cache.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _held = slot.lock().unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    let worker = core.clone();
    let task = tokio::spawn(async move { worker.revoke().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!task.is_finished());
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.retention, RevocationRetention::Recorded);
}

#[cfg(unix)]
#[tokio::test]
async fn cache_initialization_cannot_create_key_while_admission_guard_is_held() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let key = crate::security::keys::cache_key(&paired.device_id);
    secrets.delete(&key).unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    assert!(matches!(
        core.pending_commands(),
        Err(CoreError::Unavailable(_))
    ));
    assert!(secrets.load(&key).unwrap().is_none());
    drop(guard);
}

#[cfg(unix)]
#[tokio::test]
async fn waiting_marker_failure_with_busy_cache_does_not_await_or_leave_credentials() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.pending_commands().unwrap();
    let marker = format!("pending:revoke:{}", "a".repeat(32));
    super::revoked_commands::write_marker(dir.path(), &marker).unwrap();
    core.fail_waiting_marker.store(true, Ordering::SeqCst);
    let slot = core.cache.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _held = slot.lock().unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    let result = tokio::time::timeout(
        Duration::from_millis(300),
        core.clear_local_pairing_async_with_timeout(
            &paired.device_id,
            &marker,
            Duration::from_secs(5),
        ),
    )
    .await;
    assert!(
        result.is_ok(),
        "marker failure must not wait for a cache slot"
    );
    assert!(result.unwrap().is_err());
    assert!(secrets
        .load(&crate::security::keys::device_token(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&paired.device_id))
        .unwrap()
        .is_none());
    assert!(super::revoked_commands::marker(dir.path())
        .unwrap()
        .unwrap()
        .ends_with(":conflict"));
    release_tx.send(()).unwrap();
    holder.join().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_core_revocation_automatically_initializes_existing_pending_cache() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let identity = core.pending_commands().unwrap()[0].request_id.clone();
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    assert!(reopened.cache.lock().unwrap().is_none());
    let outcome = reopened.revoke().await.unwrap();
    assert_eq!(outcome.retention, RevocationRetention::Recorded);
    assert_eq!(
        reopened.unconfirmed_revoked_commands().unwrap(),
        vec![identity]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_core_revocation_waits_for_transient_writer_and_preserves_identity() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let identity = core.pending_commands().unwrap()[0].request_id.clone();
    let options = core.options.clone();
    drop(core);
    let script = r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=1)
c.execute('BEGIN IMMEDIATE')
c.execute('UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1')
print('ready',flush=True)
sys.stdin.readline()
c.rollback()
c.close()
"#;
    let mut writer = Command::new("python3")
        .arg("-u")
        .arg("-c")
        .arg(script)
        .arg(dir.path().join("cache.sqlite3"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(writer.stdout.take().unwrap());
    let mut ready = String::new();
    reader.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "ready");
    let reopened = Core::new(options).unwrap();
    let worker = reopened.clone();
    let task = tokio::spawn(async move { worker.revoke().await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!task.is_finished());
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    assert!(writer.wait().unwrap().success());
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.retention, RevocationRetention::Recorded);
    assert_eq!(
        reopened.unconfirmed_revoked_commands().unwrap(),
        vec![identity]
    );
}

#[tokio::test]
async fn recovery_after_the_access_request_row_expires_uses_the_host_journal() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core.send_message("session-1", "fixture").await.is_err());
    let pending = core.pending_commands().unwrap();
    access.host.lock().unwrap().drop_command_receipts = false;
    // Access now answers 410 expired for the control-plane row (a long wait or
    // cleanup), but the Host journal still has the executed command.
    access.host.lock().unwrap().expire_request_id = Some(pending[0].request_id.clone());
    let recovered = core
        .recover_pending_command(&pending[0].request_id)
        .await
        .expect("an expired Access row must not discard the Host journal result");
    assert_eq!(recovered.status, OnlineStatus::Succeeded);
    assert_eq!(recovered.message_id.as_deref(), Some("message-1"));
    assert!(core.pending_commands().unwrap().is_empty());
}
