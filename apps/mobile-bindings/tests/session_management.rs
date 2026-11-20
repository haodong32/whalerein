//! Behavioral tests for the exported session-management surface.
//!
//! The cases use Core's deterministic loopback fake Access and Host fixtures.
//! They exercise the real binding and Core facade without external services.

use std::time::{Duration, Instant};

use whalerein_core::api::test_support::{FakeAccess, LifecycleReply};
use whalerein_mobile::{
    write_client_config, ConversationEvent, MemorySecretStore, MobileClientService, MobileCore,
    SessionCreateInputView,
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

fn pair(core: &MobileCore, access: &FakeAccess) -> String {
    let prompt = core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_decide(true, Some(access.fingerprint()))
        .unwrap();
    let (_host_id, code) = wait_for_pairing(core);
    assert_eq!(code, None);
    prompt.host_id
}

/// Pairs against a fresh fake Host and returns the Core plus host id.
///
/// The Tokio runtime must outlive the fake Access server, so it stays with the
/// caller as a leaked static for the duration of the test.
fn paired_core(dir: &tempfile::TempDir) -> (std::sync::Arc<MobileCore>, FakeAccess, String) {
    let runtime = Box::leak(Box::new(tokio::runtime::Runtime::new().unwrap()));
    let access = runtime.block_on(FakeAccess::start());
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);
    (core, access, host_id)
}

fn create_input() -> SessionCreateInputView {
    SessionCreateInputView {
        cwd: None,
        workspace_id: None,
        agent_preset: None,
        preferred_session_id: Some("session-preferred".into()),
    }
}

// AC-01
#[test]
fn create_returns_the_native_identity_and_forwards_inputs_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().session_create_supported = true;
    assert!(core.session_create_supported(host_id.clone()).unwrap());

    let result = core
        .create_session(host_id.clone(), create_input())
        .unwrap();
    assert_eq!(result.session_id.as_deref(), Some("created-1"));
    assert_eq!(result.disposition, "READY");
    assert_eq!(result.warning_code, None);
    assert_eq!(result.receipt.status, "succeeded");
    assert!(!result.receipt.command_id.is_empty());
    let host = access.host.lock().unwrap();
    assert_eq!(host.create_calls, 1);
    assert_eq!(
        host.last_create_input
            .as_ref()
            .and_then(|input| input.preferred_session_id.as_deref()),
        Some("session-preferred"),
        "the preferred identity must reach the Host unchanged"
    );
}

// AC-01 warning
#[test]
fn create_warning_stays_a_warning_with_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().session_create_supported = true;
    access.host.lock().unwrap().create_reply = LifecycleReply::Warning {
        session_id: "created-warn".into(),
        warning_code: "workspace_attach_failed".into(),
        warning_detail: Some("attach incomplete".into()),
    };
    let result = core.create_session(host_id, create_input()).unwrap();
    assert_eq!(result.disposition, "READY_WITH_WARNING");
    assert_eq!(result.session_id.as_deref(), Some("created-warn"));
    assert_eq!(
        result.warning_code.as_deref(),
        Some("workspace_attach_failed")
    );
    // The lifecycle detail is not flattened into a failure message.
    assert_eq!(result.reason_code, None);
}

// AC-01 proven rejection
#[test]
fn create_failure_reports_reason_and_retryability_without_an_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().session_create_supported = true;
    access.host.lock().unwrap().create_reply = LifecycleReply::Failed {
        reason_code: "WORKSPACE_NOT_FOUND".into(),
        retryable: true,
    };
    let result = core.create_session(host_id, create_input()).unwrap();
    assert_eq!(result.session_id, None);
    assert_eq!(result.disposition, "FAILED");
    assert_eq!(result.reason_code.as_deref(), Some("WORKSPACE_NOT_FOUND"));
    assert!(result.retryable);
}

// AC-01 unknown/reconcile
#[test]
fn create_unknown_preserves_the_reconcile_contract() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().session_create_supported = true;
    access.host.lock().unwrap().create_reply = LifecycleReply::Unknown {
        reason_code: "NATIVE_INTERRUPTED".into(),
        reconcile_mode: "PREFERRED_SESSION_ID".into(),
        reconcile_key: Some("session-preferred".into()),
    };
    let result = core.create_session(host_id, create_input()).unwrap();
    assert_eq!(result.session_id, None);
    assert_eq!(result.disposition, "UNKNOWN");
    assert_eq!(
        result.reconcile_mode.as_deref(),
        Some("PREFERRED_SESSION_ID")
    );
    assert_eq!(result.reconcile_key.as_deref(), Some("session-preferred"));
    assert_eq!(access.host.lock().unwrap().create_calls, 1);
}

// AC-01 invalid input
#[test]
fn create_rejects_invalid_values_before_submitting_a_lifecycle_command() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().session_create_supported = true;
    let invalid = [
        SessionCreateInputView {
            cwd: Some("relative/worktree".into()),
            ..create_input()
        },
        SessionCreateInputView {
            cwd: Some("/work".into()),
            workspace_id: Some("workspace-1".into()),
            ..create_input()
        },
        SessionCreateInputView {
            agent_preset: Some("   ".into()),
            ..create_input()
        },
    ];
    for input in invalid {
        let error = core
            .create_session(host_id.clone(), input)
            .expect_err("invalid create input");
        assert_eq!(error.code(), "invalid_input");
    }
    assert_eq!(
        access.host.lock().unwrap().create_calls,
        0,
        "Core's local preflight must run before the lifecycle submission"
    );
}

// AC-01 capability
#[test]
fn create_reports_a_missing_capability_without_submitting() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    assert!(!core.session_create_supported(host_id.clone()).unwrap());
    let error = core
        .create_session(host_id, create_input())
        .expect_err("unsupported create");
    assert_eq!(error.code(), "unsupported_capability");
    assert_eq!(access.host.lock().unwrap().create_calls, 0);
}

// AC-02
#[test]
fn session_details_carry_the_host_and_session_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _access, host_id) = paired_core(&dir);
    let detail = core
        .session_details(host_id.clone(), "session-1".into())
        .unwrap();
    assert_eq!(detail.host_id, host_id);
    assert_eq!(detail.session_id, "session-1");
    assert!(!detail.title.is_empty());
    assert!(!detail.cwd.is_empty());
    assert!(detail.capabilities.is_some());
}

// AC-03
#[test]
fn fork_reports_the_confirmed_child_and_shared_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    let result = core
        .fork_session_outcome(host_id.clone(), "session-1".into())
        .unwrap();
    assert_eq!(result.session_id.as_deref(), Some("forked-1"));
    assert_eq!(result.parent_session_id.as_deref(), Some("session-1"));
    assert_eq!(result.disposition, "READY");
    assert_eq!(result.working_directory.as_deref(), Some("/work-child"));
    assert_eq!(result.working_directory_error, None);
    assert_eq!(result.receipt.status, "succeeded");
    let host = access.host.lock().unwrap();
    assert_eq!(host.fork_calls, 1);
    assert_eq!(host.last_fork_parent.as_deref(), Some("session-1"));
}

// AC-03 directory unavailable
#[test]
fn fork_keeps_the_confirmed_child_when_the_directory_read_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().fail_get_session_for =
        Some(("forked-1".into(), whalerein_proto::ErrorCode::NotFound));
    let result = core
        .fork_session_outcome(host_id, "session-1".into())
        .unwrap();
    assert_eq!(result.session_id.as_deref(), Some("forked-1"));
    assert_eq!(result.disposition, "READY");
    assert_eq!(result.working_directory, None);
    assert_eq!(result.working_directory_error.as_deref(), Some("not_found"));
}

// AC-03 unsupported
#[test]
fn fork_reports_a_missing_capability() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    access.host.lock().unwrap().session_fork_supported = false;
    let error = core
        .fork_session_outcome(host_id, "session-1".into())
        .expect_err("unsupported fork");
    assert_eq!(error.code(), "unsupported_capability");
    assert_eq!(access.host.lock().unwrap().fork_calls, 0);
}

// AC-04
#[test]
fn model_catalog_and_passive_read_use_the_source_values() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    let catalog = core.model_catalog(host_id.clone()).unwrap();
    assert!(catalog.complete);
    assert_eq!(catalog.effect_scope, "session_and_source_default_attempt");
    assert_eq!(catalog.default_selection.as_ref().unwrap().model, "model");
    assert!(catalog.failures.is_empty());

    access.host.lock().unwrap().model_observed = Some("model".into());
    let model = core
        .read_session_model(host_id.clone(), "session-1".into())
        .unwrap();
    assert!(model.default_is_fallback);
    assert!(model.pending_known);
    assert_eq!(model.next.as_ref().unwrap().model, "model");
    assert_eq!(model.default_selection.as_ref().unwrap().model, "model");

    access.host.lock().unwrap().model_selection_available = false;
    assert_eq!(
        core.model_catalog(host_id.clone()).unwrap_err().code(),
        "unsupported_capability"
    );
    assert_eq!(
        core.read_session_model(host_id, "session-1".into())
            .unwrap_err()
            .code(),
        "unsupported_capability"
    );
}

// AC-05
#[test]
fn model_selection_reports_session_acceptance_and_unconfirmed_default() {
    let dir = tempfile::tempdir().unwrap();
    let (core, access, host_id) = paired_core(&dir);
    let result = core
        .select_session_model(
            host_id.clone(),
            "session-1".into(),
            "provider".into(),
            "model".into(),
            Some("high".into()),
        )
        .unwrap();
    let selected = result.selected.expect("selected choice");
    assert_eq!(selected.provider, "provider");
    assert_eq!(selected.model, "model");
    assert_eq!(selected.effort.as_deref(), Some("high"));
    assert_eq!(
        result.effect_scope.as_deref(),
        Some("session_and_source_default_attempt")
    );
    assert_eq!(result.default_persistence.as_deref(), Some("unconfirmed"));
    assert_eq!(result.receipt.status, "succeeded");
    assert!(!result.receipt.command_id.is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);

    // Re-selecting the effective next model is a Core no-op with no command
    // identity; the receipt must not fabricate one.
    access.host.lock().unwrap().model_observed = Some("model".into());
    let before = access.host.lock().unwrap().commands_seen;
    let noop = core
        .select_session_model(
            host_id,
            "session-1".into(),
            "provider".into(),
            "model".into(),
            None,
        )
        .unwrap();
    assert!(noop.receipt.command_id.is_empty());
    assert_eq!(noop.receipt.status, "succeeded");
    assert_eq!(noop.default_persistence.as_deref(), Some("unconfirmed"));
    assert_eq!(access.host.lock().unwrap().commands_seen, before);
}
