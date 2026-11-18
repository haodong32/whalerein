//! Interrupt handling shared by every command.
//!
//! SIGINT and SIGTERM are equivalent. The process never dies from the default
//! handler: dispatch races the signal and reports a stable envelope instead.
//! A signal that arrives before a command was submitted is a local
//! cancellation (exit 2). A signal after submission is an unresolved outcome
//! (exit 7 with the retained command identity) because the Host may have
//! accepted work whose result was not observed.

use std::collections::BTreeSet;

use whalerein_core::api::PendingCommand;

use crate::cli::Command;
use crate::output::CliOutcome;

/// Register the OS signal handlers immediately and return a future that
/// resolves when either signal arrives.
///
/// Startup can take longer than the interval before a user's first Ctrl-C
/// (configuration parsing, profile resolution and cache opening). Registering
/// synchronously avoids the default process-killing disposition during that
/// window; the lazy `ctrl_c()` registration used by [`interrupt`] only happens
/// when the future is first polled.
pub fn registered_interrupt() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let interrupt = signal(SignalKind::interrupt()).ok();
        let terminate = signal(SignalKind::terminate()).ok();
        async move {
            match (interrupt, terminate) {
                (Some(mut interrupt), Some(mut terminate)) => {
                    tokio::select! {
                        _ = interrupt.recv() => {},
                        _ = terminate.recv() => {},
                    }
                }
                (Some(mut interrupt), None) => {
                    let _ = interrupt.recv().await;
                }
                (None, Some(mut terminate)) => {
                    let _ = terminate.recv().await;
                }
                (None, None) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        async {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Resolve when SIGINT or SIGTERM arrives.
pub async fn interrupt() {
    registered_interrupt().await
}

/// Request identities that were still unfinished when a snapshot was taken.
pub fn pending_requests(commands: &[PendingCommand]) -> BTreeSet<String> {
    commands
        .iter()
        .map(|command| command.request_id.clone())
        .collect()
}

/// Build the terminal envelope for an interrupt.
///
/// `before` and `after` are the unfinished request identities observed before
/// dispatch and after the signal. A new identity proves that a command reached
/// the local journal (and therefore Access) before the interrupt; its result is
/// unknown and the user must reconcile instead of retrying.
pub fn interruption_outcome(
    command: &Command,
    before: &[PendingCommand],
    after: &[PendingCommand],
    diagnostic_id: &str,
) -> CliOutcome {
    let known = pending_requests(before);
    let submitted: Vec<&PendingCommand> = after
        .iter()
        .filter(|pending| !known.contains(&pending.request_id))
        .collect();
    if submitted.is_empty() {
        return CliOutcome::local_failure(
            command,
            "cancelled",
            "Interrupted before the command was submitted. Nothing was sent to the Host."
                .to_owned(),
            2,
        );
    }
    let retained: Vec<serde_json::Value> = submitted
        .iter()
        .map(|pending| {
            serde_json::json!({
                "request_id": pending.request_id,
                "command_id": pending.command_id,
            })
        })
        .collect();
    let described: Vec<String> = submitted
        .iter()
        .map(|pending| format!("{} (request {})", pending.command_id, pending.request_id))
        .collect();
    let hint = format!(
        "Interrupted after submission; command {} may have been accepted. Inspect retained identities with `whalerein operation list`, then read the result with `whalerein operation recover <request-id>`. Do not resend.",
        described.join(", ")
    );
    let mut outcome =
        CliOutcome::local_failure_with_diagnostic(command, "unknown", hint, 7, diagnostic_id);
    outcome.data = Some(serde_json::json!({
        "interrupted": true,
        "retained_commands": retained,
    }));
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(request: &str) -> PendingCommand {
        PendingCommand {
            request_id: request.to_owned(),
            command_id: format!("command-{request}"),
            host_id: "host-1".to_owned(),
            device_id: "device-1".to_owned(),
            access_url: "http://127.0.0.1:8080".to_owned(),
            host_fingerprint: String::new(),
            status: whalerein_core::api::OnlineStatus::HostAccepted,
        }
    }

    #[test]
    fn an_interrupt_before_submission_is_a_local_cancellation() {
        let command = Command::Wait {
            session: "session-1".to_owned(),
            timeout: Some("30".to_owned()),
            fail_on_timeout: false,
        };
        let outcome = interruption_outcome(&command, &[], &[], "trace");
        assert_eq!(outcome.exit, 2);
        assert_eq!(outcome.command, "session.wait");
        assert_eq!(outcome.to_json()["error"]["code"], "cancelled");
        let error = outcome.error.as_ref().unwrap();
        assert_eq!(error.code, "cancelled");
        assert!(error.message.contains("Nothing was sent"));
        assert!(error.diagnostic_id.is_none());
    }

    #[test]
    fn an_interrupt_after_submission_reports_unknown_and_the_retained_identity() {
        let command = Command::Send {
            session: "session-1".to_owned(),
            text: "hello".to_owned(),
            delivery: crate::cli::Delivery::EnqueueAfterRun,
            detach: false,
            timeout: None,
            operation_id: None,
        };
        let before = [pending("request-old")];
        let after = [pending("request-old"), pending("request-new")];
        let outcome = interruption_outcome(&command, &before, &after, "trace-1");
        assert_eq!(outcome.exit, 7);
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "unknown");
        assert_eq!(value["error"]["diagnostic_id"], "trace-1");
        assert_eq!(
            value["data"]["retained_commands"][0]["request_id"],
            "request-new"
        );
        assert_eq!(
            value["data"]["retained_commands"][0]["command_id"],
            "command-request-new"
        );
        let error = outcome.error.unwrap();
        assert!(error.message.contains("command-request-new"));
        assert!(error.message.contains("request-new"));
        assert!(error.message.contains("operation list"));
        assert!(error.message.contains("operation recover"));
        assert!(error.message.contains("Do not resend"));
        assert!(!error.message.contains("hello"));
        assert!(!error.message.contains("request-old"));
    }

    #[test]
    fn signal_snapshot_keeps_only_request_identities() {
        let snapshot = pending_requests(&[pending("r-1"), pending("r-2")]);
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.contains("r-1"));
    }
}
