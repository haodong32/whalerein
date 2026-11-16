use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{OnlineStatus, PairingDecision};

#[tokio::test]
async fn wait_uses_remote_state_and_rejects_invalid_deadlines() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let passive = core.read_wait_state("session-1").await.unwrap();
    assert!(!passive.active && !passive.queue_available);
    for timeout in [0, 2_147_483_648] {
        assert!(matches!(
            core.wait_session("session-1", timeout).await,
            Err(CoreError::InvalidInput(_))
        ));
    }
    access.host.lock().unwrap().wait_timeouts = 2;
    let observation = core.wait_session("session-1", 3000).await.unwrap();
    assert_eq!(observation.reason, crate::api::WaitReason::Idle);
    assert_eq!(access.host.lock().unwrap().wait_queries, 3);
    assert!(!observation.state.active);
    assert!(!observation.state.queue_available);
    // A live tail owns this guard; a read-only wait must not claim it.
    core.watch_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (first, second) = tokio::join!(
        core.wait_session("session-1", 3000),
        core.wait_session("session-1", 3000)
    );
    assert_eq!(first.unwrap().reason, crate::api::WaitReason::Idle);
    assert_eq!(second.unwrap().reason, crate::api::WaitReason::Idle);
    assert!(core.watch_active.load(std::sync::atomic::Ordering::SeqCst));
    core.watch_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    access.host.lock().unwrap().wait_timeouts = usize::MAX;
    // Leave a real timing margin: the first session lookup and coverage
    // instrumentation share the deadline, and a 100 ms budget is too tight on
    // slower native runners.
    let observation = core.wait_session("session-1", 1000).await.unwrap();
    assert_eq!(observation.reason, crate::api::WaitReason::Timeout);
    access.host.lock().unwrap().wait_timeouts = 0;
    for (code, expected) in [
        (whalerein_proto::ErrorCode::Unavailable, "unavailable"),
        (whalerein_proto::ErrorCode::Expired, "expired"),
    ] {
        access.host.lock().unwrap().query_error = Some(code);
        let failure = core.wait_session("session-1", 3000).await.unwrap_err();
        assert_eq!(failure.code(), expected);
    }
    access.host.lock().unwrap().query_error = None;
    access.host.lock().unwrap().wait_bad_id = true;
    assert!(matches!(
        core.read_wait_state("session-1").await,
        Err(CoreError::ChannelMismatch)
    ));
    assert!(matches!(
        core.wait_session("session-1", 3000).await,
        Err(CoreError::ChannelMismatch)
    ));
}

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
async fn a_channel_without_a_capability_rejects_the_operation_before_submission() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();

    let mut advertised = crate::pairing::default_capabilities();
    advertised
        .capabilities
        .retain(|value| *value != whalerein_proto::Capability::Messages as i32);
    access.host.lock().unwrap().channel_capabilities = Some(advertised);

    let error = core
        .send_message("session-1", "hello")
        .await
        .expect_err("messages capability was not advertised");
    assert!(matches!(error, CoreError::Unsupported));
    assert_eq!(error.code(), "unsupported_capability");
    assert_eq!(error.exit_code(), 9);
    assert!(
        core.pending_commands().unwrap().is_empty(),
        "a gated command must not be journaled"
    );
    assert_eq!(
        access.host.lock().unwrap().commands_seen,
        0,
        "a gated command must not be submitted"
    );

    // The ordinary advertised set still carries the same operation, so the
    // gate cannot reject a normal channel.
    access.host.lock().unwrap().channel_capabilities = None;
    core.runtime().clear_channel("host-1");
    let handle = core.send_message("session-1", "hello").await.unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
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
    assert_eq!(handle.delivery_id.as_deref(), Some("delivery-1"));
    assert!(!handle.request_id.is_empty());
    assert!(!handle.command_id.is_empty());
    assert_eq!(
        access.host.lock().unwrap().last_delivery_mode,
        Some(whalerein_proto::DeliveryMode::EnqueueAfterRun as i32)
    );
    core.send_message_with_delivery("session-1", "fixture", whalerein_proto::DeliveryMode::Steer)
        .await
        .unwrap();
    assert_eq!(
        access.host.lock().unwrap().last_delivery_mode,
        Some(whalerein_proto::DeliveryMode::Steer as i32)
    );

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
    assert_eq!(revoked.device_id, outcome.device_id);
    assert!(access.host.lock().unwrap().revoked);
    assert!(!core.is_paired());
    assert!(!secrets.is_empty());
    assert!(!dir.path().join("config.json").exists());
    // Windows can keep the (now key-less) SQLite file open until process exit;
    // authorization is already gone, so only assert the portable invariant.
    #[cfg(not(windows))]
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
    let key = core.session_key("session-1").await.unwrap();
    assert_eq!(core.cached_messages(&key).unwrap().len(), 1);
}

#[tokio::test]
async fn model_selection_uses_fresh_capability_and_preserves_receipt() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    assert_eq!(
        core.list_models("host-1")
            .await
            .unwrap()
            .default_selection
            .unwrap()
            .model,
        "model"
    );
    assert!(core
        .read_session_model("session-1")
        .await
        .unwrap()
        .pending
        .is_none());
    access.host.lock().unwrap().model_selection_available = false;
    assert!(matches!(
        core.list_models("host-1").await,
        Err(CoreError::Unsupported)
    ));
    assert!(matches!(
        core.read_session_model("session-1").await,
        Err(CoreError::Unsupported)
    ));
    assert!(matches!(
        core.select_session_model("session-1", "provider", "model", None)
            .await,
        Err(CoreError::Unsupported)
    ));
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    access.host.lock().unwrap().model_selection_available = true;
    let receipt = core
        .select_session_model("session-1", "provider", "model", Some("high"))
        .await
        .unwrap()
        .model_selection
        .unwrap();
    assert_eq!(receipt.selected.unwrap().effort.as_deref(), Some("high"));
    assert_eq!(receipt.default_persistence, "unconfirmed");
    assert_eq!(receipt.effect_scope, "session_and_source_default_attempt");
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    access.host.lock().unwrap().query_error = Some(whalerein_proto::ErrorCode::InvalidArgument);
    let legacy = core.host_capabilities("host-1").await.unwrap();
    assert!(!legacy.model_catalog && !legacy.model_read);
    access.host.lock().unwrap().query_error = Some(whalerein_proto::ErrorCode::Unauthorized);
    assert!(core.host_capabilities("host-1").await.is_err());
}

#[tokio::test]
async fn question_queries_are_scoped_and_guarded_answers_keep_the_receipt() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    let page = core.list_questions("session-1", "").await.unwrap();
    assert!(core
        .cancel_question_answer("session-1", "command-1")
        .await
        .unwrap());
    assert!(core.cancel_question_answer("session-1", "").await.is_err());
    assert_eq!(page.batches.len(), 1);
    let batch = core.get_question("session-1", "batch-1").await.unwrap();
    assert_eq!(
        batch.questions[0].options[0].description.as_deref(),
        Some("Option")
    );
    assert!(core.get_question("session-1", "other").await.is_err());
    let answer = crate::api::QuestionAnswer {
        id: "q".into(),
        selected: vec!["A".into()],
        custom: None,
    };
    let handle = core
        .answer_question(&batch, vec![answer.clone()])
        .await
        .unwrap();
    assert_eq!(
        handle.question_receipt.unwrap().phase,
        "QUESTION_REPLY_QUEUED"
    );
    let mut invalid = batch.clone();
    invalid.source_epoch.clear();
    assert!(core
        .answer_question(&invalid, vec![answer.clone()])
        .await
        .is_err());
    invalid = batch.clone();
    invalid.phase = "QUESTION_SETTLED".into();
    assert!(core.answer_question(&invalid, vec![answer]).await.is_err());
}

#[tokio::test]
async fn wait_reconnects_with_a_fresh_channel_without_revoking_pairing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    let old = access.host.lock().unwrap().channel_id();
    access.host.lock().unwrap().drop_channel();
    let result = core.wait_session("session-1", 3000).await.unwrap();
    assert_eq!(result.reason, crate::api::WaitReason::Idle);
    assert_ne!(access.host.lock().unwrap().channel_id(), old);
    assert!(core.is_paired());
    assert!(!secrets.is_empty());
}

#[tokio::test]
async fn wait_recovery_reissues_a_new_identity_with_only_remaining_budget() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.wait_drop_channel_once = true;
        host.wait_receipt_delay_ms = 150;
    }
    let result = core.wait_session("session-1", 3000).await.unwrap();
    assert_eq!(result.reason, crate::api::WaitReason::Idle);
    let host = access.host.lock().unwrap();
    assert_eq!(host.wait_ids.len(), 2);
    assert_ne!(host.wait_ids[0], host.wait_ids[1]);
    assert!(host.wait_timeout_values[1] + 50 < host.wait_timeout_values[0]);
}

#[tokio::test]
async fn wait_deadline_during_delayed_first_result_is_an_error() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_receipt_delay_ms = 250;
    let started = tokio::time::Instant::now();
    assert!(matches!(
        core.wait_session("session-1", 100).await,
        Err(CoreError::Timeout)
    ));
    // The contract is the Timeout, not the scheduler latency: only a stuck
    // implementation would exceed this bound, which coverage instrumentation
    // and slower Windows timers can otherwise trip.
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn wait_slice_budget_is_calculated_after_a_delayed_channel_handshake() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().channel_receipt_delay_ms = 200;
    assert_eq!(
        core.wait_session("session-1", 1500).await.unwrap().reason,
        crate::api::WaitReason::Idle
    );
    let host = access.host.lock().unwrap();
    assert!(
        host.wait_last_timeout_ms <= 1100,
        "slice did not account for handshake time"
    );
}

#[tokio::test]
async fn cancelling_a_wait_attempts_to_cancel_its_exact_remote_observation() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_receipt_delay_ms = 1000;
    let runner = tokio::spawn(async move { core.wait_session("session-1", 3000).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while access.host.lock().unwrap().wait_ids.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    runner.abort();
    let _ = runner.await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while access.host.lock().unwrap().wait_cancel_ids.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let host = access.host.lock().unwrap();
    assert_eq!(host.wait_cancel_ids, host.wait_ids);
    assert_eq!(host.commands_seen, 0);
}

#[tokio::test]
async fn creation_requires_live_supported_host_without_submitting_a_mutation() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    for code in [
        whalerein_proto::ErrorCode::UnsupportedCapability,
        whalerein_proto::ErrorCode::InvalidArgument,
    ] {
        access.host.lock().unwrap().query_error = Some(code);
        assert!(matches!(
            core.create_session("host-1", CreateSessionInput::default())
                .await,
            Err(CoreError::Unsupported)
        ));
        assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    }
    {
        let mut host = access.host.lock().unwrap();
        host.query_error = None;
        host.host_online = false;
    }
    assert!(matches!(
        core.create_session("host-1", CreateSessionInput::default())
            .await,
        Err(CoreError::HostOffline)
    ));
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    assert!(core.pending_commands().unwrap().is_empty());
}

#[tokio::test]
async fn model_operations_do_not_cache_or_defer_when_host_is_offline() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    assert!(core.list_models("host-1").await.is_ok());
    assert!(core.read_session_model("session-1").await.is_ok());
    access.host.lock().unwrap().host_online = false;
    assert!(core.list_models("host-1").await.is_err());
    assert!(core.read_session_model("session-1").await.is_err());
    assert!(core
        .select_session_model("session-1", "provider", "model", None)
        .await
        .is_err());
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    access.host.lock().unwrap().host_online = true;
    assert!(core.pending_commands().unwrap().is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
}

#[tokio::test]
async fn concurrent_waits_on_distinct_sessions_share_initial_channel() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        core.wait_session("session-1", 3000),
        core.wait_session("session-2", 3000)
    );
    assert_eq!(first.unwrap().session_id, "session-1");
    assert_eq!(second.unwrap().session_id, "session-2");
}

#[tokio::test]
async fn concurrent_waits_recover_one_replaced_channel_without_evicting_each_other() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    let baseline;
    {
        let mut host = access.host.lock().unwrap();
        baseline = host.channel_handshake_steps;
        host.wait_drop_channel_once = true;
        host.wait_receipt_delay_ms = 150;
    }
    let (first, second) = tokio::join!(
        core.wait_session("session-1", 3000),
        core.wait_session("session-2", 3000)
    );
    assert_eq!(first.unwrap().session_id, "session-1");
    assert_eq!(second.unwrap().session_id, "session-2");
    assert!(core.is_paired());
    assert_eq!(
        access.host.lock().unwrap().channel_handshake_steps,
        baseline + 2
    );
}

#[tokio::test]
async fn revocation_during_channel_handshake_cannot_restore_a_channel() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().channel_receipt_delay_ms = 200;
    let running = core.clone();
    let task = tokio::spawn(async move { running.wait_session("session-1", 3000).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while access.host.lock().unwrap().channel_handshake_steps == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    core.runtime().revoke();
    assert!(matches!(task.await.unwrap(), Err(CoreError::Revoked)));
    assert!(core.runtime().channel("host-1").is_none());
    assert_eq!(
        core.runtime().handshake_phase("host-1"),
        crate::runtime::HandshakePhase::Failed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cold_waits_share_identity_initialization() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let core = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(9));
    let mut tasks = Vec::new();
    for i in 0..8 {
        let core = core.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            core.wait_session(&format!("session-{i}"), 5000).await
        }));
    }
    barrier.wait().await;
    for task in tasks {
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn wait_recovers_only_the_explicit_channel_replacement_signal() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_replaced_once = true;
    access.host.lock().unwrap().wait_extra_replacements = 1;
    let result = core.wait_session("session-1", 3000).await.unwrap();
    assert_eq!(result.reason, crate::api::WaitReason::Idle);
    let host = access.host.lock().unwrap();
    assert_eq!(host.wait_ids.len(), 3);
    assert_ne!(host.wait_ids[0], host.wait_ids[1]);
    assert_ne!(host.wait_ids[1], host.wait_ids[2]);
}

#[tokio::test]
async fn wait_preserves_a_source_authorization_error_without_retrying_it() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_source_unauthorized_once = true;
    let error = core.wait_session("session-1", 3000).await.unwrap_err();
    assert_eq!(error.code(), "unauthorized");
    assert_eq!(access.host.lock().unwrap().wait_queries, 1);
    assert!(core.is_paired());
}

#[tokio::test]
async fn wait_reports_persistent_transport_failure_during_recovery_handshake() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.wait_timeouts = 1;
        host.wait_replace_at = 2;
        host.fail_handshake_polls_after_replacement = true;
    }
    let started = tokio::time::Instant::now();
    let error = core.wait_session("session-1", 1500).await.unwrap_err();
    assert_eq!(error.code(), "unavailable");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(access.host.lock().unwrap().wait_queries, 2);
    assert!(core.is_paired());
}

#[tokio::test]
async fn wait_rejects_tampered_error_body_even_if_status_claims_channel_replaced() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_corrupt_source_error_once = true;
    let error = core.wait_session("session-1", 3000).await.unwrap_err();
    assert!(matches!(
        error,
        CoreError::Security(_) | CoreError::Internal(_)
    ));
    assert_eq!(access.host.lock().unwrap().wait_queries, 1);
    assert!(core.is_paired());
    assert!(!core.runtime().is_revoked());
    assert!(!secrets.is_empty());
}

#[tokio::test]
async fn authenticated_source_error_takes_precedence_over_a_conflicting_status_code() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access
        .host
        .lock()
        .unwrap()
        .wait_conflicting_source_error_once = true;
    let error = core.wait_session("session-1", 3000).await.unwrap_err();
    assert_eq!(error.code(), "unauthorized");
    assert_eq!(error.source_layer(), "source");
    assert_eq!(access.host.lock().unwrap().wait_queries, 1);
    assert!(core.is_paired() && !core.runtime().is_revoked() && !secrets.is_empty());
}

#[tokio::test]
async fn authenticated_source_error_takes_precedence_over_a_conflicting_success_status() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_forged_success_once = true;
    let error = core.wait_session("session-1", 3000).await.unwrap_err();
    assert_eq!(error.code(), "unauthorized");
    assert_eq!(error.source_layer(), "source");
    assert_eq!(access.host.lock().unwrap().wait_queries, 1);
    assert!(core.is_paired() && !core.runtime().is_revoked() && !secrets.is_empty());
}

#[tokio::test]
async fn wait_rejects_a_foreign_channel_error_body_without_retrying() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().wait_foreign_frame_once = true;
    let error = core.wait_session("session-1", 3000).await.unwrap_err();
    assert!(matches!(error, CoreError::Security(_)));
    assert_eq!(access.host.lock().unwrap().wait_queries, 1);
    assert!(core.is_paired() && !core.runtime().is_revoked() && !secrets.is_empty());
}

#[tokio::test]
async fn legacy_host_creation_capability_errors_preserve_existing_controls() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    for code in [
        whalerein_proto::ErrorCode::InvalidArgument,
        whalerein_proto::ErrorCode::UnsupportedCapability,
    ] {
        access.host.lock().unwrap().legacy_capabilities_error = Some(code);
        let before = access.host.lock().unwrap().commands_seen;
        assert!(matches!(
            core.create_session("host-1", CreateSessionInput::default())
                .await,
            Err(CoreError::Unsupported)
        ));
        assert_eq!(access.host.lock().unwrap().commands_seen, before);
        assert_eq!(
            core.sessions("host-1").await.unwrap().sessions[0].session_id,
            "session-1"
        );
        assert_eq!(
            core.send_message("session-1", "fixture")
                .await
                .unwrap()
                .status,
            OnlineStatus::Succeeded
        );
        assert_eq!(
            core.interrupt("session-1").await.unwrap().status,
            OnlineStatus::Succeeded
        );
        assert_eq!(
            core.approve("approval-1", &[0xab; 32], 3)
                .await
                .unwrap()
                .status,
            OnlineStatus::HostAccepted
        );
    }
}

#[tokio::test]
async fn wait_deadline_includes_authorized_host_discovery() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().hosts_delay_ms = 500;
    let started = std::time::Instant::now();
    assert!(matches!(
        core.wait_session("session-1", 60).await,
        Err(CoreError::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_millis(400));
    assert_eq!(access.host.lock().unwrap().wait_queries, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn host_discovery_does_not_hide_confirmed_revocation_from_cache_admission() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    revoked_commands::write_marker(dir.path(), "confirmed-saved").unwrap();
    assert!(matches!(core.hosts().await, Err(CoreError::Revoked)));
}

#[cfg(unix)]
#[tokio::test]
async fn host_discovery_preserves_revocation_retention_loss() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    revoked_commands::write_marker(dir.path(), "confirmed-lost").unwrap();
    assert!(matches!(
        core.hosts().await,
        Err(CoreError::RevokedRetentionUnverified)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn host_discovery_rejects_malformed_revocation_marker() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    std::fs::write(dir.path().join("admission-revoked"), b"malformed").unwrap();
    assert!(matches!(core.hosts().await, Err(CoreError::Internal(_))));
}

#[cfg(unix)]
#[tokio::test]
async fn host_discovery_tolerates_unrelated_cache_failure() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let pairing = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    *core.cache.lock().unwrap() = None;
    secrets
        .save(&keys::cache_key(&pairing.device_id), &[0])
        .unwrap();
    assert_eq!(core.hosts().await.unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn host_discovery_reports_retention_unverified_while_revocation_is_waiting() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    revoked_commands::write_marker(dir.path(), "confirmed-waiting").unwrap();
    assert!(core.revoked_identity_retention_unverified().unwrap());
    assert!(matches!(
        core.hosts().await,
        Err(CoreError::RevokedRetentionUnverified)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn host_discovery_treats_unverifiable_warning_symlink_as_unverified() {
    use std::os::unix::fs::symlink;
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    revoked_commands::write_marker(dir.path(), "confirmed-saved").unwrap();
    symlink("missing", dir.path().join("revocation-retention-warning")).unwrap();
    assert!(core.revoked_identity_retention_unverified().unwrap());
    assert!(matches!(
        core.hosts().await,
        Err(CoreError::RevokedRetentionUnverified)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn active_pairing_rejects_forget_even_if_admission_is_temporarily_shared() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let paired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let guard = crate::security::device::ProfileLock::acquire_admission_shared(dir.path()).unwrap();
    assert!(matches!(core.forget_device(), Err(CoreError::Conflict(_))));
    assert!(matches!(
        core.forget_local_device(),
        Err(CoreError::Unavailable(_))
    ));
    assert!(core.is_paired());
    assert!(secrets
        .load(&keys::device_token(&paired.device_id))
        .unwrap()
        .is_some());
    drop(guard);
    assert!(matches!(core.forget_device(), Err(CoreError::Conflict(_))));
    core.forget_local_device().unwrap();
    assert!(!core.is_paired());
}

#[tokio::test]
async fn selecting_the_effective_model_is_a_core_noop_without_a_command() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _secrets) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    // The source projection reports this session's effective next model.
    access.host.lock().unwrap().model_observed = Some("model".into());
    let before = access.host.lock().unwrap().commands_seen;
    let handle = core
        .select_session_model("session-1", "provider", "model", None)
        .await
        .unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    // A no-op invents no command identity and journals nothing.
    assert!(handle.request_id.is_empty());
    assert!(handle.command_id.is_empty());
    let receipt = handle.model_selection.expect("model receipt");
    let selected = receipt.selected.expect("selected model");
    assert_eq!(selected.provider, "provider");
    assert_eq!(selected.model, "model");
    assert_eq!(selected.effort, None);
    assert_eq!(receipt.effect_scope, "session_and_source_default_attempt");
    assert_eq!(receipt.default_persistence, "unconfirmed");
    assert_eq!(access.host.lock().unwrap().commands_seen, before);
    assert!(core.pending_commands().unwrap().is_empty());

    // A different effort or model is still submitted exactly once.
    let high = core
        .select_session_model("session-1", "provider", "model", Some("high"))
        .await
        .unwrap();
    assert_eq!(high.status, OnlineStatus::Succeeded);
    assert_eq!(
        high.model_selection.unwrap().selected.unwrap().effort,
        Some("high".into())
    );
    assert_eq!(access.host.lock().unwrap().commands_seen, before + 1);
    let other = core
        .select_session_model("session-1", "provider", "other-model", None)
        .await
        .unwrap();
    assert_eq!(other.status, OnlineStatus::Succeeded);
    assert_eq!(access.host.lock().unwrap().commands_seen, before + 2);
}
