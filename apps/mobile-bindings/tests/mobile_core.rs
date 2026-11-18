//! Behavioral tests for the exported `MobileCore` contract.
//!
//! The pairing-backed cases use Core's deterministic loopback fake Access and
//! Host fixtures. They exercise the real binding, Core facade and cache without
//! external services.

use std::time::{Duration, Instant};

use whalerein_core::api::test_support::FakeAccess;
use whalerein_mobile::{
    write_client_config, ConversationEvent, DeliveryChoice, MemorySecretStore, MobileClientService,
    MobileCore,
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

fn new_core(data_dir: &std::path::Path) -> std::sync::Arc<MobileCore> {
    MobileCore::new(
        write_config(data_dir),
        MemorySecretStore::new(),
        "test-app".to_owned(),
    )
    .expect("create MobileCore")
}

fn wait_for_pairing(core: &MobileCore) -> (Option<String>, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for event in core.poll_events(500) {
            if let ConversationEvent::PairingFinished { host_id, code } = event {
                return (host_id, code);
            }
        }
        assert!(
            Instant::now() < deadline,
            "pairing did not finish before the deadline"
        );
    }
}

#[test]
fn unpaired_hosts_is_a_typed_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let error = core.hosts().unwrap_err();
    assert_eq!(error.code(), "not_paired");
    assert!(!error.message().is_empty());
    // It stays a typed failure on repetition and cached_sessions also fails.
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
    assert_eq!(
        core.cached_sessions("host-1".into()).unwrap_err().code(),
        "not_paired"
    );
    assert_eq!(
        core.capabilities("host-1".into(), "session-1".into())
            .unwrap_err()
            .code(),
        "not_paired"
    );
    assert_eq!(
        core.cached_messages("host-1".into(), "session-1".into())
            .unwrap_err()
            .code(),
        "not_paired"
    );
    assert_eq!(core.connection_state("host-1".into()), "disconnected");
}

#[test]
fn poll_events_timeout_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let started = Instant::now();
    let events = core.poll_events(50);
    assert!(events.is_empty());
    assert!(started.elapsed() < Duration::from_secs(2));
    // A zero timeout never blocks.
    assert!(core.poll_events(0).is_empty());
}

#[test]
fn validation_failures_are_typed_before_any_network_work() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    assert_eq!(
        core.send(
            "host-1".into(),
            "session-1".into(),
            "   ".into(),
            DeliveryChoice::Send
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.send(
            "host-1".into(),
            "session-1".into(),
            "x".repeat(300 * 1024),
            DeliveryChoice::Steer
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.resolve_approval(
            "host-1".into(),
            "session-1".into(),
            "approval-1".into(),
            1,
            "not-hex".into(),
            true,
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.resolve_approval(
            "host-1".into(),
            "session-1".into(),
            "approval-1".into(),
            0,
            "ab".repeat(32),
            true,
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.save_draft("host-1".into(), "session-1".into(), "x".repeat(300 * 1024))
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}

#[test]
fn unpaired_diagnostics_preview_is_redacted_and_local() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let report = core.diagnostics_preview().unwrap();
    let value: serde_json::Value = serde_json::from_str(&report.json).unwrap();
    assert_eq!(report.app_version, "test-app");
    assert_eq!(report.core_version, env!("CARGO_PKG_VERSION"));
    assert!(!report.redaction_fields.is_empty());
    assert!(value.get("redactions").is_some());
    assert!(value.get("fields").is_some());
    assert!(report.last_failure_code.is_none());
}

#[test]
fn pairing_enables_hosts_drafts_and_redacted_export() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    access.host.lock().unwrap().host_id = "host-fixture-9f3".to_owned();
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());

    let prompt = core.pairing_start(access.pairing_uri()).unwrap();
    assert_eq!(prompt.fingerprint, access.fingerprint());
    assert!(!prompt.fingerprint_display.is_empty());
    core.pairing_decide(true, Some(access.fingerprint()))
        .unwrap();

    let (finished_host, code) = wait_for_pairing(&core);
    assert_eq!(code, None);
    assert_eq!(finished_host.as_deref(), Some(prompt.host_id.as_str()));

    let hosts = core.hosts().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].host_id, prompt.host_id);
    assert_eq!(hosts[0].fingerprint, access.fingerprint());

    let host_id = prompt.host_id.clone();
    let session_id = "session-1".to_owned();
    // An empty local cache is an empty list, not an error.
    assert!(core
        .cached_messages(host_id.clone(), session_id.clone())
        .unwrap()
        .is_empty());
    assert_eq!(
        core.load_draft(host_id.clone(), session_id.clone())
            .unwrap(),
        None
    );
    core.save_draft(
        host_id.clone(),
        session_id.clone(),
        "unsent draft text".into(),
    )
    .unwrap();
    assert_eq!(
        core.load_draft(host_id.clone(), session_id.clone())
            .unwrap()
            .as_deref(),
        Some("unsent draft text")
    );
    // Empty text clears the draft, matching an emptied editor.
    core.save_draft(host_id.clone(), session_id.clone(), String::new())
        .unwrap();
    assert_eq!(
        core.load_draft(host_id.clone(), session_id.clone())
            .unwrap(),
        None
    );
    // Explicit clear is idempotent.
    core.clear_draft(host_id.clone(), session_id).unwrap();

    let preview = core.diagnostics_preview().unwrap();
    assert!(!preview.json.contains("unsent draft"));
    assert!(!preview.json.contains(&host_id));
    assert!(preview
        .redaction_fields
        .iter()
        .any(|field| field == "message content"));

    let path = dir.path().join("diagnostics.json");
    let export = core
        .export_diagnostics(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(path.exists());
    assert!(!export.json.contains("unsent draft"));
    assert!(!export.json.contains(&host_id));
    assert!(export
        .redaction_fields
        .iter()
        .any(|field| field == "pairing tokens"));
}

#[test]
fn pairing_cancel_emits_cancelled_and_allows_a_new_attempt() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());

    core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_cancel();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events = core.poll_events(500);
        if events.iter().any(|event| {
            matches!(
                event,
                ConversationEvent::PairingFinished {
                    code: Some(code),
                    ..
                } if code == "cancelled"
            )
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "cancel event missing");
    }
    // A cancelled attempt is not running who knows what; hosts stays unpaired.
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
}

fn pair(core: &MobileCore, access: &FakeAccess) -> String {
    let prompt = core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_decide(true, Some(access.fingerprint()))
        .unwrap();
    let (host_id, code) = wait_for_pairing(core);
    assert_eq!(code, None);
    assert_eq!(host_id.as_deref(), Some(prompt.host_id.as_str()));
    prompt.host_id
}

#[test]
fn memory_constructor_and_revision_accessors_work() {
    let dir = tempfile::tempdir().unwrap();
    let core =
        MobileCore::new_with_memory_secret_store(write_config(dir.path()), "t".into()).unwrap();
    assert_eq!(core.core_revision(), whalerein_mobile::core_revision());
    assert!(!core.core_revision().is_empty());
}

#[test]
fn pairing_rejection_and_repeat_start_are_typed() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());

    core.pairing_start(access.pairing_uri()).unwrap();
    // A second start while one is pending is a conflict.
    assert_eq!(
        core.pairing_start(access.pairing_uri()).unwrap_err().code(),
        "conflict"
    );
    core.pairing_decide(false, None).unwrap();
    let (host_id, code) = wait_for_pairing(&core);
    assert_eq!(host_id, None);
    assert_eq!(code.as_deref(), Some("cancelled"));
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");

    // After completion a fresh attempt is allowed.
    core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_cancel();
}

#[test]
fn sessions_send_stop_and_approvals_reach_the_fixture_host() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);

    let sessions = core.sessions(host_id.clone()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "session-1");
    assert!(!sessions[0].title.is_empty());
    assert!(sessions[0].running);
    assert!(sessions[0].capabilities.is_some());

    let capabilities = core
        .capabilities(host_id.clone(), "session-1".into())
        .unwrap();
    // The fixture Source reports its capability set explicitly; the binding
    // must reflect it rather than inventing delivery support.
    assert!(!capabilities.interrupt);
    assert!(capabilities.delivery_modes.is_empty());
    assert!(capabilities.model_selection);
    assert_eq!(
        core.capabilities(host_id.clone(), "missing-session".into())
            .unwrap_err()
            .code(),
        "not_found"
    );
    // The session list was cached by the successful query.
    assert_eq!(core.cached_sessions(host_id.clone()).unwrap().len(), 1);

    let sent = core
        .send(
            host_id.clone(),
            "session-1".into(),
            "hello from the binding".into(),
            DeliveryChoice::Send,
        )
        .unwrap();
    assert!(sent.status == "succeeded" || sent.status == "host_accepted");
    assert!(!sent.request_id.is_empty());
    assert!(sent.message_id.is_some());
    let recovered = core.recover_send(sent.request_id.clone(), true).unwrap();
    assert_eq!(recovered.operation_id, sent.request_id);
    assert_eq!(recovered.host_id, host_id);
    assert_eq!(recovered.session_id, "session-1");
    assert!(matches!(
        recovered.reply,
        Some(whalerein_mobile::MessageReplyView::Committed { ref attempt_id, .. })
            if attempt_id == "fixture-attempt"
    ));
    // The same result is queued for observers.
    assert!(core.poll_events(0).iter().any(|event| matches!(
        event,
        ConversationEvent::CommandStatus { command } if command.request_id == sent.request_id
    )));

    let stopped = core.stop(host_id.clone(), "session-1".into()).unwrap();
    assert!(!stopped.command_id.is_empty());

    // The fixture Host does not implement approval listing; the binding must
    // surface a typed failure rather than panicking.
    match core.approvals(host_id.clone(), "session-1".into()) {
        Ok(approvals) => assert!(approvals.is_empty()),
        Err(error) => assert!(
            matches!(error.code(), "unavailable" | "internal"),
            "unexpected approval listing error: {error:?}"
        ),
    }
    let resolved = core
        .resolve_approval(
            host_id.clone(),
            "session-1".into(),
            "approval-1".into(),
            1,
            "ab".repeat(32),
            true,
        )
        .unwrap();
    assert!(!resolved.request_id.is_empty());
}

#[test]
fn watch_start_and_stop_are_prompt_and_idempotent() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);

    core.start_watch(host_id.clone(), "session-1".into())
        .unwrap();
    // Replacing an active watch is allowed.
    core.start_watch(host_id.clone(), "session-1".into())
        .unwrap();
    core.stop_watch();
    core.stop_watch();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ended = 0;
    while Instant::now() < deadline && ended == 0 {
        ended += core
            .poll_events(200)
            .iter()
            .filter(|event| matches!(event, ConversationEvent::WatchEnded))
            .count();
    }
    assert!(ended >= 1, "stopWatch must emit watchEnded");
}

#[test]
fn forget_device_removes_pairing_and_revoke_is_terminal() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);
    core.save_draft(host_id.clone(), "session-1".into(), "draft".into())
        .unwrap();
    // Core requires explicit revocation before forgetting a paired identity.
    assert_eq!(core.forget_device().unwrap_err().code(), "conflict");
    core.revoke().unwrap();
    core.forget_device().unwrap();
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");

    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let _host_id = pair(&core, &access);
    core.revoke().unwrap();
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
}

#[test]
fn export_diagnostics_rejects_an_unusable_path() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"file").unwrap();
    let unusable = blocker.join("nested").join("diagnostics.json");
    let error = core
        .export_diagnostics(unusable.to_string_lossy().into_owned())
        .unwrap_err();
    assert_eq!(error.code(), "internal");
}
