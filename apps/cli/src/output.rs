//! Stable CLI output: the `whalerein/v1` JSON envelope and human rendering.

use serde_json::{json, Value};
use whalerein_core::api::{CommandHandle, DoctorReport, ExportManifest, HostInfo, SessionList};

use crate::cli::Command;

use whalerein_core::conversation::{
    content_block, message_text, ApprovalState, AttemptOutcome, MessageRole, RunPhase, ViewUpdate,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub diagnostic_id: Option<String>,
    pub layer: Option<String>,
}

impl CliError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            diagnostic_id: None,
            layer: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CliOutcome {
    pub command: String,
    pub data: Option<Value>,
    pub error: Option<CliError>,
    pub human: String,
    pub exit: u8,
}

impl CliOutcome {
    pub fn success(command: &Command, data: Value, human: impl Into<String>) -> Self {
        Self {
            command: command.name().to_owned(),
            data: Some(data),
            error: None,
            human: human.into(),
            exit: 0,
        }
    }

    #[allow(dead_code)]
    pub fn failure(command: &Command, error: CliError) -> Self {
        let exit = error_exit_code(&error.code);
        Self {
            command: command.name().to_owned(),
            data: None,
            error: Some(error),
            human: String::new(),
            exit,
        }
    }

    pub fn local_failure(
        command: &Command,
        code: &str,
        message: impl Into<String>,
        exit: u8,
    ) -> Self {
        Self {
            command: command.name().to_owned(),
            data: None,
            error: Some(CliError::new(code, message)),
            human: String::new(),
            exit,
        }
    }

    /// A lifecycle outcome is a terminal domain result. FAILED and UNKNOWN keep
    /// their data payload for JSON while still returning a non-zero exit.
    pub fn lifecycle(
        command: &Command,
        handle: &CommandHandle,
        code: &str,
        human: String,
        exit: u8,
    ) -> Self {
        Self {
            command: command.name().to_owned(),
            data: Some(command_data(handle)),
            error: (exit != 0).then(|| CliError::new(code, human.clone())),
            human,
            exit,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut value = match &self.error {
            Some(error) => {
                let mut value = json!({
                    "schema": "whalerein/v1",
                    "command": self.command,
                    "error": {
                        "code": error.code,
                        "message": error.message,
                    }
                });
                if let Some(data) = &self.data {
                    value["data"] = data.clone();
                }
                value
            }
            None => json!({
                "schema": "whalerein/v1",
                "command": self.command,
                "data": self.data,
            }),
        };
        if let Some(id) = self
            .error
            .as_ref()
            .and_then(|error| error.diagnostic_id.as_ref())
        {
            value["error"]["diagnostic_id"] = json!(id);
        }
        if let Some(layer) = self.error.as_ref().and_then(|error| error.layer.as_ref()) {
            value["error"]["layer"] = json!(layer);
        }
        value
    }

    pub fn render(&self) -> String {
        if !self.human.is_empty() {
            return self.human.clone();
        }
        if let Some(error) = &self.error {
            let mut text = format!("error: {} ({})\n", error.message, error.code);
            if let Some(id) = &error.diagnostic_id {
                text.push_str(&format!("Diagnostic ID: {id}\n"));
            }
            return text;
        }
        if self.human.is_empty() {
            format!(
                "{}\n",
                serde_json::to_string_pretty(&self.data).unwrap_or_default()
            )
        } else {
            self.human.clone()
        }
    }
}

#[allow(dead_code)]
fn error_exit_code(code: &str) -> u8 {
    match code {
        "invalid_argument" | "invalid_configuration" | "cancelled" | "invalid_input" => 2,
        "not_found" => 3,
        "unauthorized" | "forbidden" | "revoked" | "not_paired" => 4,
        "host_offline" | "unavailable" | "payload_too_large" => 5,
        "rejected" | "approval_unavailable" => 6,
        "timeout" | "unknown" | "expired" => 7,
        "conflict"
        | "payload_digest_mismatch"
        | "approval_version_mismatch"
        | "approval_already_resolved"
        | "approval_expired"
        | "snapshot_required"
        | "revocations_pending"
        | "rate_limited"
        | "host_fingerprint_mismatch"
        | "channel_mismatch" => 8,
        "unsupported_capability" | "protocol_version_incompatible" => 9,
        _ => 1,
    }
}

pub fn core_error_outcome(command: &Command, error: whalerein_core::CoreError) -> CliOutcome {
    let code = error.code().to_owned();
    let exit = error.exit_code();
    let message = if matches!(command, Command::Send { .. }) && code == "unsupported_capability" {
        format!(
            "{}; the selected delivery mode is unavailable on this Host; check its runtime compatibility",
            error
        )
    } else {
        error.to_string()
    };
    let mut cli_error = CliError::new(code, message);
    cli_error.layer = Some(error.source_layer().to_owned());
    CliOutcome {
        command: command.name().to_owned(),
        data: None,
        error: Some(cli_error),
        human: String::new(),
        exit,
    }
}

pub fn render_hosts(hosts: &[HostInfo]) -> String {
    if hosts.is_empty() {
        return "No authorized hosts.\n".into();
    }
    let mut out = String::from("HOST\tONLINE\tPROTOCOL\tFINGERPRINT\n");
    for host in hosts {
        out.push_str(&format!(
            "{}\t{}\tv{}\t{}\n",
            host.host_id,
            if host.online { "yes" } else { "no" },
            host.protocol_version,
            host.fingerprint
        ));
    }
    out
}

pub fn hosts_data(hosts: &[HostInfo]) -> Value {
    json!({ "hosts": hosts })
}

pub fn render_sessions(list: &SessionList) -> String {
    if list.sessions.is_empty() {
        return "No sessions.\n".into();
    }
    let mut out = String::from("SOURCE\tCOMPLETE\tSESSION\tRUNNING\tTITLE\n");
    for session in &list.sessions {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            list.source,
            if list.complete { "yes" } else { "no" },
            session.session_id,
            if session.running { "yes" } else { "no" },
            session.title
        ));
    }
    out
}

pub fn sessions_data(host_id: &str, list: &SessionList) -> Value {
    json!({
        "host_id": host_id,
        "source": list.source,
        "complete": list.complete,
        "sessions": list.sessions,
    })
}

pub fn render_command(handle: &CommandHandle) -> String {
    let mut out = format!(
        "request: {}\ncommand: {}\nstatus: {}\n",
        handle.request_id,
        handle.command_id,
        handle.status.as_str()
    );
    if let Some(message_id) = &handle.message_id {
        out.push_str(&format!("legacy receipt: {message_id}\n"));
    }
    if let Some(receipt) = &handle.model_selection {
        if let Some(selection) = &receipt.selected {
            out.push_str(&format!(
                "provider: {}\nmodel: {}\n",
                selection.provider, selection.model
            ));
            if let Some(effort) = &selection.effort {
                out.push_str(&format!("effort: {effort}\n"));
            }
        }
        out.push_str(&format!(
            "effect: {}\ndefault persistence: {}\n",
            receipt.effect_scope, receipt.default_persistence
        ));
    }
    if let Some(delivery_id) = &handle.delivery_id {
        out.push_str(&format!("delivery: {delivery_id}\n"));
    }
    if let Some(receipt) = &handle.question_receipt {
        out.push_str(&format!(
            "question: {}\nquestion outcome: {}\n",
            receipt.batch_id, receipt.phase
        ));
    }
    if let Some(receipt) = &handle.lifecycle_receipt {
        out.push_str(&format!(
            "operation: {}\ndisposition: {}\n",
            receipt.operation, receipt.disposition
        ));
        if !receipt.session_id.is_empty() {
            out.push_str(&format!("session: {}\n", receipt.session_id));
        }
        if let Some(parent) = &receipt.parent_session_id {
            out.push_str(&format!("parent: {parent}\n"));
        }
        if let Some(phase) = &receipt.phase {
            out.push_str(&format!("phase: {phase}\n"));
        }
        if let Some(code) = &receipt.warning_code {
            out.push_str(&format!("warning: {code}\n"));
        }
        if let Some(reason) = &receipt.reason_code {
            out.push_str(&format!("reason: {reason}\n"));
        }
        if receipt.retryable {
            out.push_str("retryable: yes\n");
        }
        if let Some(id) = &receipt.operation_id {
            out.push_str(&format!("operation id: {id}\n"));
        }
        if let Some(mode) = &receipt.reconcile_mode {
            out.push_str(&format!("reconcile: {mode}"));
            if let Some(key) = &receipt.reconcile_key {
                out.push_str(&format!(" {key}"));
            }
            out.push('\n');
        }
    }
    out
}

/// Human rendering for the four lifecycle dispositions. UNKNOWN never claims
/// that a session was or was not created; FAILED never prints identity.
pub fn render_lifecycle_outcome(handle: &CommandHandle) -> Option<String> {
    let receipt = handle.lifecycle_receipt.as_ref()?;
    let subject = if receipt.operation == "fork" {
        "Child session"
    } else {
        "Session"
    };
    match receipt.disposition.as_str() {
        "READY" => Some(if receipt.session_id.is_empty() {
            "Operation completed.\n".to_owned()
        } else {
            format!("{subject} {} is ready.\n", receipt.session_id)
        }),
        "READY_WITH_WARNING" => {
            let warning = receipt
                .warning_code
                .as_deref()
                .unwrap_or("workspace attach incomplete");
            Some(format!(
                "{subject} {} exists, with a warning: {warning}.\nThe session was created; do not retry or delete it.\n",
                receipt.session_id
            ))
        }
        "FAILED" => {
            let reason = receipt.reason_code.as_deref().unwrap_or("FAILED");
            Some(format!(
                "Operation failed before any native effect.\nreason: {reason}\nretryable: {}\n",
                if receipt.retryable { "yes" } else { "no" }
            ))
        }
        "UNKNOWN" => {
            let mut text = String::from(
                "Outcome unknown: this operation may have happened. The session may or may not exist.\nDo not retry automatically.\n",
            );
            if let Some(id) = &receipt.operation_id {
                text.push_str(&format!("operation id: {id}\n"));
            }
            if let Some(mode) = &receipt.reconcile_mode {
                text.push_str(&format!("reconcile: {mode}"));
                if let Some(key) = &receipt.reconcile_key {
                    text.push_str(&format!(" {key}"));
                }
                text.push('\n');
            }
            text.push_str(&format!(
                "Reconcile with: whalerein command-recover --request {}\n",
                handle.request_id
            ));
            Some(text)
        }
        _ => None,
    }
}

/// FAILED is a usable, retryable decision; UNKNOWN requires reconciliation and
/// must not look like success to a script.
pub fn lifecycle_exit_code(handle: &CommandHandle) -> u8 {
    let Some(receipt) = &handle.lifecycle_receipt else {
        return 0;
    };
    match receipt.disposition.as_str() {
        "FAILED" => match receipt.reason_code.as_deref() {
            Some("INVALID_ARGUMENT") | Some("PREFLIGHT_INVALID_ARGUMENT") => 2,
            Some("WORKSPACE_NOT_FOUND") | Some("PRESET_NOT_FOUND") | Some("SOURCE_NOT_FOUND") => 3,
            Some("WORKSPACE_UNAVAILABLE")
            | Some("PRESET_UNAVAILABLE")
            | Some("SOURCE_NO_COMPLETE_TURN")
            | Some("PREFLIGHT_UNSUPPORTED_CAPABILITY") => 8,
            _ => 5,
        },
        "UNKNOWN" => 7,
        _ => 0,
    }
}

pub fn lifecycle_error_code(handle: &CommandHandle) -> &'static str {
    match handle
        .lifecycle_receipt
        .as_ref()
        .map(|receipt| receipt.disposition.as_str())
    {
        Some("FAILED") => "operation_failed",
        Some("UNKNOWN") => "unknown",
        _ => "internal",
    }
}

pub fn command_data(handle: &CommandHandle) -> Value {
    let mut value = json!({
        "request_id": handle.request_id,
        "command_id": handle.command_id,
        "status": handle.status.as_str(),
        "message_id": handle.message_id,
        "delivery_id": handle.delivery_id,
        "question_receipt": handle.question_receipt,
    });
    if let Some(receipt) = &handle.model_selection {
        value["model_selection"] = json!(receipt);
    }
    if let Some(receipt) = &handle.lifecycle_receipt {
        value["lifecycle_receipt"] = json!(receipt);
    }
    value
}

pub fn render_doctor(report: &DoctorReport) -> String {
    format!(
        "Address: {}\nCommands: {} unfinished, {} unknown\n",
        report.address.as_deref().unwrap_or("(not paired)"),
        report.commands.non_terminal,
        report.commands.unknown
    )
}

pub fn doctor_data(report: &DoctorReport) -> Value {
    serde_json::to_value(report).unwrap_or_else(|_| json!({}))
}

pub fn render_manifest(manifest: &ExportManifest) -> String {
    let mut out = String::from("Diagnostic export fields:\n");
    for field in &manifest.fields {
        out.push_str(&format!("  - {field}\n"));
    }
    out.push_str("Never included:\n");
    for redaction in &manifest.redactions {
        out.push_str(&format!("  - {redaction}\n"));
    }
    out.push_str(&format!(
        "Written to {} ({} bytes)\n",
        manifest.path, manifest.bytes
    ));
    out
}

pub fn manifest_data(manifest: &ExportManifest) -> Value {
    serde_json::to_value(manifest).unwrap_or_else(|_| json!({}))
}

/// Render only the typed normalized view. Native event JSON is never available.
pub fn tail_event_json(update: &ViewUpdate) -> Value {
    let event = match update {
        ViewUpdate::Preview {
            attempt,
            block_id,
            text,
            replace,
            incomplete,
        } => {
            json!({"type":"preview","attempt_id":attempt.attempt_id,"live_epoch":attempt.live_epoch,"block_id":block_id,"text":text,"replace":replace,"incomplete":incomplete})
        }
        ViewUpdate::Message {
            message, origin, ..
        } => {
            let blocks:Vec<Value>=message.blocks.iter().map(|b|match &b.content {
                Some(content_block::Content::Text(t))=>json!({"block_id":b.block_id,"type":"text","text":t.text}),
                Some(content_block::Content::ToolCall(t))=>json!({"block_id":b.block_id,"type":"tool_call","call_id":t.call_id,"name":t.name,"summary":t.summary}),
                Some(content_block::Content::ToolResult(t))=>json!({"block_id":b.block_id,"type":"tool_result","call_id":t.call_id,"text":t.text,"is_error":t.is_error}),
                Some(content_block::Content::Notice(t))=>json!({"block_id":b.block_id,"type":"notice","category":t.category,"text":t.text}),
                Some(content_block::Content::Attachment(t))=>json!({"block_id":b.block_id,"type":"attachment","name":t.name,"media_type":t.media_type,"size":t.size,"available":t.available}),
                None=>json!({"block_id":b.block_id,"type":"unavailable"}),
            }).collect();
            json!({"type":"message","message_id":message.message_id,"role":MessageRole::try_from(message.role).unwrap_or_default().as_str_name(),"blocks":blocks,"authored_at_unix_ms":message.authored_at_unix_ms,"origin":origin.as_ref().map(|a|json!({"live_epoch":a.live_epoch,"attempt_id":a.attempt_id}))})
        }
        ViewUpdate::AttemptEnded { attempt, outcome } => {
            json!({"type":"attempt_ended","attempt_id":attempt.attempt_id,"live_epoch":attempt.live_epoch,"outcome":AttemptOutcome::try_from(*outcome).unwrap_or_default().as_str_name()})
        }
        ViewUpdate::State(state) => {
            let approvals:Vec<Value>=state.approvals.iter().map(|a|json!({"approval_id":a.approval_id,"session_id":a.session_id,"tool_name":a.tool_name,"call_id":a.call_id,"reason":a.reason,"digest":hex::encode(&a.digest),"version":a.version,"expires_at_unix_ms":a.expires_at_unix_ms,"state":ApprovalState::try_from(a.state).unwrap_or_default().as_str_name()})).collect();
            json!({"type":"state","run_phase":RunPhase::try_from(state.run_phase).unwrap_or_default().as_str_name(),"approvals":approvals})
        }
        ViewUpdate::Reset => json!({"type":"reset"}),
    };
    json!({"schema":"whalerein/v1","command":"tail","data":{"event":event}})
}

/// Formatting owns terminal boundaries only; Core owns preview ordering and completion.
#[derive(Default)]
pub struct TailRenderer {
    line_open: bool,
    attempt: Option<String>,
}
impl TailRenderer {
    pub fn render(&mut self, update: &ViewUpdate) -> String {
        match update {
            ViewUpdate::Preview {
                attempt,
                text,
                replace,
                ..
            } => {
                let prefix = if self.attempt.as_deref() != Some(&attempt.attempt_id) || *replace {
                    self.finish_line()
                } else {
                    String::new()
                };
                self.attempt = Some(attempt.attempt_id.clone());
                self.line_open |= !text.is_empty();
                format!("{prefix}{text}")
            }
            ViewUpdate::Message {
                message,
                preview_prefix_bytes,
                ..
            } => {
                let text = message_text(message);
                let tools: String = message
                    .blocks
                    .iter()
                    .filter_map(|block| match &block.content {
                        Some(content_block::Content::ToolCall(tool)) => {
                            Some(format!("[tool] {} {}\n", tool.name, tool.summary))
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(offset) = preview_prefix_bytes {
                    self.line_open = false;
                    self.attempt = None;
                    format!("{}\n{tools}", text.get(*offset..).unwrap_or(&text))
                } else {
                    let prefix = self.finish_line();
                    self.attempt = None;
                    format!(
                        "{prefix}[{}] {text}\n{tools}",
                        MessageRole::try_from(message.role)
                            .unwrap_or_default()
                            .as_str_name()
                    )
                }
            }
            ViewUpdate::Reset => {
                self.attempt = None;
                self.finish_line()
            }
            ViewUpdate::AttemptEnded { outcome, .. }
                if *outcome != AttemptOutcome::Committed as i32 =>
            {
                self.attempt = None;
                self.finish_line()
            }
            _ => String::new(),
        }
    }
    pub fn finish_line(&mut self) -> String {
        if std::mem::take(&mut self.line_open) {
            "\n".into()
        } else {
            String::new()
        }
    }
}
pub fn is_tail_output(update: &ViewUpdate) -> bool {
    matches!(update, ViewUpdate::Message { .. })
        || matches!(update,ViewUpdate::Preview{text,..} if !text.is_empty())
}

pub fn render_pair(outcome: &whalerein_core::api::PairingOutcome) -> String {
    format!(
        "Paired host {} as device {}\nFingerprint: {}\n",
        outcome.host_id, outcome.device_id, outcome.fingerprint
    )
}
pub fn render_approvals(approvals: &[whalerein_core::api::ApprovalInfo]) -> String {
    if approvals.is_empty() {
        return "No pending approvals.\n".into();
    }
    approvals
        .iter()
        .map(|a| {
            format!(
                "{} {} version={} digest={} {}\n",
                a.approval_id,
                a.tool_name,
                a.version,
                hex::encode(&a.digest),
                a.reason
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_core::api::{CommandDiagnostics, LifecycleReceipt, OnlineStatus};

    fn command() -> Command {
        Command::Hosts
    }

    fn handle() -> CommandHandle {
        CommandHandle {
            request_id: "r-1".into(),
            command_id: "c-1".into(),
            status: OnlineStatus::HostAccepted,
            model_selection: None,
            message_id: Some("m-1".into()),
            delivery_id: Some("d-1".into()),
            lifecycle_receipt: None,
            question_receipt: None,
        }
    }

    fn lifecycle_handle(disposition: &str) -> CommandHandle {
        let failed = matches!(disposition, "FAILED" | "UNKNOWN");
        CommandHandle {
            request_id: "r-1".into(),
            command_id: "c-1".into(),
            status: OnlineStatus::Succeeded,
            model_selection: None,
            message_id: None,
            delivery_id: None,
            lifecycle_receipt: Some(LifecycleReceipt {
                session_id: if failed {
                    String::new()
                } else {
                    "session-1".into()
                },
                parent_session_id: if failed { None } else { Some("parent".into()) },
                operation: "create".into(),
                disposition: disposition.into(),
                warning_code: (disposition == "READY_WITH_WARNING")
                    .then(|| "workspace_attach_failed".into()),
                operation_id: Some("op-1".into()),
                phase: Some(if failed {
                    "NATIVE".into()
                } else {
                    "COMPLETE".into()
                }),
                reason_code: failed.then(|| {
                    if disposition == "FAILED" {
                        "WORKSPACE_NOT_FOUND".into()
                    } else {
                        "NATIVE_INTERNAL".into()
                    }
                }),
                retryable: disposition == "FAILED",
                reconcile_mode: (disposition == "UNKNOWN").then(|| "OPERATION_ID".into()),
                reconcile_key: (disposition == "UNKNOWN").then(|| "op-1".into()),
                warning_detail: None,
            }),
            question_receipt: None,
        }
    }

    fn report() -> DoctorReport {
        DoctorReport {
            schema: "whalerein/v1".into(),
            address: Some("https://access.test".into()),
            commands: CommandDiagnostics {
                non_terminal: 1,
                unknown: 2,
            },
        }
    }

    #[test]
    fn emits_stable_json_schema_for_successful_outcomes() {
        let outcome = CliOutcome::success(&command(), json!({"hosts": []}), "none\n");
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/v1");
        assert_eq!(value["command"], "hosts");
        assert!(value.get("data").is_some());
        assert!(value.get("error").is_none());
    }

    #[test]
    fn emits_stable_json_error_shape_for_failed_outcomes() {
        let outcome = CliOutcome::failure(&command(), CliError::new("unauthorized", "no"));
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/v1");
        assert_eq!(value["error"]["code"], "unauthorized");
        assert_eq!(value["error"]["message"], "no");
        assert_eq!(outcome.exit, 4);
        assert_eq!(outcome.render(), "error: no (unauthorized)\n");
    }

    #[test]
    fn maps_local_error_codes_to_process_exit_codes() {
        for (code, exit) in [
            ("invalid_argument", 2),
            ("not_found", 3),
            ("revoked", 4),
            ("host_offline", 5),
            ("rejected", 6),
            ("timeout", 7),
            ("payload_digest_mismatch", 8),
            ("protocol_version_incompatible", 9),
            ("internal", 1),
        ] {
            let outcome = CliOutcome::failure(&command(), CliError::new(code, "x"));
            assert_eq!(outcome.exit, exit, "{code}");
        }
    }

    #[test]
    fn maps_core_error_to_cli_error_outcome() {
        let outcome = core_error_outcome(&command(), whalerein_core::CoreError::HostOffline);
        assert_eq!(outcome.command, "hosts");
        assert_eq!(outcome.error.unwrap().code, "host_offline");
        assert_eq!(outcome.exit, 5);
    }

    #[test]
    fn unsupported_send_includes_a_runtime_compatibility_hint() {
        let command = Command::Send {
            session: "fixture".into(),
            delivery: crate::cli::Delivery::EnqueueAfterRun,
            text: "synthetic".into(),
        };
        let outcome = core_error_outcome(&command, whalerein_core::CoreError::Unsupported);
        let error = outcome.error.unwrap();
        assert_eq!(error.code, "unsupported_capability");
        assert_eq!(outcome.exit, 9);
        assert!(error.message.contains("runtime compatibility"));
        assert!(!error.message.contains("synthetic"));
    }

    #[test]
    fn renders_host_presence_and_session_summaries() {
        let hosts = vec![HostInfo {
            host_id: "h-1".into(),
            online: true,
            protocol_version: 1,
            fingerprint: "abcd".into(),
        }];
        let text = render_hosts(&hosts);
        assert!(text.contains("h-1"));
        assert!(text.contains("yes"));
        assert!(render_hosts(&[]).contains("No authorized hosts"));
        let list = SessionList {
            sessions: vec![whalerein_core::api::SessionSummary {
                session_id: "s-1".into(),
                title: "title".into(),
                cwd: "/".into(),
                created_at_unix_ms: 1,
                updated_at_unix_ms: 2,
                running: true,
                agent_available: true,
                blank: false,
                parent_session_id: String::new(),
                origin: "cli".into(),
                history_cursor: String::new(),
                source_capabilities: None,
                queued_prompts: 0,
            }],
            source: "dsh".into(),
            complete: true,
        };
        assert!(render_sessions(&list).contains("s-1"));
        assert!(render_sessions(&SessionList {
            sessions: vec![],
            source: "cache".into(),
            complete: false
        })
        .contains("No sessions"));
        let data = sessions_data("h-1", &list);
        assert_eq!(data["host_id"], "h-1");
        assert_eq!(data["sessions"][0]["session_id"], "s-1");
    }

    #[test]
    fn renders_model_selection_receipt_in_human_and_json_output() {
        let mut command = handle();
        command.model_selection = Some(whalerein_core::api::ModelSelectionReceipt {
            selected: Some(whalerein_core::api::ModelChoice {
                provider: "fixture-provider".into(),
                model: "fixture-model".into(),
                effort: Some("high".into()),
            }),
            effect_scope: "session_and_source_default_attempt".into(),
            default_persistence: "unconfirmed".into(),
        });
        let text = render_command(&command);
        for expected in [
            "provider: fixture-provider",
            "model: fixture-model",
            "effort: high",
            "effect: session_and_source_default_attempt",
            "default persistence: unconfirmed",
        ] {
            assert!(text.contains(expected));
        }
        let data = command_data(&command);
        assert_eq!(data["model_selection"]["selected"]["effort"], "high");
        assert_eq!(
            data["model_selection"]["default_persistence"],
            "unconfirmed"
        );
    }

    #[test]
    fn renders_command_status_without_sensitive_payload() {
        let handle = handle();
        let text = render_command(&handle);
        assert!(text.contains("host_accepted"));
        assert!(text.contains("m-1"));
        assert!(text.contains("delivery: d-1"));
        let data = command_data(&handle);
        assert_eq!(data["delivery_id"], "d-1");
        assert_eq!(data["request_id"], "r-1");
        assert_eq!(data["status"], "host_accepted");
    }

    #[test]
    fn renders_doctor_report_without_host_or_device_ids() {
        let text = render_doctor(&report());
        assert!(text.contains("Address: https://access.test"));
        assert!(text.contains("Commands: 1 unfinished, 2 unknown"));
        assert!(!text.contains("h-1"));
        assert!(!text.contains("Connection"));
        let data = doctor_data(&report());
        assert_eq!(data["commands"]["non_terminal"], 1);
        assert!(data.get("connection").is_none());
    }

    #[test]
    fn manifest_lists_fields_and_redactions() {
        let manifest = ExportManifest {
            schema: "whalerein/v1".into(),
            path: "out.json".into(),
            fields: vec!["address".into()],
            redactions: vec!["private keys".into()],
            bytes: 10,
        };
        let text = render_manifest(&manifest);
        assert!(text.contains("address"));
        assert!(text.contains("private keys"));
        assert_eq!(manifest_data(&manifest)["bytes"], 10);
    }
    #[test]
    fn renders_every_lifecycle_disposition_without_claiming_unproven_identity() {
        let ready = render_lifecycle_outcome(&lifecycle_handle("READY")).unwrap();
        assert!(ready.contains("session-1"));
        assert!(ready.contains("is ready"));

        let warning = render_lifecycle_outcome(&lifecycle_handle("READY_WITH_WARNING")).unwrap();
        assert!(warning.contains("session-1"));
        assert!(warning.contains("workspace_attach_failed"));
        assert!(warning.contains("do not retry"));
        assert!(!warning.contains("Operation failed"));

        let failed = render_lifecycle_outcome(&lifecycle_handle("FAILED")).unwrap();
        assert!(failed.contains("failed before any native effect"));
        assert!(failed.contains("WORKSPACE_NOT_FOUND"));
        assert!(failed.contains("retryable: yes"));
        assert!(!failed.contains("session-1"));
        assert!(!render_command(&lifecycle_handle("FAILED")).contains("session:"));

        let unknown = render_lifecycle_outcome(&lifecycle_handle("UNKNOWN")).unwrap();
        assert!(unknown.contains("Outcome unknown"));
        assert!(unknown.contains("may have happened"));
        assert!(unknown.contains("operation id: op-1"));
        assert!(unknown.contains("reconcile: OPERATION_ID op-1"));
        assert!(unknown.contains("command-recover --request r-1"));
        assert!(!unknown.contains("was created"));
        assert!(!unknown.contains("not created"));
        assert!(!render_command(&lifecycle_handle("UNKNOWN")).contains("session:"));
    }

    #[test]
    fn lifecycle_json_carries_all_four_states_and_machine_fields() {
        for disposition in ["READY", "READY_WITH_WARNING", "FAILED", "UNKNOWN"] {
            let outcome = CliOutcome::lifecycle(
                &command(),
                &lifecycle_handle(disposition),
                lifecycle_error_code(&lifecycle_handle(disposition)),
                "human\n".to_owned(),
                lifecycle_exit_code(&lifecycle_handle(disposition)),
            );
            let value = outcome.to_json();
            assert_eq!(
                value["data"]["lifecycle_receipt"]["disposition"],
                disposition
            );
            assert_eq!(value["data"]["lifecycle_receipt"]["operation"], "create");
            assert_eq!(value["data"]["lifecycle_receipt"]["operation_id"], "op-1");
            if disposition == "UNKNOWN" {
                assert_eq!(outcome.exit, 7);
                assert_eq!(value["error"]["code"], "unknown");
                assert_eq!(
                    value["data"]["lifecycle_receipt"]["reconcile_mode"],
                    "OPERATION_ID"
                );
            } else if disposition == "FAILED" {
                assert_eq!(outcome.exit, 3);
                assert_eq!(value["error"]["code"], "operation_failed");
                assert_eq!(
                    value["data"]["lifecycle_receipt"]["reason_code"],
                    "WORKSPACE_NOT_FOUND"
                );
            } else {
                assert_eq!(outcome.exit, 0);
                assert!(value.get("error").is_none());
            }
            assert_eq!(outcome.render(), "human\n");
        }
    }
}

#[cfg(test)]
mod typed_output_tests {
    use super::*;
    use whalerein_core::conversation::{AttemptRef, ConversationMessage};
    fn attempt() -> AttemptRef {
        AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        }
    }
    fn message() -> ConversationMessage {
        ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant as i32,
            blocks: vec![],
            authored_at_unix_ms: 1,
        }
    }
    #[test]
    fn human_output_uses_core_prefix_decisions_and_closes_provisional_lines() {
        let mut renderer = TailRenderer::default();
        let preview = ViewUpdate::Preview {
            attempt: attempt(),
            block_id: "text".into(),
            text: "x".into(),
            replace: false,
            incomplete: false,
        };
        assert_eq!(renderer.render(&preview).len(), 1);
        assert!(is_tail_output(&preview));
        assert_eq!(
            renderer.render(&ViewUpdate::Message {
                message: message(),
                origin: Some(attempt()),
                preview_prefix_bytes: Some(0)
            }),
            "\n"
        );
        assert!(renderer.finish_line().is_empty());
        renderer.render(&preview);
        assert_eq!(renderer.render(&ViewUpdate::Reset), "\n");
        assert!(!is_tail_output(&ViewUpdate::Reset));
        renderer.render(&preview);
        assert_eq!(
            renderer.render(&ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Abandoned as i32
            }),
            "\n"
        );
        assert!(renderer
            .render(&ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Committed as i32
            })
            .is_empty());
        renderer.render(&preview);
        let mut reset = preview.clone();
        if let ViewUpdate::Preview { replace, .. } = &mut reset {
            *replace = true
        }
        assert_eq!(renderer.render(&reset).len(), 2);
        assert_eq!(renderer.finish_line(), "\n");
        assert!(renderer
            .render(&ViewUpdate::Message {
                message: message(),
                origin: None,
                preview_prefix_bytes: None
            })
            .contains("ASSISTANT"));
    }
    #[test]
    fn json_view_preserves_correlation_and_typed_preview_fields() {
        let preview = ViewUpdate::Preview {
            attempt: attempt(),
            block_id: "text".into(),
            text: "x".into(),
            replace: false,
            incomplete: true,
        };
        let value = tail_event_json(&preview);
        assert_eq!(value["data"]["event"]["type"], "preview");
        assert!(value["data"]["event"]["incomplete"].as_bool().unwrap());
        let value = tail_event_json(&ViewUpdate::Message {
            message: message(),
            origin: Some(attempt()),
            preview_prefix_bytes: None,
        });
        assert_eq!(value["data"]["event"]["origin"]["attempt_id"], "a");
        assert!(value["data"]["event"].get("data_json").is_none());
        let ended = tail_event_json(&ViewUpdate::AttemptEnded {
            attempt: attempt(),
            outcome: AttemptOutcome::Committed as i32,
        });
        assert_eq!(ended["data"]["event"]["outcome"], "COMMITTED");
    }
    #[test]
    fn state_reset_and_empty_preview_do_not_satisfy_visible_tail_output() {
        let mut renderer = TailRenderer::default();
        let state = ViewUpdate::State(Default::default());
        assert!(renderer.render(&state).is_empty());
        assert!(!is_tail_output(&state));
        assert_eq!(tail_event_json(&state)["data"]["event"]["type"], "state");
        assert_eq!(
            tail_event_json(&ViewUpdate::Reset)["data"]["event"]["type"],
            "reset"
        );
        assert!(!is_tail_output(&ViewUpdate::Preview {
            attempt: attempt(),
            block_id: "text".into(),
            text: String::new(),
            replace: false,
            incomplete: false
        }));
    }
    #[test]
    fn pairing_and_approval_formatting_reveal_only_the_requested_public_details() {
        let pair = whalerein_core::api::PairingOutcome {
            host_id: "h".into(),
            device_id: "d".into(),
            fingerprint: "fixture".into(),
            access_url: "https://example.test".into(),
        };
        assert_eq!(
            render_pair(&pair),
            "Paired host h as device d\nFingerprint: fixture\n"
        );
        assert!(!render_pair(&pair).contains(&pair.access_url));
        assert_eq!(render_approvals(&[]), "No pending approvals.\n");
    }
}

#[cfg(test)]
mod typed_content_tests {
    use super::*;
    use whalerein_core::conversation::ConversationMessage;
    #[test]
    fn json_formats_every_declared_content_family_without_native_payloads() {
        use content_block::Content;
        let mut message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant as i32,
            ..Default::default()
        };
        for content in [
            Some(Content::Text(Default::default())),
            Some(Content::ToolCall(Default::default())),
            Some(Content::ToolResult(Default::default())),
            Some(Content::Notice(Default::default())),
            Some(Content::Attachment(Default::default())),
            None,
        ] {
            message.blocks.push(Default::default());
            let id = message.blocks.len().to_string();
            let block = message.blocks.last_mut().unwrap();
            block.block_id = id;
            block.content = content;
        }
        let value = tail_event_json(&ViewUpdate::Message {
            message,
            origin: None,
            preview_prefix_bytes: None,
        });
        assert_eq!(
            value["data"]["event"]["blocks"].as_array().unwrap().len(),
            6
        );
        assert_eq!(value["data"]["event"]["blocks"][1]["type"], "tool_call");
    }
    #[test]
    fn state_json_carries_approval_digest_version_and_typed_status() {
        let mut state = whalerein_core::conversation::ViewUpdate::State(Default::default());
        if let ViewUpdate::State(s) = &mut state {
            s.approvals.push(Default::default());
            let a = s.approvals.last_mut().unwrap();
            a.approval_id = "a".into();
            a.digest = vec![1; 32];
            a.version = 3;
            a.state = ApprovalState::Pending as i32;
            s.run_phase = RunPhase::Idle as i32;
        }
        let value = tail_event_json(&state);
        assert_eq!(value["data"]["event"]["run_phase"], "IDLE");
        assert_eq!(
            value["data"]["event"]["approvals"][0]["state"],
            "APPROVAL_STATE_PENDING"
        );
        assert_eq!(value["data"]["event"]["approvals"][0]["approval_id"], "a");
        assert_eq!(value["data"]["event"]["approvals"][0]["version"], 3);
        assert_eq!(
            value["data"]["event"]["approvals"][0]["digest"],
            "01".repeat(32)
        );
        let approval = whalerein_core::api::ApprovalInfo {
            approval_id: "a".into(),
            session_id: "s".into(),
            tool_name: "fixture".into(),
            call_id: "c".into(),
            reason: "review".into(),
            digest: vec![1; 32],
            version: 3,
            expires_at_unix_ms: 9,
            state: "Pending".into(),
            decision: "Unspecified".into(),
        };
        assert_eq!(
            render_approvals(&[approval]),
            format!("a fixture version=3 digest={} review\n", "01".repeat(32))
        );
    }
}
