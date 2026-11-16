//! Core queue contract: response scope/type validation and terminal mapping.

use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{OnlineStatus, PairingDecision};

async fn paired() -> (FakeAccess, tempfile::TempDir, Core) {
    let access = FakeAccess::start().await;
    let directory = tempfile::tempdir().unwrap();
    let (core, _secrets) = core(directory.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    (access, directory, core)
}

#[tokio::test]
async fn pending_queue_rejects_a_scope_mismatch_and_an_unexpected_payload() {
    let (access, _directory, core) = paired().await;
    let queue = core.pending_queue("session-1").await.unwrap();
    assert_eq!(queue.session_id, "session-1");
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].item_id, "queue-1");

    // A response that names another session must never be accepted as this
    // session's queue, even when the payload is otherwise well formed.
    access.host.lock().unwrap().pending_queue_session_override = Some("session-2".into());
    let error = core.pending_queue("session-1").await.unwrap_err();
    assert!(
        matches!(error, CoreError::Internal(ref message) if message.contains("invalid pending queue response")),
        "unexpected scope error: {error:?}"
    );

    access.host.lock().unwrap().pending_queue_session_override = None;
    access.host.lock().unwrap().pending_queue_wrong_payload = true;
    let error = core.pending_queue("session-1").await.unwrap_err();
    assert!(
        matches!(error, CoreError::Internal(ref message) if message.contains("invalid pending queue response")),
        "unexpected payload error: {error:?}"
    );
}

#[tokio::test]
async fn update_pending_queue_maps_host_accepted_to_the_terminal_result() {
    let (access, _directory, core) = paired().await;
    // The first result poll sees host_accepted; the accepted control must be
    // converted into the host's terminal status rather than returned as-is.
    access.host.lock().unwrap().pending_result_polls = 1;
    let handle = core
        .update_pending_queue("session-1", "queue-1", PendingQueueAction::Edit, "fixture")
        .await
        .unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    assert!(!handle.request_id.is_empty());
    assert!(!handle.command_id.is_empty());
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(core.pending_commands().unwrap().is_empty());

    // A terminal failure keeps the exact host code and its client exit value.
    access.host.lock().unwrap().settled_command_status = whalerein_proto::OnlineStatus::Failed;
    access.host.lock().unwrap().settled_error_code = Some(whalerein_proto::ErrorCode::Conflict);
    let error = core
        .update_pending_queue("session-1", "queue-1", PendingQueueAction::Remove, "")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert_eq!(error.exit_code(), 8);
    assert_eq!(access.host.lock().unwrap().commands_seen, 2);
}
