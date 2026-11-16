//! Session discovery, creation and fork regression tests.
//!
//! These assert the Core-observable behavior the CLI relies on: routing
//! classification, cache fallback shape and order, lifecycle receipts and the
//! local pre-submission validation contract.
use super::test_support::core;
use super::*;
use crate::api::test_support::{FakeAccess, LifecycleReply};
use crate::api::CreateSessionInput;
use crate::api::PairingDecision;

fn proto_session(
    session_id: &str,
    title: &str,
    updated_at_unix_ms: i64,
) -> whalerein_proto::SessionSummary {
    whalerein_proto::SessionSummary {
        session_id: session_id.into(),
        title: title.into(),
        cwd: "/work".into(),
        updated_at_unix_ms,
        running: true,
        agent_available: true,
        ..Default::default()
    }
}

async fn paired(access: &FakeAccess, dir: &tempfile::TempDir) -> Core {
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core
}

// SESS-01
#[tokio::test]
async fn an_unauthorized_sessions_host_is_not_found_not_a_fingerprint_conflict() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    assert_eq!(
        client.sessions("host-1").await.unwrap().sessions[0].session_id,
        "session-1"
    );

    let error = client.sessions("host-tpyo").await.unwrap_err();
    assert!(matches!(error, CoreError::NotFound(_)), "{error:?}");
    assert_eq!(error.code(), "not_found");
    assert_eq!(error.exit_code(), 3);
    assert!(
        !error.to_string().contains("fingerprint"),
        "an unknown Host must not suggest re-pairing: {error}"
    );

    // A Host advertised by Access but never paired with this profile is not
    // authorized either; discovery must not silently adopt it.
    access.host.lock().unwrap().extra_host = true;
    let error = client.sessions("host-2").await.unwrap_err();
    assert!(matches!(error, CoreError::NotFound(_)), "{error:?}");
}

// SESS-02
#[tokio::test]
async fn hosts_report_an_offline_authorized_host_without_cached_presence() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    let online = client.hosts().await.unwrap();
    assert_eq!(online.len(), 1);
    assert!(online[0].online);

    access.host.lock().unwrap().host_online = false;
    let offline = client.hosts().await.unwrap();
    assert_eq!(offline.len(), 1);
    assert_eq!(offline[0].host_id, "host-1");
    assert!(
        !offline[0].online,
        "offline discovery must report online=false"
    );
    // Presence is runtime state; a fresh scan is authoritative again.
    let cached = local_cache_only_view(&client);
    assert!(
        !cached[0].online,
        "cached discovery never claims an offline Host is online"
    );
    access.host.lock().unwrap().host_online = true;
    assert!(client.hosts().await.unwrap()[0].online);
}

fn local_cache_only_view(client: &Core) -> Vec<HostInfo> {
    client.cache().unwrap().load_hosts().unwrap()
}

// SESS-03
#[tokio::test]
async fn sessions_offline_cache_fallback_reports_source_and_empty_cache() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().host_online = false;
    // The empty-cache branch keeps the transport error and its exit code.
    let error = client.sessions("host-1").await.unwrap_err();
    assert!(matches!(error, CoreError::HostOffline), "{error:?}");
    assert_eq!(error.code(), "host_offline");
    assert_eq!(error.exit_code(), 5);

    access.host.lock().unwrap().host_online = true;
    let remote = client.sessions("host-1").await.unwrap();
    assert_eq!(remote.source, "remote");
    assert!(remote.complete);

    access.host.lock().unwrap().host_online = false;
    let cached = client.sessions("host-1").await.unwrap();
    assert_eq!(cached.source, "cache");
    assert!(!cached.complete);
    assert_eq!(cached.sessions[0].session_id, "session-1");
}

// SESS-04
#[tokio::test]
async fn a_remote_session_list_replaces_removed_entries_in_the_cache() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().sessions = vec![
        proto_session("session-1", "first", 20),
        proto_session("session-2", "second", 10),
    ];
    assert_eq!(client.sessions("host-1").await.unwrap().sessions.len(), 2);
    access.host.lock().unwrap().host_online = false;
    assert_eq!(client.sessions("host-1").await.unwrap().sessions.len(), 2);

    access.host.lock().unwrap().host_online = true;
    access.host.lock().unwrap().sessions = vec![proto_session("session-2", "second", 10)];
    let remote = client.sessions("host-1").await.unwrap();
    assert_eq!(remote.sessions.len(), 1);
    assert_eq!(remote.sessions[0].session_id, "session-2");

    access.host.lock().unwrap().host_online = false;
    let cached = client.sessions("host-1").await.unwrap();
    assert_eq!(cached.sessions.len(), 1);
    assert_eq!(
        cached.sessions[0].session_id, "session-2",
        "a removed or archived session must not linger in the offline cache"
    );
}

// SESS-05
#[tokio::test]
async fn remote_and_cached_session_order_is_identical_and_stable() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().sessions = vec![
        proto_session("session-a", "a", 10),
        proto_session("session-b", "b", 20),
        proto_session("session-c", "c", 10),
    ];
    let remote = client.sessions("host-1").await.unwrap();
    let expected = ["session-b", "session-a", "session-c"];
    assert_eq!(
        remote
            .sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        expected
    );

    access.host.lock().unwrap().host_online = false;
    let cached = client.sessions("host-1").await.unwrap();
    assert_eq!(
        cached
            .sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        expected,
        "offline order must not jump relative to the remote order"
    );
    assert_eq!(
        client
            .cached_sessions("host-1")
            .unwrap()
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
}

// SESS-06
#[tokio::test]
async fn fork_returns_every_lifecycle_disposition() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;

    let handle = client.fork_session("session-1").await.unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    let ready = handle.lifecycle_receipt.unwrap();
    assert_eq!(ready.operation, "fork");
    assert_eq!(ready.disposition, "READY");
    assert_eq!(ready.session_id, "forked-1");
    assert_eq!(ready.parent_session_id.as_deref(), Some("session-1"));
    assert!(ready.warning_code.is_none());
    assert_eq!(
        access.host.lock().unwrap().last_fork_parent.as_deref(),
        Some("session-1")
    );

    access.host.lock().unwrap().fork_reply = LifecycleReply::Warning {
        session_id: "forked-1".into(),
        warning_code: "workspace_attach_failed".into(),
        warning_detail: Some("attach incomplete".into()),
    };
    let handle = client.fork_session("session-1").await.unwrap();
    let warning = handle.lifecycle_receipt.unwrap();
    assert_eq!(warning.disposition, "READY_WITH_WARNING");
    assert_eq!(warning.session_id, "forked-1");
    assert_eq!(
        warning.warning_code.as_deref(),
        Some("workspace_attach_failed")
    );

    access.host.lock().unwrap().fork_reply = LifecycleReply::Failed {
        reason_code: "SOURCE_NO_COMPLETE_TURN".into(),
        retryable: false,
    };
    let handle = client.fork_session("session-1").await.unwrap();
    let failed = handle.lifecycle_receipt.unwrap();
    assert_eq!(failed.disposition, "FAILED");
    assert_eq!(
        failed.reason_code.as_deref(),
        Some("SOURCE_NO_COMPLETE_TURN")
    );
    assert!(!failed.retryable);
    assert!(failed.session_id.is_empty());
    assert_eq!(failed.parent_session_id.as_deref(), Some("session-1"));

    access.host.lock().unwrap().fork_reply = LifecycleReply::Unknown {
        reason_code: "NATIVE_INTERRUPTED".into(),
        reconcile_mode: "PARENT_SESSION_ID".into(),
        reconcile_key: Some("session-1".into()),
    };
    let handle = client.fork_session("session-1").await.unwrap();
    let unknown = handle.lifecycle_receipt.unwrap();
    assert_eq!(unknown.disposition, "UNKNOWN");
    assert_eq!(unknown.reconcile_mode.as_deref(), Some("PARENT_SESSION_ID"));
    assert_eq!(unknown.reconcile_key.as_deref(), Some("session-1"));
    assert!(unknown.session_id.is_empty());
    assert_eq!(access.host.lock().unwrap().fork_calls, 4);
}

// SESS-07
#[tokio::test]
async fn fork_requires_the_advertised_host_capability_without_submitting() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;

    access.host.lock().unwrap().session_fork_supported = false;
    let error = client.fork_session("session-1").await.unwrap_err();
    assert!(matches!(error, CoreError::Unsupported), "{error:?}");
    assert_eq!(error.code(), "unsupported_capability");
    assert_eq!(error.exit_code(), 9);
    assert_eq!(access.host.lock().unwrap().fork_calls, 0);

    {
        let mut host = access.host.lock().unwrap();
        host.session_fork_supported = true;
        host.legacy_capabilities_error = Some(whalerein_proto::ErrorCode::UnsupportedCapability);
    }
    assert!(matches!(
        client.fork_session("session-1").await,
        Err(CoreError::Unsupported)
    ));
    assert_eq!(access.host.lock().unwrap().fork_calls, 0);

    access.host.lock().unwrap().legacy_capabilities_error = None;
    access.host.lock().unwrap().host_online = false;
    assert!(matches!(
        client.fork_session("session-1").await,
        Err(CoreError::HostOffline)
    ));
    assert_eq!(access.host.lock().unwrap().fork_calls, 0);
}

// SESS-09
#[tokio::test]
async fn fork_without_a_completed_turn_is_a_non_retryable_failure_not_unknown() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().fork_reply = LifecycleReply::Failed {
        reason_code: "SOURCE_NO_COMPLETE_TURN".into(),
        retryable: false,
    };
    let handle = client.fork_session("session-1").await.unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    let receipt = handle.lifecycle_receipt.unwrap();
    assert_eq!(receipt.disposition, "FAILED");
    assert_eq!(
        receipt.reason_code.as_deref(),
        Some("SOURCE_NO_COMPLETE_TURN")
    );
    assert!(
        !receipt.retryable,
        "the running parent has no completed turn to fork, so an automatic retry cannot succeed"
    );
    assert!(
        receipt.session_id.is_empty(),
        "a failure must not claim a child"
    );
    assert!(receipt.reconcile_mode.is_none());
}

// SESS-10
#[tokio::test]
async fn sessions_preserve_fork_lineage_and_parent_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    let parent = proto_session("session-1", "parent", 20);
    let mut child = proto_session("session-2", "child", 10);
    child.parent_session_id = "session-1".into();
    child.origin = "fork".into();
    access.host.lock().unwrap().sessions = vec![parent, child];

    let list = client.sessions("host-1").await.unwrap();
    let listed = list
        .sessions
        .iter()
        .find(|session| session.session_id == "session-2")
        .unwrap();
    assert_eq!(listed.parent_session_id, "session-1");
    assert_eq!(listed.origin, "fork");
    let fetched = client.get_session("session-2").await.unwrap();
    assert_eq!(fetched.parent_session_id, "session-1");

    // The same lineage survives a cache-only view.
    access.host.lock().unwrap().host_online = false;
    let cached = client.sessions("host-1").await.unwrap();
    let listed = cached
        .sessions
        .iter()
        .find(|session| session.session_id == "session-2")
        .unwrap();
    assert_eq!(listed.parent_session_id, "session-1");
    assert_eq!(listed.origin, "fork");
}

// SESS-11
#[tokio::test]
async fn create_rejects_local_invalid_input_before_contacting_the_host() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().session_create_supported = true;
    let invalid = [
        CreateSessionInput {
            cwd: Some(String::new()),
            ..Default::default()
        },
        CreateSessionInput {
            cwd: Some("   ".into()),
            ..Default::default()
        },
        CreateSessionInput {
            cwd: Some("relative/worktree".into()),
            ..Default::default()
        },
        CreateSessionInput {
            cwd: Some("/work".into()),
            workspace_id: Some("workspace-1".into()),
            ..Default::default()
        },
        CreateSessionInput {
            agent_preset: Some("\t".into()),
            ..Default::default()
        },
        CreateSessionInput {
            workspace_id: Some(String::new()),
            ..Default::default()
        },
        CreateSessionInput {
            preferred_session_id: Some(String::new()),
            ..Default::default()
        },
    ];
    for input in invalid {
        let error = client
            .create_session("host-1", input.clone())
            .await
            .expect_err("invalid create input");
        assert!(
            matches!(error, CoreError::InvalidInput(_)),
            "{input:?}: {error:?}"
        );
        assert_eq!(error.code(), "invalid_argument");
        assert_eq!(error.exit_code(), 2);
    }
    let host = access.host.lock().unwrap();
    assert_eq!(
        host.create_calls, 0,
        "local validation must run before any lifecycle submission"
    );
    assert_eq!(host.commands_seen, 0);
    assert!(
        host.request_traces.is_empty(),
        "no request, not even a capability query, may precede local validation"
    );
}

#[tokio::test]
async fn create_reports_a_host_preflight_rejection_as_a_failed_lifecycle() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().session_create_supported = true;
    access.host.lock().unwrap().create_reply = LifecycleReply::Failed {
        reason_code: "WORKSPACE_NOT_FOUND".into(),
        retryable: true,
    };
    let handle = client
        .create_session(
            "host-1",
            CreateSessionInput {
                workspace_id: Some("missing".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let receipt = handle.lifecycle_receipt.unwrap();
    assert_eq!(receipt.disposition, "FAILED");
    assert_eq!(receipt.reason_code.as_deref(), Some("WORKSPACE_NOT_FOUND"));
    assert!(receipt.retryable);
    assert_eq!(access.host.lock().unwrap().create_calls, 1);
}

// SESS-12
#[tokio::test]
async fn create_unknown_carries_the_reconcile_contract_and_survives_same_id_retry() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().session_create_supported = true;
    access.host.lock().unwrap().create_reply = LifecycleReply::Unknown {
        reason_code: "NATIVE_INTERRUPTED".into(),
        reconcile_mode: "PREFERRED_SESSION_ID".into(),
        reconcile_key: Some("session-x".into()),
    };
    let input = CreateSessionInput {
        preferred_session_id: Some("session-x".into()),
        ..Default::default()
    };
    let handle = client
        .create_session("host-1", input.clone())
        .await
        .unwrap();
    let unknown = handle.lifecycle_receipt.unwrap();
    assert_eq!(unknown.disposition, "UNKNOWN");
    assert_eq!(
        unknown.reconcile_mode.as_deref(),
        Some("PREFERRED_SESSION_ID")
    );
    assert_eq!(unknown.reconcile_key.as_deref(), Some("session-x"));
    assert!(unknown.session_id.is_empty());
    assert!(unknown.reason_code.is_some());

    // A same-ID retry reuses the stable preferred identity and converges on one
    // child instead of creating a duplicate.
    access.host.lock().unwrap().create_reply = LifecycleReply::Ready {
        session_id: "session-x".into(),
    };
    let retried = client.create_session("host-1", input).await.unwrap();
    let ready = retried.lifecycle_receipt.unwrap();
    assert_eq!(ready.disposition, "READY");
    assert_eq!(ready.session_id, "session-x");
    let host = access.host.lock().unwrap();
    assert_eq!(host.create_calls, 2);
    assert_eq!(
        host.last_create_input
            .as_ref()
            .and_then(|input| input.preferred_session_id.as_deref()),
        Some("session-x")
    );
}

// SESS-13
#[tokio::test]
async fn lifecycle_failure_reason_codes_are_preserved_without_rewriting() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    // Golden reason codes the Host can emit for a proven pre-native failure.
    let golden = [
        ("INVALID_ARGUMENT", true),
        ("WORKSPACE_NOT_FOUND", true),
        ("WORKSPACE_UNAVAILABLE", false),
        ("PRESET_NOT_FOUND", true),
        ("PRESET_UNAVAILABLE", false),
        ("SOURCE_NOT_FOUND", false),
        ("SOURCE_NO_COMPLETE_TURN", false),
        ("PREFLIGHT_UNSUPPORTED_CAPABILITY", false),
        ("PREFLIGHT_UNAVAILABLE", false),
        ("NATIVE_INTERNAL", false),
        ("NATIVE_TIMEOUT", false),
        ("PROCESS_INTERRUPTED", false),
    ];
    for (reason, retryable) in golden {
        access.host.lock().unwrap().fork_reply = LifecycleReply::Failed {
            reason_code: reason.into(),
            retryable,
        };
        let handle = client.fork_session("session-1").await.unwrap();
        let receipt = handle.lifecycle_receipt.unwrap();
        assert_eq!(receipt.disposition, "FAILED", "{reason}");
        assert_eq!(receipt.reason_code.as_deref(), Some(reason));
        assert_eq!(receipt.retryable, retryable, "{reason}");
        assert!(
            receipt.session_id.is_empty(),
            "{reason} must not claim a child"
        );
    }

    // An UNKNOWN receipt without a reconcile mode is not actionable and must not
    // be presented as a normal lifecycle result.
    access.host.lock().unwrap().fork_reply = LifecycleReply::Unknown {
        reason_code: "NATIVE_INTERRUPTED".into(),
        reconcile_mode: String::new(),
        reconcile_key: Some("session-1".into()),
    };
    assert!(matches!(
        client.fork_session("session-1").await,
        Err(CoreError::UnknownResult)
    ));
}

// SESS-14
#[tokio::test]
async fn large_session_lists_are_not_truncated_and_keep_the_remote_order_offline() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    let sessions = (0..500)
        .map(|index| proto_session(&format!("session-{index:03}"), "title", index as i64))
        .collect::<Vec<_>>();
    access.host.lock().unwrap().sessions = sessions;
    let remote = client.sessions("host-1").await.unwrap();
    assert_eq!(remote.sessions.len(), 500);
    assert_eq!(remote.sessions[0].session_id, "session-499");
    assert_eq!(remote.sessions[499].session_id, "session-000");

    access.host.lock().unwrap().host_online = false;
    let cached = client.sessions("host-1").await.unwrap();
    assert_eq!(cached.sessions.len(), 500);
    assert_eq!(cached.sessions[0].session_id, "session-499");
    assert_eq!(cached.sessions[499].session_id, "session-000");
}

// SESS-15
#[tokio::test]
async fn session_titles_and_host_ids_round_trip_without_control_character_loss() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    let title = "tab\tnewline\nunicode 你好 \"quoted\"";
    let large = "x".repeat(4096);
    access.host.lock().unwrap().sessions = vec![
        proto_session("session-tab", title, 30),
        proto_session("session-large", &large, 20),
    ];
    let remote = client.sessions("host-1").await.unwrap();
    assert!(remote.sessions.iter().any(|session| session.title == title));
    assert!(remote.sessions.iter().any(|session| session.title == large));

    access.host.lock().unwrap().host_online = false;
    let cached = client.sessions("host-1").await.unwrap();
    assert!(cached.sessions.iter().any(|session| session.title == title));
    assert!(cached.sessions.iter().any(|session| session.title == large));
}

// SESS-10 / SESS-16
#[tokio::test]
async fn repeated_forks_form_a_parent_child_chain_in_session_listing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    let first = client.fork_session("session-1").await.unwrap();
    let first = first.lifecycle_receipt.unwrap();
    assert_eq!(first.session_id, "forked-1");

    access.host.lock().unwrap().fork_reply = LifecycleReply::Ready {
        session_id: "forked-2".into(),
    };
    let second = client.fork_session("forked-1").await.unwrap();
    let second = second.lifecycle_receipt.unwrap();
    assert_eq!(second.parent_session_id.as_deref(), Some("forked-1"));

    access.host.lock().unwrap().sessions = vec![
        proto_session("session-1", "parent", 30),
        {
            let mut child = proto_session("forked-1", "child", 20);
            child.parent_session_id = "session-1".into();
            child.origin = "fork".into();
            child
        },
        {
            let mut grandchild = proto_session("forked-2", "grandchild", 10);
            grandchild.parent_session_id = "forked-1".into();
            grandchild.origin = "fork".into();
            grandchild
        },
    ];
    let list = client.sessions("host-1").await.unwrap();
    assert_eq!(list.sessions.len(), 3);
    assert_eq!(list.sessions[1].parent_session_id, "session-1");
    assert_eq!(list.sessions[2].parent_session_id, "forked-1");
    assert_eq!(access.host.lock().unwrap().fork_calls, 2);
}

// SESS-17
#[tokio::test]
async fn explicit_sessions_host_must_agree_with_the_global_host_scope() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().extra_host = true;
    let mut config = client.current_config().unwrap();
    config
        .host_fingerprints
        .insert("host-2".into(), config.host_fingerprint.clone().unwrap());
    client.config_store().save(&config).unwrap();
    *client.config.write().unwrap() = Some(config);

    let scoped = client.with_session_host("host-2").unwrap();
    let list = scoped.sessions("host-2").await.unwrap();
    assert_eq!(list.sessions[0].session_id, "session-1");
    let error = scoped.sessions("host-1").await.unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)), "{error:?}");
    assert_eq!(error.code(), "conflict");
    assert_eq!(error.exit_code(), 8);
    assert!(error.to_string().contains("--host"));

    // Without an explicit sessions Host the global scope is authoritative.
    let global = scoped.sessions("host-2").await.unwrap();
    assert_eq!(global.source, "remote");
}

// SESS-08
#[tokio::test]
async fn fork_reports_an_unreadable_child_working_directory_explicitly() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().fail_get_session_for =
        Some(("forked-1".into(), whalerein_proto::ErrorCode::NotFound));
    let outcome = client.fork_session_outcome("session-1").await.unwrap();
    assert_eq!(
        outcome
            .handle
            .lifecycle_receipt
            .as_ref()
            .unwrap()
            .disposition,
        "READY",
        "the fork itself still succeeded"
    );
    assert!(outcome.working_directory.is_none());
    assert_eq!(
        outcome.working_directory_error.as_deref(),
        Some("not_found")
    );

    access.host.lock().unwrap().fail_get_session_for = None;
    let outcome = client.fork_session_outcome("session-1").await.unwrap();
    assert_eq!(outcome.working_directory.as_deref(), Some("/work-child"));
    assert!(outcome.working_directory_error.is_none());
    // The plain handle API remains available for callers that do not present
    // the child location.
    let handle = client.fork_session("session-1").await.unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
}

#[test]
fn host_absolute_path_rules_cover_posix_and_windows_forms() {
    for value in ["/work", "/", "C:\\work", "c:/work", "\\\\server\\share"] {
        assert!(is_absolute_host_path(value), "{value}");
    }
    for value in [
        "",
        "relative",
        "./relative",
        "../up",
        "C:relative",
        "~/home",
    ] {
        assert!(!is_absolute_host_path(value), "{value}");
    }
}

// JRNY-20
#[tokio::test]
async fn unqualified_session_resolution_names_the_candidate_hosts() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().extra_host = true;
    let mut config = client.current_config().unwrap();
    config
        .host_fingerprints
        .insert("host-2".into(), config.host_fingerprint.clone().unwrap());
    client.config_store().save(&config).unwrap();
    *client.config.write().unwrap() = Some(config);

    // Both Hosts report the same source session id, so an unqualified resolve
    // cannot choose one.
    let error = client.session_key("session-1").await.unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)), "{error:?}");
    assert_eq!(error.exit_code(), 8);
    let message = error.to_string();
    assert!(message.contains("host-1"), "{message}");
    assert!(message.contains("host-2"), "{message}");
    assert!(message.contains("--host"), "{message}");
    assert!(
        !message.contains("session-1"),
        "the ambiguity message must not repeat session content: {message}"
    );

    let scoped = client.with_session_host("host-2").unwrap();
    assert_eq!(
        scoped.session_key("session-1").await.unwrap().host_id,
        "host-2"
    );
}

// PAIR-16
#[tokio::test]
async fn unqualified_resolution_aborts_when_revocation_lands_mid_resolution() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let client = paired(&access, &dir).await;
    access.host.lock().unwrap().extra_host = true;
    let mut config = client.current_config().unwrap();
    config
        .host_fingerprints
        .insert("host-2".into(), config.host_fingerprint.clone().unwrap());
    client.config_store().save(&config).unwrap();
    *client.config.write().unwrap() = Some(config);

    // Discovery succeeds with two Hosts; the device is revoked before the first
    // per-Host session listing can complete.
    access.host.lock().unwrap().revoke_after_hosts = true;
    let error = client.session_key("session-1").await.unwrap_err();
    assert!(
        matches!(
            error,
            CoreError::Revoked
                | CoreError::RevokedRetentionUnverified
                | CoreError::NotPaired
                | CoreError::Unauthorized
        ),
        "a revoked resolve must not return a key or an ambiguity hint: {error:?}"
    );
    assert!(matches!(
        error,
        CoreError::Revoked | CoreError::RevokedRetentionUnverified | CoreError::NotPaired
    ));
    assert!(!client.is_paired(), "confirmed revocation clears pairing");
}
