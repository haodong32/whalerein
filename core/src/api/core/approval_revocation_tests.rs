use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::PairingDecision;

#[tokio::test]
async fn fake_host_accepts_approval_submissions_then_self_revocation_clears_local_pairing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let approved = core
        .approve("host-1", "approval-1", &[0xabu8; 32], 3)
        .await
        .unwrap();
    assert_eq!(approved.status, OnlineStatus::HostAccepted);
    let rejected = core
        .reject("host-1", "approval-2", &[0xcdu8; 32], 4)
        .await
        .unwrap();
    assert_eq!(rejected.status, OnlineStatus::HostAccepted);
    assert_eq!(access.host.lock().unwrap().commands_seen, 2);
    assert_eq!(core.cache().unwrap().command_counts().unwrap(), (2, 0));
    let revoked = core.revoke().await.unwrap();
    assert_eq!(revoked.device_id, outcome.device_id);
    assert!(!core.is_paired());
    assert!(matches!(core.hosts().await, Err(CoreError::NotPaired)));
    assert!(!dir.path().join("config.json").exists());
    assert!(!dir.path().join("cache.sqlite3").exists());
}

// --- APPR-03 / APPR-07: Host-scoped approval reads and candidate naming ----

fn fixture_approval(id: &str, state: whalerein_proto::ApprovalState) -> whalerein_proto::Approval {
    whalerein_proto::Approval {
        approval_id: id.into(),
        session_id: "session-1".into(),
        tool_name: "bash".into(),
        call_id: "call-1".into(),
        reason: "fixture".into(),
        digest: vec![0xab; 32],
        version: 3,
        expires_at_unix_ms: 1_700_000_000_000,
        state: state as i32,
        decision: whalerein_proto::ApprovalDecision::Unspecified as i32,
    }
}

/// Authorize the FakeHost's optional host-2 identity with the same pin.
fn authorize_extra_host(core: &Core) {
    let mut config = core.current_config().unwrap();
    config
        .host_fingerprints
        .insert("host-2".into(), config.host_fingerprint.clone().unwrap());
    core.config_store().save(&config).unwrap();
    *core.config.write().unwrap() = Some(config);
}

#[tokio::test]
async fn get_approval_reports_live_terminal_and_unknown_states_with_a_not_found_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let cases = [
        (
            whalerein_proto::ApprovalState::Pending,
            "APPROVAL_STATE_PENDING",
        ),
        (
            whalerein_proto::ApprovalState::Approved,
            "APPROVAL_STATE_APPROVED",
        ),
        (
            whalerein_proto::ApprovalState::Rejected,
            "APPROVAL_STATE_REJECTED",
        ),
        (
            whalerein_proto::ApprovalState::Expired,
            "APPROVAL_STATE_EXPIRED",
        ),
        (
            whalerein_proto::ApprovalState::Unavailable,
            "APPROVAL_STATE_UNAVAILABLE",
        ),
        (
            whalerein_proto::ApprovalState::Unspecified,
            "APPROVAL_STATE_UNSPECIFIED",
        ),
    ];
    for (state, label) in cases {
        access.host.lock().unwrap().approvals = vec![fixture_approval("approval-1", state)];
        let info = core.get_approval("host-1", "approval-1").await.unwrap();
        assert_eq!(info.state, label);
        assert_eq!(info.approval_id, "approval-1");
        assert_eq!(info.version, 3);
        assert_eq!(info.digest, vec![0xab; 32]);
    }
    access.host.lock().unwrap().approvals = vec![whalerein_proto::Approval {
        state: 99,
        ..fixture_approval("approval-1", whalerein_proto::ApprovalState::Pending)
    }];
    assert_eq!(
        core.get_approval("host-1", "approval-1")
            .await
            .unwrap()
            .state,
        "UNKNOWN_ENUM_99"
    );

    assert!(matches!(
        core.get_approval("host-1", "missing").await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        core.get_approval("host-1", "").await,
        Err(CoreError::InvalidInput(_))
    ));
    access.host.lock().unwrap().approval_error = Some(whalerein_proto::ErrorCode::Unavailable);
    assert!(matches!(
        core.get_approval("host-1", "approval-1").await,
        Err(CoreError::Host { .. })
    ));
}

#[tokio::test]
async fn approval_host_resolution_is_explicit_and_names_bounded_candidates() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().approvals = vec![fixture_approval(
        "approval-1",
        whalerein_proto::ApprovalState::Expired,
    )];
    assert_eq!(
        core.get_approval("host-1", "approval-1")
            .await
            .unwrap()
            .state,
        "APPROVAL_STATE_EXPIRED"
    );
    assert_eq!(
        access.host.lock().unwrap().approval_queries.last(),
        Some(&"host-1".to_owned())
    );

    access.host.lock().unwrap().extra_host = true;
    authorize_extra_host(&core);
    let error = core.approval_host(None).await.unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)));
    let message = error.to_string();
    assert!(
        message.contains("host-1") && message.contains("host-2"),
        "{message}"
    );
    assert!(message.contains("--host"), "{message}");

    access.host.lock().unwrap().extra_host_approvals = vec![fixture_approval(
        "approval-2",
        whalerein_proto::ApprovalState::Approved,
    )];
    assert_eq!(core.approval_host(Some("host-2")).await.unwrap(), "host-2");
    let info = core.get_approval("host-2", "approval-2").await.unwrap();
    assert_eq!(info.state, "APPROVAL_STATE_APPROVED");
    assert_eq!(
        access.host.lock().unwrap().approval_queries.last(),
        Some(&"host-2".to_owned())
    );
    assert!(matches!(
        core.get_approval("host-2", "approval-1").await,
        Err(CoreError::NotFound(_))
    ));

    assert!(matches!(
        core.approval_host(Some("host-9")).await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        core.get_approval("host-9", "approval-2").await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn ambiguous_approval_candidates_are_bounded_in_one_message() {
    let hosts = (0..10)
        .map(|index| HostInfo {
            host_id: format!("host-{index}"),
            online: true,
            protocol_version: 1,
            fingerprint: "fixture".into(),
        })
        .collect::<Vec<_>>();
    let shown = super::bounded_approval_host_candidates(&hosts);
    assert!(shown.starts_with("host-0"));
    assert!(shown.contains("host-7"));
    assert!(!shown.contains("host-8"), "{shown}");
    assert!(shown.ends_with("and 2 more"), "{shown}");
}

#[tokio::test]
async fn approve_and_reject_name_candidates_and_route_only_the_scoped_host() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.host.lock().unwrap().extra_host = true;
    authorize_extra_host(&core);

    let approve_error = core.approval_host(None).await.unwrap_err();
    assert!(matches!(approve_error, CoreError::Conflict(_)));
    let approve_message = approve_error.to_string();
    assert!(
        approve_message.contains("host-1") && approve_message.contains("host-2"),
        "{approve_message}"
    );
    assert!(approve_message.contains("--host"), "{approve_message}");
    assert_eq!(access.host.lock().unwrap().approval_resolutions.len(), 0);
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    assert!(matches!(
        core.approve("host-9", "approval-1", &[0xab; 32], 3).await,
        Err(CoreError::NotFound(_))
    ));
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);

    let before = access.host.lock().unwrap().commands_seen;
    let handle = core
        .approve("host-1", "approval-1", &[0xab; 32], 3)
        .await
        .unwrap();
    assert_eq!(handle.status, OnlineStatus::HostAccepted);
    assert_eq!(access.host.lock().unwrap().commands_seen, before + 1);
    assert_eq!(
        access.host.lock().unwrap().approval_resolutions,
        vec![(
            "host-1".to_owned(),
            "approval-1".to_owned(),
            whalerein_proto::ApprovalDecision::Approve as i32,
        )]
    );

    let handle = core
        .reject("host-2", "approval-2", &[0xcd; 32], 4)
        .await
        .unwrap();
    assert_eq!(handle.status, OnlineStatus::HostAccepted);
    assert_eq!(
        access.host.lock().unwrap().approval_resolutions,
        vec![
            (
                "host-1".to_owned(),
                "approval-1".to_owned(),
                whalerein_proto::ApprovalDecision::Approve as i32,
            ),
            (
                "host-2".to_owned(),
                "approval-2".to_owned(),
                whalerein_proto::ApprovalDecision::Reject as i32,
            ),
        ]
    );
}

#[tokio::test]
async fn list_approvals_honours_the_host_scope_and_keeps_the_session_filter() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let mut second_session =
        fixture_approval("approval-2", whalerein_proto::ApprovalState::Pending);
    second_session.session_id = "session-2".into();
    access.host.lock().unwrap().approvals = vec![
        fixture_approval("approval-1", whalerein_proto::ApprovalState::Pending),
        second_session,
    ];
    let listed = core.list_approvals("host-1", "session-1").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].approval_id, "approval-1");
    assert_eq!(
        access.host.lock().unwrap().approval_filters.last(),
        Some(&"session-1".to_owned())
    );
    assert_eq!(
        core.list_approvals("host-1", "").await.unwrap().len(),
        2,
        "an empty session lists every pending approval when --host resolves the Host"
    );
    assert!(matches!(
        core.list_approvals("host-9", "").await,
        Err(CoreError::NotFound(_))
    ));

    access.host.lock().unwrap().extra_host = true;
    authorize_extra_host(&core);
    access.host.lock().unwrap().extra_host_approvals = vec![fixture_approval(
        "approval-3",
        whalerein_proto::ApprovalState::Rejected,
    )];
    let listed = core.list_approvals("host-2", "session-1").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].approval_id, "approval-3");
    assert_eq!(
        access.host.lock().unwrap().approval_queries.last(),
        Some(&"host-2".to_owned())
    );
    assert_eq!(
        access.host.lock().unwrap().approval_filters.last(),
        Some(&"session-1".to_owned())
    );
    // The FakeHost serves both identities through one channel, so an
    // unscoped probe across two Hosts is exercised by the session-routing
    // tests; here the scoped listing must stay on the named Host.
    assert!(matches!(
        core.list_approvals("host-9", "session-1").await,
        Err(CoreError::NotFound(_))
    ));
}
