use super::{run_with_signal, Cli, CliOutcome};
use crate::diagnostics::CliDiagnosticSink;
use clap::Parser;
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use whalerein_core::api::{
    test_support::{FakeAccess, LifecycleReply},
    ClientConfig, ClientService, DiagnosticContext, MemorySecretStore, PairingDecision,
    SecretStore,
};
use whalerein_core::Core;
use whalerein_proto::{Approval, ApprovalDecision, ApprovalState, ErrorCode, MessageReplyState};

pub(super) struct Fixture {
    pub(super) directory: TempDir,
    config: PathBuf,
    secrets: Arc<dyn SecretStore>,
    pub(super) access: FakeAccess,
}

impl Fixture {
    pub(super) async fn paired() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let access = FakeAccess::start().await;
        let core = Core::open(&config, secrets.clone()).unwrap();
        core.connect_standalone(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        Self {
            directory,
            config,
            secrets,
            access,
        }
    }

    pub(super) async fn run(&self, args: &[&str]) -> CliOutcome {
        self.run_with_interrupt(args, std::future::pending()).await
    }

    pub(super) async fn run_with_interrupt(
        &self,
        args: &[&str],
        signal: impl std::future::Future<Output = ()>,
    ) -> CliOutcome {
        let cli = Cli::try_parse_from(
            [
                "whalerein",
                "--config",
                self.config.to_str().unwrap(),
                "--json",
            ]
            .into_iter()
            .chain(args.iter().copied()),
        )
        .unwrap();
        let diagnostics = CliDiagnosticSink::disabled();
        Box::pin(run_with_signal(
            cli,
            self.secrets.clone(),
            signal,
            &diagnostics,
            DiagnosticContext::new(diagnostics.clone()),
            &mut std::io::sink(),
        ))
        .await
    }
}

#[tokio::test]
async fn grouped_lifecycle_create_preserves_capability_and_receipts() {
    let f = Fixture::paired().await;
    let args = ["session", "create", "--host", "host-1", "--cwd", "/work"];
    let denied = f.run(&args).await;
    assert_eq!(denied.exit, 9, "{denied:?}");
    assert_eq!(f.access.host.lock().unwrap().commands_seen, 0);

    f.access.host.lock().unwrap().session_create_supported = true;
    let ready = f.run(&args).await;
    assert_eq!(ready.exit, 0, "{ready:?}");
    assert_eq!(ready.to_json()["command"], "session.create");
    assert_eq!(
        ready.to_json()["data"]["lifecycle_receipt"]["session_id"],
        "created-1"
    );
    assert!(ready.human.contains("Session created-1 is ready."));

    f.access.host.lock().unwrap().create_reply = LifecycleReply::Warning {
        session_id: "created-2".into(),
        warning_code: "workspace_attach_failed".into(),
        warning_detail: None,
    };
    let warned = f.run(&args).await;
    assert_eq!(
        warned.to_json()["data"]["lifecycle_receipt"]["disposition"],
        "READY_WITH_WARNING"
    );
    assert!(warned.human.contains("do not retry or delete it"));

    f.access.host.lock().unwrap().create_reply = LifecycleReply::Failed {
        reason_code: "WORKSPACE_NOT_FOUND".into(),
        retryable: true,
    };
    let failed = f
        .run(&[
            "session",
            "create",
            "--workspace",
            "missing",
            "--host",
            "host-1",
        ])
        .await;
    assert_eq!(failed.exit, 3, "{failed:?}");
    assert_eq!(
        failed.to_json()["data"]["lifecycle_receipt"]["disposition"],
        "FAILED"
    );
}

#[tokio::test]
async fn grouped_lifecycle_fork_keeps_child_on_failed_summary() {
    let f = Fixture::paired().await;
    let args = ["session", "fork", "session-1", "--host", "host-1"];
    let ready = f.run(&args).await;
    assert_eq!(ready.exit, 0, "{ready:?}");
    assert_eq!(ready.to_json()["command"], "session.fork");
    assert_eq!(ready.to_json()["data"]["working_directory"], "/work-child");
    assert!(ready.human.contains("Directory: /work-child"));
    assert!(ready.human.contains("no worktree is created"));
    {
        let mut host = f.access.host.lock().unwrap();
        host.fork_reply = LifecycleReply::Ready {
            session_id: "forked-2".into(),
        };
        host.fail_get_session_for = Some(("forked-2".into(), ErrorCode::Unavailable));
    }
    let warned = f.run(&args).await;
    assert_eq!(warned.exit, 0, "{warned:?}");
    let data = &warned.to_json()["data"];
    assert_eq!(data["lifecycle_receipt"]["session_id"], "forked-2");
    assert_eq!(data["working_directory_error"], "unavailable");
    assert!(data.get("working_directory").is_none());
    assert!(warned
        .human
        .contains("child still exists and must not be retried or deleted"));
}

pub(super) fn approval(id: &str, state: ApprovalState) -> Approval {
    Approval {
        approval_id: id.into(),
        session_id: "session-1".into(),
        tool_name: "fixture".into(),
        call_id: format!("call-{id}"),
        reason: "review".into(),
        digest: vec![1; 32],
        version: 3,
        expires_at_unix_ms: 9,
        state: state as i32,
        decision: ApprovalDecision::Unspecified as i32,
        review_action: "Change one file".into(),
        review_target: "Sources/App.swift".into(),
        review_impact: "Replaces the selected file content.".into(),
        review_availability: whalerein_proto::ApprovalReviewAvailability::ApprovalReviewComplete
            as i32,
    }
}

#[tokio::test]
async fn grouped_approval_reads_scoped_list_and_terminal_states() {
    let f = Fixture::paired().await;
    let cases = [
        ("pending", ApprovalState::Pending, "APPROVAL_STATE_PENDING"),
        (
            "approved",
            ApprovalState::Approved,
            "APPROVAL_STATE_APPROVED",
        ),
        ("expired", ApprovalState::Expired, "APPROVAL_STATE_EXPIRED"),
        (
            "unavailable",
            ApprovalState::Unavailable,
            "APPROVAL_STATE_UNAVAILABLE",
        ),
    ];
    f.access.host.lock().unwrap().approvals = cases
        .iter()
        .map(|(id, state, _)| approval(id, *state))
        .collect();
    let list = f
        .run(&["session", "approval", "list", "--session", "session-1"])
        .await;
    assert_eq!(list.exit, 0, "{list:?}");
    assert_eq!(list.to_json()["data"]["session_id"], "session-1");
    assert_eq!(
        list.to_json()["data"]["approvals"][0]["approval_id"],
        "pending"
    );

    for (id, _, state) in cases {
        let got = f
            .run(&[
                "session",
                "approval",
                "list",
                "--request",
                id,
                "--host",
                "host-1",
            ])
            .await;
        assert_eq!(got.exit, 0, "{got:?}");
        let envelope = got.to_json();
        assert_eq!(envelope["command"], "session.approval.list");
        assert_eq!(envelope["data"]["approval"]["state"], state);
        assert!(envelope["data"].get("approvals").is_none());
        assert!(got.human.contains("version: 3"));
    }
    let missing = f
        .run(&["session", "approval", "list", "--request", "missing"])
        .await;
    assert_eq!(missing.exit, 3, "{missing:?}");
    assert_eq!(missing.to_json()["error"]["code"], "not_found");
    assert!(Cli::try_parse_from(["whalerein", "session", "approval", "list"]).is_err());
}

#[tokio::test]
async fn approval_review_renders_host_facts_and_states_limitation() {
    let f = Fixture::paired().await;
    {
        let mut host = f.access.host.lock().unwrap();
        host.approvals = vec![
            approval("complete", ApprovalState::Pending),
            {
                let mut review = approval("incomplete", ApprovalState::Pending);
                review.review_action = "Change [REDACTED] credential scope".into();
                review.review_target = "Sources/[REDACTED].swift".into();
                review.review_impact = String::new();
                review.review_availability =
                    whalerein_proto::ApprovalReviewAvailability::ApprovalReviewIncomplete as i32;
                review
            },
            {
                let mut absent = approval("absent", ApprovalState::Pending);
                absent.review_action = String::new();
                absent.review_target = String::new();
                absent.review_impact = String::new();
                absent.review_availability =
                    whalerein_proto::ApprovalReviewAvailability::ApprovalReviewUnspecified as i32;
                absent
            },
        ];
    }
    let before = f.access.host.lock().unwrap().approval_queries.len();
    let list = f
        .run(&["session", "approval", "list", "--session", "session-1"])
        .await;
    assert_eq!(list.exit, 0, "{list:?}");
    // The review facts already arrived with the one list response; rendering
    // never issues another approval query.
    assert_eq!(
        f.access.host.lock().unwrap().approval_queries.len(),
        before + 1
    );
    assert!(list.human.contains("review: complete"), "{}", list.human);
    assert!(list.human.contains("action: Change one file"));
    assert!(list
        .human
        .contains("impact: Replaces the selected file content."));
    // Values arrive masked from the Host and are rendered exactly, with no
    // reconstructed raw argument text.
    assert!(list
        .human
        .contains("action: Change [REDACTED] credential scope"));
    assert!(list.human.contains("target: Sources/[REDACTED].swift"));
    // Missing impact and unavailable default are never presented as a full
    // review, and no fact is guessed from the tool name or reason.
    assert!(list.human.contains("review: incomplete"), "{}", list.human);
    assert!(list.human.contains("review: unavailable"), "{}", list.human);
    // The limitation is advisory: the CLI no longer claims to withhold Allow,
    // because the resolution is submitted to the Host regardless.
    assert!(list.human.contains("review limitation:"), "{}", list.human);
    assert!(list.human.contains("not fully reviewed"), "{}", list.human);
    assert!(!list.human.contains("Allow is withheld"), "{}", list.human);
    assert!(!list.human.contains("target: fixture"));
    assert!(!list.human.contains("action: review"));

    // Reject remains available with the explicit digest/version guard while
    // approval presence does not change that requirement.
    let rejected = f
        .run(&[
            "session",
            "approval",
            "reject",
            "--request",
            "absent",
            "--digest",
            &"01".repeat(32),
            "--version",
            "3",
            "--host",
            "host-1",
        ])
        .await;
    assert_eq!(rejected.exit, 0, "{rejected:?}");
    assert_eq!(
        f.access.host.lock().unwrap().approval_resolutions.last(),
        Some(&(
            "host-1".into(),
            "absent".into(),
            ApprovalDecision::Reject as i32
        ))
    );
}

#[tokio::test]
async fn approval_approve_submits_incomplete_and_complete_reviews_without_a_detail_read() {
    let f = Fixture::paired().await;
    {
        let mut host = f.access.host.lock().unwrap();
        host.approvals = vec![
            {
                // Keep the complete fixture on the digest this test submits,
                // so the approved resolution matches the review it displays.
                let mut complete = approval("complete", ApprovalState::Pending);
                complete.digest = vec![0xab; 32];
                complete
            },
            {
                let mut incomplete = approval("incomplete", ApprovalState::Pending);
                incomplete.digest = vec![0xab; 32];
                incomplete.review_impact = String::new();
                incomplete.review_availability =
                    whalerein_proto::ApprovalReviewAvailability::ApprovalReviewIncomplete as i32;
                incomplete
            },
            {
                let mut unavailable = approval("unavailable", ApprovalState::Pending);
                unavailable.digest = vec![0xab; 32];
                unavailable.review_availability =
                    whalerein_proto::ApprovalReviewAvailability::ApprovalReviewUnavailable as i32;
                unavailable
            },
            {
                // A `complete` label cannot stand in for an empty essential
                // fact; the limitation display still calls it out, but it no
                // longer blocks the resolution.
                let mut empty_fact = approval("empty-fact", ApprovalState::Pending);
                empty_fact.digest = vec![0xab; 32];
                empty_fact.review_target = String::new();
                empty_fact
            },
        ];
    }
    let digest = "ab".repeat(32);
    for request in ["complete", "incomplete", "unavailable", "empty-fact"] {
        let (commands_before, queries_before, resolutions_before) = {
            let host = f.access.host.lock().unwrap();
            (
                host.commands_seen,
                host.approval_queries.len(),
                host.approval_resolutions.len(),
            )
        };
        let outcome = f
            .run(&[
                "session",
                "approval",
                "approve",
                "--request",
                request,
                "--digest",
                &digest,
                "--version",
                "3",
                "--host",
                "host-1",
            ])
            .await;
        assert_eq!(outcome.exit, 0, "{request}: {outcome:?}");
        let envelope = outcome.to_json();
        assert_eq!(envelope["command"], "session.approval.approve");
        assert_eq!(envelope["data"]["status"], "host_accepted");
        let host = f.access.host.lock().unwrap();
        // Exactly one resolution carrying the requested approval, decision and
        // the CLI's digest/version guard. The fake records the resolved
        // identity; the digest/version travel in the same ResolveApproval
        // request and are enforced by the Host (see native-dsh approval tests).
        assert_eq!(host.approval_resolutions.len(), resolutions_before + 1);
        assert_eq!(
            host.approval_resolutions.last(),
            Some(&(
                "host-1".into(),
                request.to_owned(),
                ApprovalDecision::Approve as i32
            )),
            "{request}"
        );
        assert_eq!(host.commands_seen, commands_before + 1, "{request}");
        // Approve must not pre-read the detail: review facts belong to the
        // list/detail read path only.
        assert_eq!(host.approval_queries.len(), queries_before, "{request}");
    }
    assert_eq!(f.access.host.lock().unwrap().approval_resolutions.len(), 4);
}

#[tokio::test]
async fn approval_approve_ignores_detail_read_failure_and_keeps_core_error_mapping() {
    let f = Fixture::paired().await;
    {
        let mut host = f.access.host.lock().unwrap();
        // A pre-read would fail here. Approve has no pre-read, so the
        // resolution still goes out exactly once.
        host.approval_error = Some(ErrorCode::Unavailable);
    }
    let commands_before = f.access.host.lock().unwrap().commands_seen;
    let outcome = f
        .run(&[
            "session",
            "approval",
            "approve",
            "--request",
            "request-1",
            "--digest",
            &"ab".repeat(32),
            "--version",
            "3",
            "--host",
            "host-1",
        ])
        .await;
    assert_eq!(outcome.exit, 0, "{outcome:?}");
    assert_eq!(outcome.to_json()["command"], "session.approval.approve");
    {
        let host = f.access.host.lock().unwrap();
        assert_eq!(
            host.approval_resolutions,
            vec![(
                "host-1".into(),
                "request-1".into(),
                ApprovalDecision::Approve as i32
            )]
        );
        assert_eq!(host.commands_seen, commands_before + 1);
        assert!(
            host.approval_queries.is_empty(),
            "{:?}",
            host.approval_queries
        );
    }

    // Core failures keep their normal mapping; there is no special
    // `approval_unavailable` rewrite left on the approve path.
    f.access.host.lock().unwrap().host_online = false;
    let offline = f
        .run(&[
            "session",
            "approval",
            "approve",
            "--request",
            "request-1",
            "--digest",
            &"ab".repeat(32),
            "--version",
            "3",
            "--host",
            "host-1",
        ])
        .await;
    assert_eq!(offline.exit, 5, "{offline:?}");
    assert_eq!(offline.to_json()["error"]["code"], "host_offline");
    let host = f.access.host.lock().unwrap();
    assert_eq!(host.approval_resolutions.len(), 1);
    assert_eq!(host.commands_seen, commands_before + 1);
    assert!(host.approval_queries.is_empty());
}

#[tokio::test]
async fn grouped_approval_actions_reject_invalid_digest_before_submission() {
    let f = Fixture::paired().await;
    // Seed a pending approval so both resolutions use the same visible request.
    f.access.host.lock().unwrap().approvals = vec![approval("request-1", ApprovalState::Pending)];
    let digest = "01".repeat(32);
    for (action, decision) in [
        ("approve", ApprovalDecision::Approve),
        ("reject", ApprovalDecision::Reject),
    ] {
        let before = f.access.host.lock().unwrap().commands_seen;
        let invalid = f
            .run(&[
                "session",
                "approval",
                action,
                "--request",
                "request-1",
                "--digest",
                "00",
                "--version",
                "3",
            ])
            .await;
        assert_eq!(invalid.exit, 2, "{invalid:?}");
        assert_eq!(invalid.to_json()["error"]["code"], "invalid_argument");
        assert_eq!(f.access.host.lock().unwrap().commands_seen, before);

        let accepted = f
            .run(&[
                "session",
                "approval",
                action,
                "--request",
                "request-1",
                "--digest",
                &digest,
                "--version",
                "3",
                "--host",
                "host-1",
            ])
            .await;
        assert_eq!(accepted.exit, 0, "{accepted:?}");
        assert_eq!(
            accepted.to_json()["command"],
            format!("session.approval.{action}")
        );
        assert_eq!(
            f.access.host.lock().unwrap().approval_resolutions.last(),
            Some(&("host-1".into(), "request-1".into(), decision as i32))
        );
    }
}

#[tokio::test]
async fn grouped_diagnostics_retains_failure_identity_and_redacts_bundle() {
    let f = Fixture::paired().await;
    let missing = f.run(&["host", "default", "missing-host"]).await;
    assert_eq!(missing.exit, 3, "{missing:?}");
    let value = missing.to_json();
    let diagnostic = value["error"]["diagnostic_id"]
        .as_str()
        .expect("failure ID");
    assert_eq!(diagnostic.len(), 32);
    assert_eq!(value["error"]["layer"], "core");
    assert!(missing.render().contains(diagnostic));

    let path = f.directory.path().join("diagnostics.json");
    let path = path.to_str().unwrap();
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        // A fresh path needs no second prompt, even noninteractive.
        let fresh = f.run(&["doctor", "--export", path]).await;
        assert_eq!(fresh.exit, 0, "{fresh:?}");
        let first_bytes = std::fs::read(path).unwrap();
        // Replacing the now-existing regular file needs affirmative consent.
        let denied = f.run(&["doctor", "--export", path]).await;
        assert_eq!(denied.exit, 2, "{denied:?}");
        assert_eq!(denied.to_json()["error"]["code"], "invalid_argument");
        assert_eq!(std::fs::read(path).unwrap(), first_bytes);
    }
    let invalid = f
        .run(&[
            "doctor",
            "--yes",
            "--export",
            f.directory.path().to_str().unwrap(),
        ])
        .await;
    assert_eq!(invalid.to_json()["error"]["code"], "invalid_argument");

    let exported = f.run(&["doctor", "--yes", "--export", path]).await;
    assert_eq!(exported.exit, 0, "{exported:?}");
    let read_bundle = || {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains(&f.access.pairing_uri()));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(value["redactions"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        value
    };
    let bundle = read_bundle();
    assert_eq!(bundle["last_failure"]["trace_id"], diagnostic);
    assert_eq!(
        bundle["last_failure"]["events"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["code"],
        "not_found"
    );
    let cleared = f.run(&["doctor", "--clear-diagnostics"]).await;
    assert_eq!(cleared.to_json()["data"]["diagnostics_cleared"], true);
    assert_eq!(f.run(&["doctor", "--yes", "--export", path]).await.exit, 0);
    assert!(read_bundle()["last_failure"].is_null());

    let status = f.run(&["status"]).await;
    assert_eq!(status.to_json()["data"]["default_host"], "host-1");
}

#[tokio::test]
async fn a_write_to_an_offline_host_reports_host_offline_without_submitting() {
    let f = Fixture::paired().await;
    f.access.host.lock().unwrap().host_online = false;
    let submitted = f.access.host.lock().unwrap().commands_seen;
    let outcome = f
        .run(&["session", "send", "session-1", "hello", "--host", "host-1"])
        .await;
    assert_eq!(outcome.exit, 5, "{outcome:?}");
    assert_eq!(outcome.to_json()["error"]["code"], "host_offline");
    assert_eq!(f.access.host.lock().unwrap().commands_seen, submitted);
}

#[tokio::test]
async fn grouped_device_revoke_and_forget_distinguish_remote_and_local_state() {
    let f = Fixture::paired().await;
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let denied = f.run(&["device", "revoke"]).await;
        assert_eq!(denied.exit, 2);
        assert!(!f.access.host.lock().unwrap().revoked);
    }
    let active = f.run(&["device", "forget", "--yes"]).await;
    assert_eq!(active.exit, 8, "{active:?}");
    assert!(!f.access.host.lock().unwrap().revoked);
    let revoke = f.run(&["device", "revoke", "--yes"]).await;
    assert_eq!(revoke.exit, 0, "{revoke:?}");
    assert_eq!(revoke.to_json()["data"]["revoked"], true);
    assert!(f.access.host.lock().unwrap().revoked);
    let forgotten = f.run(&["device", "forget", "--yes"]).await;
    assert_eq!(forgotten.exit, 0, "{forgotten:?}");
    assert_eq!(
        forgotten.to_json()["data"]["remote_grant_may_remain"],
        false
    );

    let isolated = Fixture::paired().await;
    let local = isolated
        .run(&["device", "forget", "--yes", "--local-only"])
        .await;
    assert_eq!(local.exit, 0, "{local:?}");
    assert_eq!(local.to_json()["data"]["identity_forgotten"], true);
    assert_eq!(local.to_json()["data"]["remote_grant_may_remain"], true);
    assert!(!isolated.access.host.lock().unwrap().revoked);
}

async fn observe_fixture(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !ready() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Host reached the interruption barrier");
}

#[tokio::test]
async fn grouped_wait_signal_cancels_observation_without_a_write_receipt() {
    let f = Fixture::paired().await;
    f.access.host.lock().unwrap().wait_timeouts = usize::MAX;
    let cancelled = f
        .run_with_interrupt(&["session", "wait", "session-1"], async {
            observe_fixture(|| f.access.host.lock().unwrap().wait_last_timeout_ms > 0).await;
        })
        .await;
    assert_eq!(cancelled.exit, 2, "{cancelled:?}");
    assert_eq!(cancelled.to_json()["error"]["code"], "cancelled");
    assert!(cancelled.to_json()["data"]["operation_id"].is_null());
}

#[tokio::test]
async fn grouped_send_signal_retains_uncertain_operation_without_echoing_message() {
    let f = Fixture::paired().await;
    // Keep the foreground send waiting even after it receives a message ID.
    f.access.host.lock().unwrap().message_reply_state =
        Some(MessageReplyState::MessageReplyPending);
    let signalled = Cell::new(false);
    let operation = "7".repeat(32);
    let message = "synthetic message for interrupted operation";
    let cancelled = f
        .run_with_interrupt(
            &[
                "session",
                "send",
                "session-1",
                message,
                "--operation-id",
                &operation,
            ],
            async {
                observe_fixture(|| f.access.host.lock().unwrap().commands_seen > 0).await;
                signalled.set(true);
            },
        )
        .await;
    assert!(signalled.get(), "send finished before the interruption");
    assert_eq!(cancelled.exit, 7);
    let value = cancelled.to_json();
    assert_eq!(value["error"]["code"], "unknown");
    assert_eq!(value["data"]["operation_id"], operation);
    assert_eq!(value["data"]["target"]["host_id"], "host-1");
    assert!(!value.to_string().contains(message));
    assert!(!cancelled.human.contains(message));
    let pending = f.run(&["operation", "list"]).await;
    assert_eq!(
        pending.to_json()["data"]["sends"][0]["operation_id"],
        operation
    );
}

/// HOST-3: an incomplete cache page must print a continuation command that
/// runs. The printed shape includes `--cache`, the parser accepts
/// `--cache --continuation`, and a cache-sourced token is only readable that way.
#[test]
fn cache_history_continuation_guidance_is_executable() {
    use whalerein_core::history::{HistoryContinuation, HistoryPage, HistorySource};

    let key = whalerein_core::api::SessionKey::new("host-1", "session-1").unwrap();
    let page = HistoryPage {
        source: HistorySource::Cache,
        projection: "projection-1".into(),
        complete: false,
        messages: Vec::new(),
        activities: Vec::new(),
        continuation: Some("cache-token".parse::<HistoryContinuation>().unwrap()),
    };
    let human = crate::output::render_history_page(&key, &page, false);
    assert!(
        human.contains("--cache"),
        "cached history must be continued with --cache:\n{human}"
    );

    let accepted = Cli::try_parse_from([
        "whalerein",
        "session",
        "history",
        "session-1",
        "--host",
        "host-1",
        "--cache",
        "--continuation",
        "cache-token",
    ]);
    assert!(
        accepted.is_ok(),
        "the printed cache continuation must be accepted: {accepted:?}"
    );
}
