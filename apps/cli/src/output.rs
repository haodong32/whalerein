//! Stable CLI output: the `whalerein/client/v1` JSON envelope and human rendering.

use serde_json::{json, Value};
use whalerein_core::api::{
    CommandHandle, DoctorReport, ExportManifest, HostInfo, ModelCatalog, PairingHistoryRecord,
    PendingQueue, QuestionPageInfo, SessionList, SessionModel,
};

use crate::cli::{Command, ModelAction};

use whalerein_core::conversation::{
    content_block, AttemptOutcome, ConversationMessage, RunPhase, ViewUpdate,
};

/// Render one committed message's blocks in source order.
///
/// A preview prefix skips bytes the live stream already printed; it applies to
/// the text-like run (text, tool result, notice) exactly as Core's
/// `message_text` concatenates it. Tool calls and attachments are actionable
/// structure, so they are always shown even after a preview.
fn render_message_blocks(message: &ConversationMessage, skip: usize) -> String {
    let mut lines = Vec::new();
    let mut consumed = 0usize;
    for block in &message.blocks {
        let line = match &block.content {
            Some(content_block::Content::Text(text)) => {
                let kept = suffix_after(&text.text, &mut consumed, skip);
                if kept.is_empty() {
                    continue;
                }
                kept
            }
            Some(content_block::Content::ToolResult(result)) => {
                let kept = suffix_after(&result.text, &mut consumed, skip);
                if kept.is_empty() {
                    continue;
                }
                format!(
                    "[tool {}] {}",
                    if result.is_error { "error" } else { "result" },
                    kept
                )
            }
            Some(content_block::Content::Notice(notice)) => {
                let kept = suffix_after(&notice.text, &mut consumed, skip);
                if kept.is_empty() {
                    continue;
                }
                if notice.category.is_empty() {
                    format!("[notice] {kept}")
                } else {
                    format!("[notice {}] {kept}", sanitize_field(&notice.category))
                }
            }
            Some(content_block::Content::ToolCall(tool)) => format!(
                "[tool] {} {}",
                sanitize_field(&tool.name),
                sanitize_field(&tool.summary)
            ),
            Some(content_block::Content::Attachment(attachment)) => format!(
                "[attachment {} ({}, {} bytes{})]",
                sanitize_field(&attachment.name),
                sanitize_field(&attachment.media_type),
                attachment.size,
                if attachment.available {
                    ""
                } else {
                    ", unavailable"
                }
            ),
            None => continue,
        };
        lines.push(line);
    }
    // Text blocks can carry internal newlines; joining keeps source order and
    // does not insert a blank line between structured blocks.
    lines.join("\n")
}

/// Consume one text-like block against a byte prefix already streamed.
fn suffix_after(text: &str, consumed: &mut usize, skip: usize) -> String {
    let start = *consumed;
    *consumed += text.len();
    if start >= skip {
        return text.to_owned();
    }
    let remaining = skip - start;
    if remaining >= text.len() {
        String::new()
    } else {
        text.get(remaining..).unwrap_or_default().to_owned()
    }
}

fn attempt_outcome_label(outcome: AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Committed => "committed",
        AttemptOutcome::Abandoned => "abandoned",
        AttemptOutcome::Interrupted => "interrupted",
        AttemptOutcome::AttemptFailed => "failed",
        AttemptOutcome::Unspecified => "ended",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub code: String,
    pub message: String,
    /// Stable client action from the shared `ErrorCode` contract
    /// (`proto/v1/error_codes.json`). An `upgrade` action is terminal for the
    /// current attempt and must never be presented as a retry.
    pub action: String,
    pub diagnostic_id: Option<String>,
    pub layer: Option<String>,
}

impl CliError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            action: error_action(&code).to_owned(),
            code,
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

    /// A local failure that must stay forensically traceable. Unknown-result
    /// outcomes get the same diagnostic identity as Core-produced failures so
    /// the bounded `last-failure.json` history can be exported afterwards.
    pub fn local_failure_with_diagnostic(
        command: &Command,
        code: &str,
        message: impl Into<String>,
        exit: u8,
        diagnostic_id: &str,
    ) -> Self {
        let mut outcome = Self::local_failure(command, code, message, exit);
        if let Some(error) = outcome.error.as_mut() {
            error.diagnostic_id = Some(diagnostic_id.to_owned());
        }
        outcome
    }

    /// A failure that happened outside a parsed subcommand (usage error or an
    /// invalid environment variable). The envelope names `cli` because no
    /// command ran.
    pub fn cli_failure(code: &str, message: impl Into<String>, exit: u8) -> Self {
        Self {
            command: "cli".to_owned(),
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
                    "schema": "whalerein/client/v1",
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
                "schema": "whalerein/client/v1",
                "command": self.command,
                "data": self.data,
            }),
        };
        if let Some(action) = self.error.as_ref().map(|error| error.action.clone()) {
            value["error"]["action"] = json!(action);
        }
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

/// Exit code for a stable CLI error code string.
///
/// Wire names come from the single `ErrorCode` table in Core (which is pinned
/// to `proto/v1/WIRE.md`); this function only adds the local codes that have no
/// wire `ErrorCode` because they are produced inside the client.
pub fn error_exit_code(code: &str) -> u8 {
    match code {
        "invalid_configuration" | "cancelled" => 2,
        "revoked" | "not_paired" => 4,
        "host_offline" => 5,
        "rejected" => 6,
        "timeout" | "unknown" => 7,
        "cache_error" | "security_error" => 1,
        "operation_failed" => 5,
        _ => match whalerein_proto::ErrorCode::from_str_name(&code.to_ascii_lowercase()) {
            Some(wire) => whalerein_core::api::error_code_exit_code(wire),
            None => 1,
        },
    }
}

/// Stable client action for a CLI error code.
///
/// Wire names use Core's shared `error_code_action`; the local codes that have
/// no wire code are classified here.
pub fn error_action(code: &str) -> &'static str {
    match code {
        "invalid_configuration" => "fix_input",
        "cancelled" | "timeout" | "host_offline" | "unavailable" | "cache_error"
        | "security_error" | "internal" | "operation_failed" => "retry",
        "unknown" | "rejected" => "refresh",
        "revoked" | "not_paired" => "reauth",
        _ => match whalerein_proto::ErrorCode::from_str_name(&code.to_ascii_lowercase()) {
            Some(wire) => whalerein_core::api::error_code_action(wire),
            None => "retry",
        },
    }
}

/// Exit code for a `LifecycleReceipt::reason_code`.
///
/// `PREFLIGHT_*` and `NATIVE_*` wrap the underlying `ErrorCode` name, so they
/// inherit that code's WIRE exit value instead of falling through to a generic
/// "unavailable". Reasons that are not wire codes are classified by phase:
/// preflight failures prove that no native effect happened; post-entry failures
/// stay unavailable unless the reason itself is more specific.
pub fn lifecycle_reason_exit_code(reason_code: &str) -> u8 {
    for prefix in ["PREFLIGHT_", "NATIVE_"] {
        if let Some(suffix) = reason_code.strip_prefix(prefix) {
            return match whalerein_proto::ErrorCode::from_str_name(&suffix.to_ascii_lowercase()) {
                Some(wire) => whalerein_core::api::error_code_exit_code(wire),
                None => 5,
            };
        }
    }
    match reason_code {
        "INVALID_ARGUMENT" => 2,
        "WORKSPACE_NOT_FOUND" | "PRESET_NOT_FOUND" | "SOURCE_NOT_FOUND" => 3,
        "WORKSPACE_UNAVAILABLE" | "PRESET_UNAVAILABLE" | "SOURCE_NO_COMPLETE_TURN" => 8,
        // A Host process interrupted before the native effect proves the
        // operation did not happen; it is an availability failure (retryable
        // is false, so nothing retries automatically).
        "PROCESS_INTERRUPTED" => 5,
        _ => 5,
    }
}

pub fn core_error_outcome(command: &Command, error: whalerein_core::CoreError) -> CliOutcome {
    let code = error.code().to_owned();
    let exit = error.exit_code();
    // Guidance follows the shared action contract. `upgrade` is terminal: the
    // message names the side to upgrade and never suggests retrying the same
    // request.
    let message = match (code.as_str(), command) {
        ("authentication_required", _) => "Cloud sign-in must be refreshed. Run `whalerein login`.".into(),
        ("authorization_expired", _) => "The pairing authorization expired. Run `whalerein login` and recover the original operation.".into(),
        ("unauthorized", Command::Hosts | Command::Connect { .. }) => {
            "Cloud authorization is required. Run `whalerein login` and check that the account can access the Host.".into()
        }
        ("not_paired", _) => format!(
            "{}. Run `whalerein host list` and `whalerein host connect HOST` for Cloud, or `whalerein host connect --invite-stdin` for Standalone.",
            error
        ),
        ("host_fingerprint_mismatch", _) => format!(
            "{}; verify the Host fingerprint out of band first. Only after confirming the Host identity, run `whalerein device forget --local-only --yes` (or `whalerein device revoke --yes` while the old authorization is still valid) and pair again with `whalerein host connect --invite-stdin`. Do not re-pair against an unverified fingerprint",
            error
        ),
        ("unsupported_capability", Command::Send { .. }) => format!(
            "{}; the selected delivery mode is unavailable on this Host; check its runtime compatibility and upgrade the Host plugin if needed",
            error
        ),
        ("unsupported_capability", _) => format!(
            "{}; the Host does not advertise this capability; upgrade the Host plugin or use a supported command (do not retry the same request)",
            error
        ),
        (
            "invalid_argument",
            Command::Model {
                action: ModelAction::Set { .. },
            },
        ) => format!(
            "{}; the Host rejected this provider/model and no change was made — run `whalerein session models --host <host>`, then retry only after that provider is advertised and healthy",
            error
        ),
        (
            "unavailable",
            Command::Model {
                action: ModelAction::Set { .. },
            },
        ) => format!(
            "{}; no change is confirmed. If `whalerein session models` lists the provider as failed, retry only after the provider recovers; if the Host is offline, retry when it reconnects",
            error
        ),
        (
            "not_found",
            Command::Model {
                action: ModelAction::Set { .. },
            },
        ) => format!(
            "{}; nothing changed — verify the session with `whalerein session list --host <host>` and the selection with `whalerein session models --host <host>`",
            error
        ),
        ("unknown", Command::Model { .. }) => format!(
            "{}; the selection may or may not have been applied — do not resend it; recover the command or run `whalerein session model get --session <session-id>` to confirm",
            error
        ),
        ("snapshot_required", Command::Questions { .. }) => format!(
            "{}; question pagination state changed across a restart — re-run `whalerein session question list --session <session-id>` without --cursor",
            error
        ),
        ("protocol_version_incompatible", _) => format!(
            "{}; upgrade WhaleRein so the CLI, Host plugin, and Access share a supported protocol version (do not retry the same request)",
            error
        ),
        ("authority_override", _) => format!(
            "{}; built-in environment names cannot be removed or have their authority replaced. Choose a supported official environment or Standalone in the selected configuration",
            error
        ),
        ("security_error", _) => format!(
            "{}; if the Host identity or credentials were rebuilt, revoke this device and pair again (`whalerein device revoke --yes`, or `whalerein device forget --local-only --yes` when local keys are lost), then run `whalerein host connect --invite-stdin`",
            error
        ),
        _ => error.to_string(),
    };
    let mut cli_error = CliError::new(code, message);
    cli_error.action = error.action().to_owned();
    cli_error.layer = Some(error.source_layer().to_owned());
    CliOutcome {
        command: command.name().to_owned(),
        data: None,
        error: Some(cli_error),
        human: String::new(),
        exit,
    }
}

/// Escape one human-output field.
///
/// Titles, reasons and tool names are Host/Source data. A tab or newline would
/// break the tab-separated layout, a carriage return would overwrite the line,
/// and an ANSI sequence would inject terminal control. Control characters are
/// rendered as visible escapes; ANSI escape sequences are removed entirely so
/// piped and `--json` output never carries color.
pub fn sanitize_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        match character {
            '\u{1b}' => {
                // CSI: ESC '[' ... final byte 0x40-0x7E. OSC: ESC ']' ... BEL.
                match chars.next() {
                    Some('[') => {
                        for next in chars.by_ref() {
                            if ('@'..='~').contains(&next) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        for next in chars.by_ref() {
                            if next == '\u{7}' {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            control if control.is_control() => {
                out.push_str(&format!("\\u{{{:x}}}", control as u32));
            }
            normal => out.push(normal),
        }
    }
    out
}

/// Terminal columns used by one character. East Asian wide characters take two
/// columns, so padding must not assume one char equals one column.
pub fn display_width(value: &str) -> usize {
    value.chars().map(char_width).sum()
}

fn char_width(character: char) -> usize {
    match character as u32 {
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// Pad a field to a display width instead of a byte or character count.
pub fn pad_cell(value: &str, width: usize) -> String {
    let mut text = value.to_owned();
    let padding = width.saturating_sub(display_width(value));
    text.extend(std::iter::repeat_n(' ', padding));
    text
}

pub fn render_hosts(hosts: &[HostInfo]) -> String {
    if hosts.is_empty() {
        return "No authorized hosts. Pair one with `whalerein host connect --invite-stdin`.\n"
            .into();
    }
    let mut out = String::from("HOST ID\tONLINE\tPROTOCOL\tFINGERPRINT\n");
    for host in hosts {
        out.push_str(&format!(
            "{}\t{}\tv{}\t{}\n",
            sanitize_field(&host.host_id),
            if host.online { "yes" } else { "no" },
            host.protocol_version,
            sanitize_field(&host.fingerprint)
        ));
    }
    out
}

pub fn render_account_hosts(hosts: &[whalerein_core::api::AccountHost]) -> String {
    if hosts.is_empty() {
        return "No computers registered to this Cloud workspace.\n".into();
    }
    let mut out = String::from("HOST ID\tNAME\tHOST STATUS\tDEVICE ACCESS\n");
    for host in hosts {
        let status = match (host.observation.as_str(), host.enrollment_state.as_str()) {
            ("missing", _) => "not enrolled",
            ("known", "enrollment_pending") => "enrollment pending",
            ("known", "enrollment_disabled") => "disabled",
            ("known", _) if host.enrolled && host.online => "online",
            ("known", _) if host.enrolled => "offline",
            _ => "status unavailable",
        };
        let access = if host.device_access.is_empty() {
            "unavailable"
        } else {
            &host.device_access
        };
        out.push_str(&format!(
            "{}\t{}\t{status}\t{}\n",
            sanitize_field(&host.host_id),
            sanitize_field(&host.display_name),
            sanitize_field(access),
        ));
    }
    out
}

pub fn hosts_data(hosts: &[HostInfo]) -> Value {
    json!({ "hosts": hosts })
}

pub fn render_sessions(list: &SessionList) -> String {
    if list.sessions.is_empty() {
        return format!(
            "No sessions on {}. Start one with `whalerein session create --host <host-id>`.\n",
            sanitize_field(&list.source)
        );
    }
    let mut out = String::from("SOURCE\tCOMPLETE\tSESSION\tPARENT\tORIGIN\tRUNNING\tTITLE\n");
    for session in &list.sessions {
        // Fork lineage is part of the session identity: an empty parent marks a
        // root session, any value names the session it was forked from.
        let parent = if session.parent_session_id.is_empty() {
            "-"
        } else {
            session.parent_session_id.as_str()
        };
        let origin = if session.origin.is_empty() {
            "-"
        } else {
            session.origin.as_str()
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            sanitize_field(&list.source),
            if list.complete { "yes" } else { "no" },
            sanitize_field(&session.session_id),
            sanitize_field(parent),
            sanitize_field(origin),
            if session.running { "yes" } else { "no" },
            sanitize_field(&session.title)
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
        sanitize_field(&handle.request_id),
        sanitize_field(&handle.command_id),
        handle.status.as_str()
    );
    if let Some(message_id) = &handle.message_id {
        out.push_str(&format!("legacy receipt: {}\n", sanitize_field(message_id)));
    }
    if let Some(receipt) = &handle.model_selection {
        if let Some(selection) = &receipt.selected {
            out.push_str(&format!(
                "provider: {}\nmodel: {}\n",
                sanitize_field(&selection.provider),
                sanitize_field(&selection.model)
            ));
            if let Some(effort) = &selection.effort {
                out.push_str(&format!("effort: {}\n", sanitize_field(effort)));
            }
        }
        out.push_str(&format!(
            "effect: {}\ndefault persistence: {}\n",
            sanitize_field(&receipt.effect_scope),
            sanitize_field(&receipt.default_persistence)
        ));
        // QMQ-15: the source default is unconfirmed on purpose. Tell the user
        // whether a retry is meaningful instead of leaving them to guess.
        out.push_str(
            "The session records this selection. Source-default persistence is unconfirmed: do not resend the command; verify with `whalerein session model get --session <session-id>` after the next turn. If `whalerein session models` lists a provider failure, retry only after that provider recovers.\n",
        );
    }
    if let Some(delivery_id) = &handle.delivery_id {
        out.push_str(&format!("delivery: {}\n", sanitize_field(delivery_id)));
    }
    if let Some(receipt) = &handle.question_receipt {
        out.push_str(&format!(
            "question: {}\nquestion outcome: {}\n",
            sanitize_field(&receipt.batch_id),
            sanitize_field(&receipt.phase)
        ));
    }
    if let Some(receipt) = &handle.lifecycle_receipt {
        out.push_str(&format!(
            "operation: {}\ndisposition: {}\n",
            sanitize_field(&receipt.operation),
            sanitize_field(&receipt.disposition)
        ));
        if !receipt.session_id.is_empty() {
            out.push_str(&format!(
                "session: {}\n",
                sanitize_field(&receipt.session_id)
            ));
        }
        if let Some(parent) = &receipt.parent_session_id {
            out.push_str(&format!("parent: {}\n", sanitize_field(parent)));
        }
        if let Some(phase) = &receipt.phase {
            out.push_str(&format!("phase: {}\n", sanitize_field(phase)));
        }
        if let Some(code) = &receipt.warning_code {
            out.push_str(&format!("warning: {}\n", sanitize_field(code)));
        }
        if let Some(reason) = &receipt.reason_code {
            out.push_str(&format!("reason: {}\n", sanitize_field(reason)));
        }
        if receipt.retryable {
            out.push_str("retryable: yes\n");
        }
        if let Some(id) = &receipt.operation_id {
            out.push_str(&format!("operation id: {}\n", sanitize_field(id)));
        }
        if let Some(mode) = &receipt.reconcile_mode {
            out.push_str(&format!("reconcile: {}", sanitize_field(mode)));
            if let Some(key) = &receipt.reconcile_key {
                out.push_str(&format!(" {}", sanitize_field(key)));
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
            format!(
                "{subject} {} is ready.\n",
                sanitize_field(&receipt.session_id)
            )
        }),
        "READY_WITH_WARNING" => {
            let warning = receipt
                .warning_code
                .as_deref()
                .unwrap_or("workspace attach incomplete");
            Some(format!(
                "{subject} {} exists, with a warning: {}.\nThe session was created; do not retry or delete it.\n",
                sanitize_field(&receipt.session_id),
                sanitize_field(warning)
            ))
        }
        "FAILED" => {
            let reason = receipt.reason_code.as_deref().unwrap_or("FAILED");
            Some(format!(
                "Operation failed before any native effect.\nreason: {}\nretryable: {}\n",
                sanitize_field(reason),
                if receipt.retryable { "yes" } else { "no" }
            ))
        }
        "UNKNOWN" => {
            let mut text = String::from(
                "Outcome unknown: this operation may have happened. The session may or may not exist.\nDo not retry automatically.\n",
            );
            if let Some(id) = &receipt.operation_id {
                text.push_str(&format!("operation id: {}\n", sanitize_field(id)));
            }
            if let Some(mode) = &receipt.reconcile_mode {
                text.push_str(&format!("reconcile: {}", sanitize_field(mode)));
                if let Some(key) = &receipt.reconcile_key {
                    text.push_str(&format!(" {}", sanitize_field(key)));
                }
                text.push('\n');
            }
            text.push_str(&format!(
                "Reconcile with: whalerein operation recover {}\n",
                sanitize_field(&handle.request_id)
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
            Some(reason) => lifecycle_reason_exit_code(reason),
            None => 5,
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
        Some("FAILED") => handle
            .lifecycle_receipt
            .as_ref()
            .and_then(|receipt| lifecycle_reason_error_code(receipt.reason_code.as_deref()?))
            .unwrap_or("operation_failed"),
        Some("UNKNOWN") => "unknown",
        _ => "internal",
    }
}

/// Wire error name for a lifecycle `reason_code`, so the envelope's `code` and
/// `exit` agree. Reasons without a wire code keep the CLI-level
/// `operation_failed` name and their phase exit code.
fn lifecycle_reason_error_code(reason_code: &str) -> Option<&'static str> {
    for prefix in ["PREFLIGHT_", "NATIVE_"] {
        if let Some(suffix) = reason_code.strip_prefix(prefix) {
            return whalerein_proto::ErrorCode::from_str_name(&suffix.to_ascii_lowercase())
                .map(|code| code.as_str_name());
        }
    }
    match reason_code {
        "INVALID_ARGUMENT" => Some("invalid_argument"),
        "WORKSPACE_NOT_FOUND" | "PRESET_NOT_FOUND" | "SOURCE_NOT_FOUND" => Some("not_found"),
        "WORKSPACE_UNAVAILABLE" | "PRESET_UNAVAILABLE" | "SOURCE_NO_COMPLETE_TURN" => {
            Some("conflict")
        }
        _ => None,
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
        sanitize_field(report.address.as_deref().unwrap_or("(not paired)")),
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
        out.push_str(&format!("  - {}\n", sanitize_field(field)));
    }
    out.push_str("Never included:\n");
    for redaction in &manifest.redactions {
        out.push_str(&format!("  - {}\n", sanitize_field(redaction)));
    }
    out.push_str(&format!(
        "Written to {} ({} bytes)\n",
        sanitize_field(&manifest.path),
        manifest.bytes
    ));
    out
}

pub fn manifest_data(manifest: &ExportManifest) -> Value {
    serde_json::to_value(manifest).unwrap_or_else(|_| json!({}))
}

fn render_model_choice(label: &str, choice: &Option<whalerein_core::api::ModelChoice>) -> String {
    match choice {
        Some(choice) => {
            let effort = choice
                .effort
                .as_deref()
                .map(|effort| format!(" effort={}", sanitize_field(effort)))
                .unwrap_or_default();
            format!(
                "{label}: {} {}{effort}\n",
                sanitize_field(&choice.provider),
                sanitize_field(&choice.model)
            )
        }
        None => format!("{label}: (none)\n"),
    }
}

/// Human model catalog. Provider failures are their own actionable lines;
/// a complete-but-empty catalog is not printed as `[]`.
pub fn render_models(catalog: &ModelCatalog) -> String {
    if catalog.groups.is_empty() && catalog.failures.is_empty() {
        return "No models advertised. Check the Host runtime compatibility or retry after it reconnects.\n".into();
    }
    let mut out = render_model_choice("default", &catalog.default_selection);
    let width = catalog
        .groups
        .iter()
        .flat_map(|group| group.models.iter())
        .map(|model| display_width(&sanitize_field(&model.id)))
        .max()
        .unwrap_or(0)
        .max("MODEL".len());
    out.push_str(&format!(
        "{}\t{}\tEFFORT\n",
        pad_cell("MODEL", width),
        "PROVIDER"
    ));
    for group in &catalog.groups {
        for model in &group.models {
            let effort = if model.default_effort.is_empty() {
                String::new()
            } else {
                format!("default={}", sanitize_field(&model.default_effort))
            };
            out.push_str(&format!(
                "{}\t{}\t{}\n",
                pad_cell(&sanitize_field(&model.id), width),
                sanitize_field(&group.id),
                effort
            ));
        }
    }
    for failure in &catalog.failures {
        out.push_str(&format!(
            "provider {} unavailable ({}); retry after the provider recovers.\n",
            sanitize_field(&failure.id),
            sanitize_field(&failure.code)
        ));
    }
    if !catalog.complete {
        out.push_str("Catalog is incomplete; some providers did not answer.\n");
    }
    out
}

/// Human session-model projection. Unknown pending state is stated explicitly.
pub fn render_session_model(model: &SessionModel) -> String {
    let mut out = render_model_choice("next", &model.next);
    out.push_str(&render_model_choice("last used", &model.last_used));
    out.push_str(&render_model_choice(
        "source default",
        &model.default_selection,
    ));
    if model.default_is_fallback {
        out.push_str("The source default is a fallback, not a recorded choice.\n");
    }
    if !model.pending_known {
        out.push_str("A pending selection cannot be proven for this session.\n");
    } else if model.pending.is_some() {
        out.push_str(&render_model_choice("pending", &model.pending));
    }
    if !model.pending_known && model.pending.is_none() && model.next.is_none() {
        out.push_str("Select one with `whalerein session model set --session <session-id> --provider <provider> --model <model>`.\n");
    }
    out
}

/// Human pending-queue projection.
pub fn render_queue(queue: &PendingQueue) -> String {
    if !queue.available {
        return format!(
            "Queue unavailable for session {}. The session may be cold; start it or retry.\n",
            sanitize_field(&queue.session_id)
        );
    }
    if queue.items.is_empty() {
        return "No queued prompts. Send one while a turn is running to queue it.\n".into();
    }
    let mut out = String::from("ITEM\tPLACEMENT\tACTIONS\tPREVIEW\n");
    for item in &queue.items {
        // The typed queue item is public through JSON only; read its own
        // normalized shape instead of depending on an unexported enum.
        let value = serde_json::to_value(item).unwrap_or(Value::Null);
        let preview = value["blocks"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .map(|block| match block["type"].as_str().unwrap_or_default() {
                        "text" => sanitize_field(block["text"].as_str().unwrap_or_default()),
                        "attachment" => format!(
                            "[attachment {} {}]",
                            sanitize_field(block["name"].as_str().unwrap_or_default()),
                            sanitize_field(block["media_type"].as_str().unwrap_or_default())
                        ),
                        _ => "[unavailable]".to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            sanitize_field(value["item_id"].as_str().unwrap_or_default()),
            sanitize_field(value["placement"].as_str().unwrap_or_default()),
            sanitize_field(
                value["actions"]
                    .as_array()
                    .map(|actions| {
                        actions
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default()
                    .as_str()
            ),
            preview
        ));
    }
    if queue.running {
        out.push_str("A turn is running; queued items are delivered in placement order.\n");
    }
    out
}

/// Human pending-question projection.
pub fn render_questions(page: &QuestionPageInfo) -> String {
    if page.batches.is_empty() {
        let mut out = String::from(
            "No pending questions. Re-run `whalerein session question list --session <session-id>` after the Agent asks one.\n",
        );
        if !page.next_cursor.is_empty() {
            out.push_str(&format!(
                "More batches exist; continue with --cursor {}.\n",
                sanitize_field(&page.next_cursor)
            ));
        }
        return out;
    }
    let mut out = String::new();
    for batch in &page.batches {
        out.push_str(&format!(
            "batch {} phase={} version={} answerable={} questions={}\n",
            sanitize_field(&batch.batch_id),
            sanitize_field(&batch.phase),
            batch.version,
            if batch.answerable { "yes" } else { "no" },
            batch.questions.len()
        ));
        for question in &batch.questions {
            out.push_str(&format!(
                "  - {}: {}{}\n",
                sanitize_field(&question.id),
                sanitize_field(&question.question),
                if question.multi_select {
                    " (multi-select)"
                } else {
                    ""
                }
            ));
        }
        if batch.content_unavailable {
            out.push_str("  question content is unavailable; refresh before answering.\n");
        }
    }
    if !page.next_cursor.is_empty() {
        out.push_str(&format!(
            "Continue with --cursor {}.\n",
            sanitize_field(&page.next_cursor)
        ));
    }
    out
}

/// Human pairing history. Redacted identities only; no token is ever shown.
pub fn render_pairing_history(records: &[PairingHistoryRecord]) -> String {
    if records.is_empty() {
        return "No retained pairing records. There is nothing to review here; pair with `whalerein host connect --invite-stdin`.\n".into();
    }
    let mut out = String::from("CLAIM\tHOST\tSTATUS\tFINGERPRINT\n");
    for record in records {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            sanitize_field(&record.claim_id),
            sanitize_field(&record.host_id),
            sanitize_field(&record.status),
            sanitize_field(&record.fingerprint)
        ));
    }
    out.push_str("Records marked unconfirmed_revoked require manual verification and cannot resubmit the original pairing.\n");
    out
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
            json!({"type":"message","message_id":message.message_id,"role":message.role.as_str_name(),"blocks":blocks,"authored_at_unix_ms":message.authored_at_unix_ms,"origin":origin.as_ref().map(|a|json!({"live_epoch":a.live_epoch,"attempt_id":a.attempt_id}))})
        }
        ViewUpdate::AttemptEnded { attempt, outcome } => {
            json!({"type":"attempt_ended","attempt_id":attempt.attempt_id,"live_epoch":attempt.live_epoch,"outcome":outcome.as_str_name()})
        }
        ViewUpdate::State(state) => {
            let approvals:Vec<Value>=state.approvals.iter().map(|a|json!({"approval_id":a.approval_id,"session_id":a.session_id,"tool_name":a.tool_name,"call_id":a.call_id,"reason":a.reason,"digest":hex::encode(&a.digest),"version":a.version,"expires_at_unix_ms":a.expires_at_unix_ms,"state":a.state.as_str_name()})).collect();
            json!({"type":"state","run_phase":state.run_phase.as_str_name(),"approvals":approvals})
        }
        ViewUpdate::Reset => json!({"type":"reset"}),
    };
    json!({"schema":"whalerein/client/v1","command":"session.watch","data":{"event":event}})
}

/// Formatting owns terminal boundaries only; Core owns preview ordering and completion.
#[derive(Default)]
pub struct TailRenderer {
    line_open: bool,
    attempt: Option<String>,
    block: Option<String>,
    last_state: Option<String>,
}
impl TailRenderer {
    pub fn render(&mut self, update: &ViewUpdate) -> String {
        match update {
            ViewUpdate::Preview {
                attempt,
                block_id,
                text,
                replace,
                ..
            } => {
                // Each preview block is its own line. A block-level reset
                // closes and reprints only that block, never the message.
                let new_block = self.block.as_deref() != Some(block_id);
                let prefix = if self.attempt.as_deref() != Some(&attempt.attempt_id)
                    || *replace
                    || new_block
                {
                    self.finish_line()
                } else {
                    String::new()
                };
                self.attempt = Some(attempt.attempt_id.clone());
                self.block = Some(block_id.clone());
                self.line_open |= !text.is_empty();
                format!("{prefix}{text}")
            }
            ViewUpdate::Message {
                message,
                preview_prefix_bytes,
                ..
            } => {
                if let Some(offset) = preview_prefix_bytes {
                    // The previewed prefix is already on screen. Emit only the
                    // suffix on the same line, then terminate that line.
                    self.line_open = false;
                    self.attempt = None;
                    self.block = None;
                    let body = render_message_blocks(message, *offset);
                    if body.is_empty() {
                        return "\n".into();
                    }
                    return format!("{body}\n");
                }
                let prefix = self.finish_line();
                self.attempt = None;
                self.block = None;
                let body = render_message_blocks(message, 0);
                let role = message.role.as_str_name();
                if body.is_empty() {
                    format!("{prefix}[{role}]\n")
                } else {
                    format!("{prefix}[{role}] {body}\n")
                }
            }
            ViewUpdate::State(state) => {
                let phase = state.run_phase;
                let mut line = format!("[state] {}", run_phase_label(phase));
                if !state.approvals.is_empty() {
                    let tools: Vec<String> = state
                        .approvals
                        .iter()
                        .map(|approval| sanitize_field(&approval.tool_name))
                        .collect();
                    line.push_str(&format!(
                        " input-required: approval for {}",
                        tools.join(", ")
                    ));
                }
                if self.last_state.as_deref() == Some(line.as_str()) {
                    return String::new();
                }
                self.last_state = Some(line.clone());
                let prefix = self.finish_line();
                format!("{prefix}{line}\n")
            }
            ViewUpdate::Reset => {
                self.attempt = None;
                self.block = None;
                self.finish_line()
            }
            ViewUpdate::AttemptEnded { outcome, .. } if *outcome != AttemptOutcome::Committed => {
                self.attempt = None;
                self.block = None;
                let prefix = self.finish_line();
                let label = attempt_outcome_label(*outcome);
                format!("{prefix}[attempt {label}]\n")
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
fn run_phase_label(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Idle => "idle",
        RunPhase::Running => "running",
        RunPhase::Stopping => "stopping",
        RunPhase::SourceUnavailable => "source-unavailable",
        RunPhase::Unspecified => "unspecified",
    }
}

pub fn is_tail_output(update: &ViewUpdate) -> bool {
    matches!(update, ViewUpdate::Message { .. })
        || matches!(update,ViewUpdate::Preview{text,..} if !text.is_empty())
        // An interrupted/abandoned turn is the only output of that attempt:
        // `tail --once` must surface it instead of timing out silently.
        || matches!(update, ViewUpdate::AttemptEnded { outcome, .. } if *outcome != AttemptOutcome::Committed)
}

pub fn render_pair(outcome: &whalerein_core::api::PairingOutcome) -> String {
    format!(
        "Paired host {} as device {}\nFingerprint: {}\n",
        sanitize_field(&outcome.host_id),
        sanitize_field(&outcome.device_id),
        sanitize_field(&outcome.fingerprint)
    )
}
/// JSON data for one approval read.
///
/// The nested `ApprovalInfo` carries the wire `APPROVAL_STATE_*` /
/// `APPROVAL_DECISION_*` names, exactly like the `tail` state JSON, so a
/// script uses one vocabulary for both read paths.
pub fn approval_data(approval: &whalerein_core::api::ApprovalInfo) -> Value {
    json!({ "approval": approval })
}

/// Human detail for one approval read. State and decision use the same wire
/// names as the JSON paths.
pub fn render_approval(approval: &whalerein_core::api::ApprovalInfo) -> String {
    let mut out = format!(
        "approval: {}\nstate: {}\ndecision: {}\nsession: {}\ntool: {}\nversion: {}\ndigest: {}\nreason: {}\nexpires at: {}\n",
        sanitize_field(&approval.approval_id),
        sanitize_field(&approval.state),
        sanitize_field(&approval.decision),
        sanitize_field(&approval.session_id),
        sanitize_field(&approval.tool_name),
        approval.version,
        hex::encode(&approval.digest),
        sanitize_field(&approval.reason),
        approval.expires_at_unix_ms
    );
    if !approval.call_id.is_empty() {
        out.push_str(&format!("call: {}\n", sanitize_field(&approval.call_id)));
    }
    out
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
                sanitize_field(&a.approval_id),
                sanitize_field(&a.tool_name),
                a.version,
                hex::encode(&a.digest),
                sanitize_field(&a.reason)
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
            schema: "whalerein/client/v1".into(),
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
        assert_eq!(value["schema"], "whalerein/client/v1");
        assert_eq!(value["command"], "host.list");
        assert!(value.get("data").is_some());
        assert!(value.get("error").is_none());
    }

    #[test]
    fn emits_stable_json_error_shape_for_failed_outcomes() {
        let outcome = CliOutcome::failure(&command(), CliError::new("unauthorized", "no"));
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/client/v1");
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
        assert_eq!(outcome.command, "host.list");
        assert_eq!(outcome.error.unwrap().code, "host_offline");
        assert_eq!(outcome.exit, 5);
    }

    #[test]
    fn unsupported_send_includes_a_runtime_compatibility_hint() {
        let command = Command::Send {
            session: "fixture".into(),
            delivery: crate::cli::Delivery::EnqueueAfterRun,
            detach: false,
            timeout: None,
            operation_id: None,
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
    fn cloud_hosts_show_the_command_id_and_distinguish_unavailable_enrollment() {
        let cloud: Vec<whalerein_core::api::AccountHost> = serde_json::from_value(json!([
            {"hostId":"host-id-1", "displayName":"Computer", "online":false, "enrolled":false, "observation":"unavailable"},
            {"hostId":"host-id-2", "displayName":"Computer", "online":false, "enrolled":false, "observation":"missing"},
            {"hostId":"host-id-3", "displayName":"Other", "online":true, "enrolled":true, "observation":"known", "enrollmentState":"enrollment_ready"},
            {"hostId":"host-id-4", "displayName":"Other", "online":false, "enrolled":true, "observation":"known", "enrollmentState":"enrollment_pending"},
            {"hostId":"host-id-5", "displayName":"Other", "online":false, "enrolled":true, "observation":"known", "enrollmentState":"enrollment_disabled"},
            {"hostId":"host-id-6", "displayName":"Other", "online":false, "enrolled":true, "observation":"known", "enrollmentState":"enrollment_ready"}
        ])).unwrap();
        assert_eq!(
            render_account_hosts(&cloud),
            concat!(
                "HOST ID\tNAME\tHOST STATUS\tDEVICE ACCESS\n",
                "host-id-1\tComputer\tstatus unavailable\tunavailable\n",
                "host-id-2\tComputer\tnot enrolled\tunavailable\n",
                "host-id-3\tOther\tonline\tunavailable\n",
                "host-id-4\tOther\tenrollment pending\tunavailable\n",
                "host-id-5\tOther\tdisabled\tunavailable\n",
                "host-id-6\tOther\toffline\tunavailable\n",
            )
        );
        assert_eq!(json!(cloud)[0]["host_id"], "host-id-1");
        assert!(render_account_hosts(&[]).starts_with("No computers"));
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
            schema: "whalerein/client/v1".into(),
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
        assert!(unknown.contains("operation recover r-1"));
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
                assert_eq!(value["error"]["code"], "not_found");
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
    use whalerein_core::conversation::{AttemptRef, ConversationMessage, MessageRole};
    fn attempt() -> AttemptRef {
        AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        }
    }
    fn message() -> ConversationMessage {
        ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
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
                outcome: AttemptOutcome::Abandoned
            }),
            "\n[attempt abandoned]\n"
        );
        assert!(renderer
            .render(&ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Committed
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
    fn human_tail_renders_blocks_in_source_order_with_attachments_and_notices() {
        use whalerein_core::conversation::content_block::{
            AttachmentContent, Content, NoticeContent, TextContent, ToolCallContent,
            ToolResultContent,
        };
        use whalerein_core::conversation::ContentBlock;
        let message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock {
                    block_id: "text".into(),
                    content: Some(Content::Text(TextContent {
                        text: "hello".into(),
                    })),
                },
                ContentBlock {
                    block_id: "call".into(),
                    content: Some(Content::ToolCall(ToolCallContent {
                        call_id: "c".into(),
                        name: "Read".into(),
                        summary: "src/main.rs".into(),
                    })),
                },
                ContentBlock {
                    block_id: "file".into(),
                    content: Some(Content::Attachment(AttachmentContent {
                        media_type: "image/png".into(),
                        name: "shot.png".into(),
                        size: 42,
                        available: true,
                    })),
                },
                ContentBlock {
                    block_id: "notice".into(),
                    content: Some(Content::Notice(NoticeContent {
                        category: "activity".into(),
                        text: "source resumed".into(),
                    })),
                },
                ContentBlock {
                    block_id: "result".into(),
                    content: Some(Content::ToolResult(ToolResultContent {
                        call_id: "c".into(),
                        text: "done".into(),
                        is_error: false,
                    })),
                },
            ],
            ..Default::default()
        };
        let mut renderer = TailRenderer::default();
        let out = renderer.render(&ViewUpdate::Message {
            message,
            origin: None,
            preview_prefix_bytes: None,
        });
        let positions = [
            out.find("hello").unwrap(),
            out.find("[tool] Read").unwrap(),
            out.find("[attachment shot.png").unwrap(),
            out.find("[notice activity]").unwrap(),
            out.find("[tool result] done").unwrap(),
        ];
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "blocks must keep source order: {out}"
        );
        assert_eq!(out.matches("hello").count(), 1, "{out}");
    }

    #[test]
    fn human_tail_surfaces_an_interrupted_attempt_as_visible_output() {
        let mut renderer = TailRenderer::default();
        let interrupted = ViewUpdate::AttemptEnded {
            attempt: attempt(),
            outcome: AttemptOutcome::Interrupted,
        };
        assert!(is_tail_output(&interrupted));
        assert_eq!(renderer.render(&interrupted), "[attempt interrupted]\n");
        // A committed attempt is described by its Message, not a synthetic line.
        assert!(!is_tail_output(&ViewUpdate::AttemptEnded {
            attempt: attempt(),
            outcome: AttemptOutcome::Committed,
        }));
    }

    #[test]
    fn human_tail_prints_state_transitions_including_input_required() {
        use whalerein_core::conversation::{Approval, ApprovalState, RunPhase, SessionState};
        let mut renderer = TailRenderer::default();
        let mut state = SessionState {
            run_phase: RunPhase::Running,
            ..Default::default()
        };
        assert!(renderer
            .render(&ViewUpdate::State(state.clone()))
            .contains("[state] running"));
        state.run_phase = RunPhase::Stopping;
        assert!(renderer
            .render(&ViewUpdate::State(state.clone()))
            .contains("[state] stopping"));
        state.run_phase = RunPhase::Idle;
        state.approvals = vec![Approval {
            approval_id: "a".into(),
            tool_name: "Write".into(),
            state: ApprovalState::Pending,
            ..Default::default()
        }];
        let rendered = renderer.render(&ViewUpdate::State(state));
        assert!(rendered.contains("[state] idle"), "{rendered}");
        assert!(
            rendered.contains("input-required: approval for Write"),
            "{rendered}"
        );
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
            outcome: AttemptOutcome::Committed,
        });
        assert_eq!(ended["data"]["event"]["outcome"], "COMMITTED");
    }
    #[test]
    fn state_reset_and_empty_preview_do_not_satisfy_visible_tail_output() {
        let mut renderer = TailRenderer::default();
        let state = ViewUpdate::State(Default::default());
        let rendered = renderer.render(&state);
        assert!(rendered.contains("[state]"), "{rendered}");
        assert_eq!(
            renderer.render(&state),
            "",
            "identical states are deduplicated"
        );
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
            operation_id: None,
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
    use whalerein_core::conversation::{ApprovalState, ConversationMessage, MessageRole};
    #[test]
    fn json_formats_every_declared_content_family_without_native_payloads() {
        use content_block::Content;
        let mut message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
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
            a.state = ApprovalState::Pending;
            s.run_phase = RunPhase::Idle;
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

#[cfg(test)]
mod cli_contract_tests {
    use super::*;
    use whalerein_core::api::{CommandHandle, OnlineStatus};
    use whalerein_proto::ALL_ERROR_CODES;

    fn lifecycle(reason: &str) -> CommandHandle {
        CommandHandle {
            request_id: "r-1".into(),
            command_id: "c-1".into(),
            status: OnlineStatus::Succeeded,
            model_selection: None,
            message_id: None,
            delivery_id: None,
            lifecycle_receipt: Some(whalerein_core::api::LifecycleReceipt {
                session_id: String::new(),
                parent_session_id: None,
                operation: "create".into(),
                disposition: "FAILED".into(),
                warning_code: None,
                operation_id: Some("op-1".into()),
                phase: Some("PREFLIGHT".into()),
                reason_code: Some(reason.into()),
                retryable: true,
                reconcile_mode: None,
                reconcile_key: None,
                warning_detail: None,
            }),
            question_receipt: None,
        }
    }

    #[test]
    fn every_wire_error_code_maps_to_the_core_exit_table() {
        for code in ALL_ERROR_CODES {
            let name = code.as_str_name();
            assert_eq!(
                error_exit_code(name),
                whalerein_core::api::error_code_exit_code(code),
                "{name}"
            );
            // Error codes are produced from CoreError; the CLI string table must
            // agree with the error's own exit code for both remote and local
            // variants of the same code.
            for error in [
                whalerein_core::CoreError::Api {
                    code: Some(code),
                    message: "m".into(),
                },
                whalerein_core::CoreError::Host {
                    code: Some(code),
                    message: "m".into(),
                },
            ] {
                assert_eq!(error_exit_code(error.code()), error.exit_code(), "{name}");
            }
        }
        // Local codes that have no wire ErrorCode stay classified.
        for (code, exit) in [
            ("invalid_configuration", 2),
            ("cancelled", 2),
            ("not_paired", 4),
            ("revoked", 4),
            ("host_offline", 5),
            ("rejected", 6),
            ("timeout", 7),
            ("unknown", 7),
            ("cache_error", 1),
            ("security_error", 1),
            ("something_unknown", 1),
        ] {
            assert_eq!(error_exit_code(code), exit, "{code}");
        }
    }

    #[test]
    fn the_cli_error_contract_matches_the_proto_json_table() {
        let table: serde_json::Value =
            serde_json::from_str(include_str!("../../../proto/v1/error_codes.json")).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for entry in table["codes"].as_array().unwrap() {
            let name = entry["name"].as_str().unwrap();
            let exit = entry["exitCode"].as_u64().unwrap() as u8;
            let action = entry["action"].as_str().unwrap();
            assert_eq!(error_exit_code(name), exit, "exit for {name}");
            assert_eq!(error_action(name), action, "action for {name}");
            let wire = whalerein_proto::ErrorCode::from_str_name(name).expect(name);
            assert_eq!(whalerein_core::api::error_code_exit_code(wire), exit);
            assert_eq!(whalerein_core::api::error_code_action(wire), action);
            assert_eq!(wire.as_str_name(), name);
            seen.insert(name.to_owned());
        }
        assert_eq!(seen.len(), whalerein_proto::ALL_ERROR_CODES.len());
        for code in whalerein_proto::ALL_ERROR_CODES {
            assert!(seen.contains(code.as_str_name()), "{}", code.as_str_name());
        }
    }

    #[test]
    fn upgrade_codes_are_terminal_and_never_suggest_a_retry() {
        for error in [
            whalerein_core::CoreError::Unsupported,
            whalerein_core::CoreError::ProtocolIncompatible,
            whalerein_core::CoreError::Api {
                code: Some(whalerein_proto::ErrorCode::ProtocolVersionIncompatible),
                message: "old peer".into(),
            },
            whalerein_core::CoreError::Host {
                code: Some(whalerein_proto::ErrorCode::UnsupportedCapability),
                message: "no capability".into(),
            },
        ] {
            let outcome = core_error_outcome(&Command::Hosts, error);
            assert_eq!(outcome.exit, 9);
            let value = outcome.to_json();
            assert_eq!(value["error"]["action"], "upgrade");
            let message = value["error"]["message"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase();
            assert!(message.contains("upgrade"), "{message}");
            assert!(!message.contains("retry later"), "{message}");
            assert!(!message.contains("please retry"), "{message}");
            assert!(
                message.contains("do not retry") || !message.contains("retry"),
                "{message}"
            );
        }
    }

    #[test]
    fn rebuilt_host_identity_guidance_names_the_recovery_commands() {
        let outcome = core_error_outcome(
            &Command::Hosts,
            whalerein_core::CoreError::Security("peer key mismatch".into()),
        );
        assert_eq!(outcome.exit, 1);
        let value = outcome.to_json();
        assert_eq!(value["error"]["action"], "retry");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains("revoke --yes"), "{message}");
        assert!(message.contains("forget --local-only --yes"), "{message}");
        assert!(message.contains("host connect --invite-stdin"), "{message}");
    }

    #[test]
    fn remote_error_codes_keep_their_wire_exits_at_the_cli_boundary() {
        use whalerein_proto::ErrorCode;
        for (code, exit) in [
            (ErrorCode::ApprovalExpired, 8),
            (ErrorCode::ApprovalVersionMismatch, 8),
            (ErrorCode::ApprovalAlreadyResolved, 8),
            (ErrorCode::ApprovalUnavailable, 8),
            (ErrorCode::RateLimited, 8),
            (ErrorCode::SnapshotRequired, 8),
            (ErrorCode::RevocationsPending, 8),
            (ErrorCode::PayloadDigestMismatch, 8),
            (ErrorCode::ProtocolVersionIncompatible, 9),
            (ErrorCode::Expired, 7),
            (ErrorCode::PayloadTooLarge, 5),
        ] {
            let outcome = core_error_outcome(
                &Command::Hosts,
                whalerein_core::CoreError::Api {
                    code: Some(code),
                    message: "remote".into(),
                },
            );
            assert_eq!(outcome.exit, exit, "{code:?}");
            assert_eq!(outcome.error.as_ref().unwrap().code, code.as_str_name());
            assert_eq!(
                outcome.error.as_ref().unwrap().layer.as_deref(),
                Some("access")
            );
        }
        // Local rejected is exit 6, which the audit found unasserted anywhere.
        let outcome = core_error_outcome(&Command::Hosts, whalerein_core::CoreError::HostRejected);
        assert_eq!(outcome.exit, 6);
        assert_eq!(outcome.error.unwrap().code, "rejected");
    }

    #[test]
    fn the_lifecycle_reason_golden_table_covers_plain_and_wrapped_codes() {
        // Every plain reason the Host can put in a FAILED receipt.
        for (reason, exit) in [
            ("INVALID_ARGUMENT", 2),
            ("WORKSPACE_NOT_FOUND", 3),
            ("WORKSPACE_UNAVAILABLE", 8),
            ("PRESET_NOT_FOUND", 3),
            ("PRESET_UNAVAILABLE", 8),
            ("SOURCE_NOT_FOUND", 3),
            ("SOURCE_NO_COMPLETE_TURN", 8),
            ("PROCESS_INTERRUPTED", 5),
        ] {
            assert_eq!(lifecycle_reason_exit_code(reason), exit, "{reason}");
            let handle = lifecycle(reason);
            assert_eq!(lifecycle_exit_code(&handle), exit, "{reason}");
            assert_eq!(
                error_exit_code(lifecycle_error_code(&handle)),
                exit,
                "{reason}: {}",
                lifecycle_error_code(&handle)
            );
        }
        // PROCESS_INTERRUPTED is an availability failure, never a silent
        // success and never a retry suggestion.
        let handle = lifecycle("PROCESS_INTERRUPTED");
        assert_eq!(lifecycle_error_code(&handle), "operation_failed");
        assert_eq!(error_action(lifecycle_error_code(&handle)), "retry");
        // Every wire code decodes through the PREFLIGHT_/NATIVE_ prefixes.
        for code in ALL_ERROR_CODES {
            let upper = code.as_str_name().to_ascii_uppercase();
            for prefix in ["PREFLIGHT_", "NATIVE_"] {
                let reason = format!("{prefix}{upper}");
                assert_eq!(
                    lifecycle_reason_exit_code(&reason),
                    whalerein_core::api::error_code_exit_code(code),
                    "{reason}"
                );
            }
        }
        // The audit's contradiction: conflict is 8, not the generic 5.
        assert_eq!(lifecycle_reason_exit_code("PREFLIGHT_CONFLICT"), 8);
        assert_ne!(lifecycle_reason_exit_code("PREFLIGHT_CONFLICT"), 5);
        assert_eq!(
            lifecycle_reason_exit_code("PREFLIGHT_UNSUPPORTED_CAPABILITY"),
            9
        );
    }

    #[test]
    fn every_lifecycle_reason_maps_to_a_documented_exit_code() {
        for (reason, exit) in [
            ("INVALID_ARGUMENT", 2),
            ("PREFLIGHT_INVALID_ARGUMENT", 2),
            ("WORKSPACE_NOT_FOUND", 3),
            ("PRESET_NOT_FOUND", 3),
            ("SOURCE_NOT_FOUND", 3),
            ("PREFLIGHT_NOT_FOUND", 3),
            ("WORKSPACE_UNAVAILABLE", 8),
            ("PRESET_UNAVAILABLE", 8),
            ("SOURCE_NO_COMPLETE_TURN", 8),
            ("PREFLIGHT_CONFLICT", 8),
            ("PREFLIGHT_UNSUPPORTED_CAPABILITY", 9),
            ("PREFLIGHT_PROTOCOL_VERSION_INCOMPATIBLE", 9),
            ("PREFLIGHT_UNAVAILABLE", 5),
            ("PREFLIGHT_EXPIRED", 7),
            ("NATIVE_CONFLICT", 8),
            ("NATIVE_UNAVAILABLE", 5),
            ("NATIVE_INTERNAL", 1),
            ("NATIVE_EXPIRED", 7),
            ("NATIVE_PAYLOAD_TOO_LARGE", 5),
            ("NATIVE_SOMETHING_NEW", 5),
            ("PREFLIGHT_SOMETHING_NEW", 5),
            ("UNKNOWN_REASON", 5),
        ] {
            assert_eq!(lifecycle_reason_exit_code(reason), exit, "{reason}");
            let mut handle = lifecycle(reason);
            assert_eq!(lifecycle_exit_code(&handle), exit, "{reason}");
            // The envelope's code and exit must agree for scripts that map one
            // to the other.
            assert_eq!(
                error_exit_code(lifecycle_error_code(&handle)),
                exit,
                "{reason}: {}",
                lifecycle_error_code(&handle)
            );
            handle.lifecycle_receipt = None;
            assert_eq!(lifecycle_exit_code(&handle), 0);
        }
        // UNKNOWN never claims success even with a benign reason.
        let mut handle = lifecycle("UNKNOWN_REASON");
        handle.lifecycle_receipt.as_mut().unwrap().disposition = "UNKNOWN".into();
        assert_eq!(lifecycle_exit_code(&handle), 7);
    }

    #[test]
    fn lifecycle_exit_codes_match_wire_conflict_and_capability_rules() {
        // The audit found PREFLIGHT_CONFLICT falling through to 5 while WIRE
        // requires conflict to be 8, and unsupported capability to be 9.
        assert_eq!(lifecycle_reason_exit_code("PREFLIGHT_CONFLICT"), 8);
        assert_eq!(
            lifecycle_reason_exit_code("PREFLIGHT_UNSUPPORTED_CAPABILITY"),
            9
        );
        assert_ne!(lifecycle_reason_exit_code("PREFLIGHT_CONFLICT"), 5);
        for code in ALL_ERROR_CODES {
            let reason = format!("PREFLIGHT_{}", code.as_str_name().to_ascii_uppercase());
            assert_eq!(
                lifecycle_reason_exit_code(&reason),
                whalerein_core::api::error_code_exit_code(code),
                "{reason}"
            );
            let reason = format!("NATIVE_{}", code.as_str_name().to_ascii_uppercase());
            assert_eq!(
                lifecycle_reason_exit_code(&reason),
                whalerein_core::api::error_code_exit_code(code),
                "{reason}"
            );
        }
    }

    #[test]
    fn human_fields_escape_layout_and_terminal_control_characters() {
        assert_eq!(sanitize_field("plain"), "plain");
        assert_eq!(sanitize_field("a\tb"), "a\\tb");
        assert_eq!(sanitize_field("a\nb"), "a\\nb");
        assert_eq!(sanitize_field("a\rb"), "a\\rb");
        assert_eq!(sanitize_field("a\u{7}b"), "a\\u{7}b");
        // CSI color and OSC title sequences are removed entirely.
        assert_eq!(sanitize_field("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(sanitize_field("\u{1b}]0;title\u{7}text"), "text");
        assert!(!sanitize_field("\u{1b}[2J\u{1b}[H").contains('\u{1b}'));
        // CJK is preserved; it is content, not control.
        assert_eq!(sanitize_field("办公室"), "办公室");
    }

    #[test]
    fn cjk_width_is_two_columns_and_padding_uses_columns() {
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("办公室"), 6);
        assert_eq!(display_width("a办"), 3);
        assert_eq!(pad_cell("办公室", 6), "办公室");
        assert_eq!(pad_cell("ab", 6), "ab    ");
        assert_eq!(pad_cell("办公室", 2), "办公室");
        // A sanitized field counts the escaped form, not the raw control bytes.
        assert_eq!(display_width(&sanitize_field("a\tb")), 4);
    }

    fn catalog() -> ModelCatalog {
        // Provider/entry/failure structs are private to Core's public API, so
        // build the catalog from its documented JSON shape.
        serde_json::from_value(serde_json::json!({
            "default_selection": {"provider": "prov", "model": "m-1", "effort": null},
            "groups": [{
                "id": "prov",
                "name": "Provider",
                "models": [{
                    "id": "办公室-model",
                    "name": "Office",
                    "efforts": [],
                    "default_effort": "high",
                }],
            }],
            "failures": [{"id": "other", "name": "Other", "code": "unavailable"}],
            "effect_scope": "session",
            "complete": false,
        }))
        .unwrap()
    }

    #[test]
    fn model_catalog_renders_a_table_with_actionable_failures() {
        let text = render_models(&catalog());
        assert!(text.contains("default: prov m-1"), "{text}");
        assert!(text.contains("MODEL"), "{text}");
        assert!(text.contains("PROVIDER\tEFFORT"), "{text}");
        assert!(text.contains("办公室-model\tprov\tdefault=high"), "{text}");
        assert!(
            text.contains("provider other unavailable (unavailable)"),
            "{text}"
        );
        assert!(text.contains("Catalog is incomplete"));
        assert!(!text.contains('{'));
        let empty: ModelCatalog = serde_json::from_value(serde_json::json!({
            "default_selection": null,
            "groups": [],
            "failures": [],
            "effect_scope": "",
            "complete": true,
        }))
        .unwrap();
        let empty = render_models(&empty);
        assert!(empty.contains("No models advertised"), "{empty}");
        assert!(!empty.trim().is_empty());
        assert!(!empty.contains("[]"));
    }

    #[test]
    fn session_model_and_queue_human_output_never_prints_pretty_json() {
        let model = SessionModel {
            pending: None,
            last_used: None,
            default_selection: None,
            default_is_fallback: true,
            next: None,
            pending_known: false,
        };
        let text = render_session_model(&model);
        assert!(text.contains("next: (none)"));
        assert!(text.contains("fallback"));
        assert!(text.contains("model set --session"));
        assert!(!text.contains('{'));
        let queue = PendingQueue {
            session_id: "session-1".into(),
            available: false,
            running: false,
            items: vec![],
        };
        let text = render_queue(&queue);
        assert!(text.contains("Queue unavailable for session session-1"));
        assert!(text.contains("retry"));
        let available = PendingQueue {
            available: true,
            ..queue
        };
        let text = render_queue(&available);
        assert!(text.contains("No queued prompts"), "{text}");
        assert!(!text.contains("[]"));
    }

    fn approval(state: &str, decision: &str) -> whalerein_core::api::ApprovalInfo {
        whalerein_core::api::ApprovalInfo {
            approval_id: "approval-1".into(),
            session_id: "session-1".into(),
            tool_name: "bash".into(),
            call_id: "call-1".into(),
            reason: "review\tthe command".into(),
            digest: vec![1; 32],
            version: 3,
            expires_at_unix_ms: 9,
            state: state.into(),
            decision: decision.into(),
        }
    }

    #[test]
    fn approval_read_json_and_human_use_the_wire_state_casing() {
        for state in [
            "APPROVAL_STATE_PENDING",
            "APPROVAL_STATE_APPROVED",
            "APPROVAL_STATE_REJECTED",
            "APPROVAL_STATE_EXPIRED",
            "APPROVAL_STATE_UNAVAILABLE",
        ] {
            let info = approval(state, "APPROVAL_DECISION_UNSPECIFIED");
            let value = approval_data(&info);
            // One APPROVAL_STATE_* vocabulary shared with the tail state JSON.
            assert_eq!(value["approval"]["state"], state);
            assert_eq!(
                value["approval"]["decision"],
                "APPROVAL_DECISION_UNSPECIFIED"
            );
            assert!(
                value.get("state").is_none(),
                "no second casing at the top level"
            );
            let text = render_approval(&info);
            assert!(text.contains(&format!("state: {state}")), "{text}");
            assert!(text.contains("decision: APPROVAL_DECISION_UNSPECIFIED"));
            assert!(text.contains("approval: approval-1"));
            assert!(text.contains("tool: bash"));
            assert!(text.contains("version: 3"));
            assert!(text.contains(&format!("digest: {}", "01".repeat(32))));
            assert!(text.contains("call: call-1"));
            // Hostile reason text cannot break the line or inject ANSI.
            assert!(text.contains("review\\tthe command"), "{text}");
            assert!(!text.contains('\u{1b}'));
        }
        let decided = approval("APPROVAL_STATE_APPROVED", "APPROVAL_DECISION_APPROVE");
        assert_eq!(
            approval_data(&decided)["approval"]["decision"],
            "APPROVAL_DECISION_APPROVE"
        );
        assert!(render_approval(&decided).contains("decision: APPROVAL_DECISION_APPROVE"));
        // The pending-list renderer keeps its one-line shape.
        assert_eq!(
            render_approvals(&[approval("APPROVAL_STATE_PENDING", "x")]),
            format!(
                "approval-1 bash version=3 digest={} review\\tthe command\n",
                "01".repeat(32)
            )
        );
    }

    #[test]
    fn pairing_history_human_output_is_a_stable_table() {
        let empty = render_pairing_history(&[]);
        assert!(empty.contains("No retained pairing records"), "{empty}");
        assert!(
            empty.contains("whalerein host connect --invite-stdin"),
            "{empty}"
        );
        let records = vec![
            PairingHistoryRecord {
                claim_id: "claim-1".into(),
                host_id: "host-1".into(),
                fingerprint: "aa".repeat(32),
                status: "committed".into(),
            },
            PairingHistoryRecord {
                claim_id: "claim\t2".into(),
                host_id: "host\u{1b}[31m2".into(),
                fingerprint: "bb".repeat(32),
                status: "unconfirmed_revoked".into(),
            },
        ];
        let text = render_pairing_history(&records);
        assert!(text.contains("CLAIM\tHOST\tSTATUS\tFINGERPRINT"), "{text}");
        assert!(text.contains("claim-1\thost-1\tcommitted\t"), "{text}");
        assert!(text.contains("claim\\t2"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(text.contains("manual verification"), "{text}");
        assert!(text.contains("unconfirmed_revoked"), "{text}");
        // No token or key material is part of a retained record.
        assert!(!text.contains("token"));
        assert!(!text.contains("private key"));
    }

    #[test]
    fn session_rendering_shows_fork_lineage_and_parent_identity() {
        let session = |id: &str, parent: &str, origin: &str| whalerein_core::api::SessionSummary {
            session_id: id.into(),
            title: format!("title-{id}"),
            cwd: "/work".into(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            running: false,
            agent_available: true,
            blank: false,
            parent_session_id: parent.into(),
            origin: origin.into(),
            history_cursor: String::new(),
            source_capabilities: None,
            queued_prompts: 0,
        };
        let list = SessionList {
            sessions: vec![
                session("root-1", "", "cli"),
                session("child-2", "root-1", "fork"),
            ],
            source: "remote".into(),
            complete: true,
        };
        let text = render_sessions(&list);
        assert!(text.contains("SESSION\tPARENT\tORIGIN"), "{text}");
        assert!(text.contains("root-1\t-\tcli"), "{text}");
        assert!(text.contains("child-2\troot-1\tfork"), "{text}");
        // Hostile lineage ids cannot break the row or inject ANSI.
        let hostile = SessionList {
            sessions: vec![session("child-3", "par\tent\u{1b}[31m", "fork\n")],
            source: "remote".into(),
            complete: true,
        };
        let text = render_sessions(&hostile);
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(text.contains("par\\tent"), "{text}");
        assert!(text.contains("fork\\n"), "{text}");
    }

    #[test]
    fn question_and_hostile_title_rendering_is_escaped_and_actionable() {
        let page = QuestionPageInfo {
            batches: vec![],
            next_cursor: String::new(),
        };
        let text = render_questions(&page);
        assert!(text.contains("No pending questions"), "{text}");
        assert!(text.contains("whalerein session question list"));
        let mut page = page;
        page.next_cursor = "cursor-2".into();
        page.batches.push(whalerein_core::api::QuestionBatchInfo {
            session_id: "s-1".into(),
            batch_id: "b-1".into(),
            source_epoch: "epoch".into(),
            version: 2,
            digest: vec![0; 32],
            phase: "QUESTION_OPEN".into(),
            questions: vec![whalerein_core::api::QuestionItemInfo {
                id: "q-1".into(),
                question: "pick\tone\u{1b}[31m".into(),
                detail: None,
                header: None,
                options: vec![],
                multi_select: false,
                intent: None,
            }],
            answerable: true,
            content_unavailable: false,
        });
        let text = render_questions(&page);
        assert!(text.contains("batch b-1 phase=QUESTION_OPEN"), "{text}");
        assert!(text.contains("pick\\tone"), "{text}");
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("--cursor cursor-2"));
        let list = SessionList {
            sessions: vec![],
            source: "dsh".into(),
            complete: true,
        };
        let text = render_sessions(&list);
        assert!(text.contains("No sessions on dsh"));
        assert!(text.contains("session create"));
        assert!(!text.contains("[]"));
    }

    #[test]
    fn tail_human_output_shows_state_transitions_once() {
        let mut renderer = TailRenderer::default();
        let mut state = whalerein_core::conversation::ViewUpdate::State(Default::default());
        if let whalerein_core::conversation::ViewUpdate::State(inner) = &mut state {
            inner.run_phase = whalerein_core::conversation::RunPhase::Running;
            inner.approvals.push(Default::default());
            inner.approvals[0].tool_name = "bash\u{1b}[0m".into();
        }
        let rendered = renderer.render(&state);
        assert!(rendered.contains("[state] running"), "{rendered}");
        assert!(
            rendered.contains("input-required: approval for bash"),
            "{rendered}"
        );
        assert!(!rendered.contains('\u{1b}'));
        assert_eq!(
            renderer.render(&state),
            "",
            "repeated state is not reprinted"
        );
        assert!(
            !is_tail_output(&state),
            "state alone does not satisfy --once"
        );
        let idle = whalerein_core::conversation::ViewUpdate::State(Default::default());
        let mut idle_mut = idle.clone();
        if let whalerein_core::conversation::ViewUpdate::State(inner) = &mut idle_mut {
            inner.run_phase = whalerein_core::conversation::RunPhase::Idle;
        }
        let rendered = renderer.render(&idle_mut);
        assert!(rendered.contains("[state] idle"), "{rendered}");
    }

    #[test]
    fn json_envelopes_never_carry_terminal_control_sequences() {
        let error = CliOutcome::cli_failure("invalid_argument", "bad\tvalue", 2);
        let text = serde_json::to_string(&error.to_json()).unwrap();
        assert!(!text.contains('\u{1b}'));
        let mut outcome = CliOutcome::success(
            &Command::Hosts,
            serde_json::json!({"title": "\u{1b}[31mred"}),
            "",
        );
        outcome.human = sanitize_field("\u{1b}[31mred");
        assert!(!outcome.render().contains('\u{1b}'));
        assert!(!outcome.render().contains("[]"));
    }

    fn model_set_command() -> Command {
        Command::Model {
            action: ModelAction::Set {
                session: "session-1".into(),
                provider: "provider".into(),
                model: "model".into(),
                effort: None,
                host: None,
            },
        }
    }

    #[test]
    fn model_set_failures_explain_whether_a_retry_is_safe() {
        let outcome = core_error_outcome(
            &model_set_command(),
            whalerein_core::CoreError::InvalidInput("provider unavailable".into()),
        );
        assert_eq!(outcome.exit, 2);
        let message = outcome.error.unwrap().message;
        assert!(message.contains("no change was made"), "{message}");
        assert!(message.contains("retry only after"), "{message}");
        assert!(message.contains("whalerein session models"), "{message}");

        let outcome = core_error_outcome(
            &model_set_command(),
            whalerein_core::CoreError::Unavailable("provider offline".into()),
        );
        assert_eq!(outcome.exit, 5);
        let message = outcome.error.unwrap().message;
        assert!(
            message.contains("retry only after the provider recovers"),
            "{message}"
        );

        let outcome = core_error_outcome(
            &model_set_command(),
            whalerein_core::CoreError::UnknownResult,
        );
        assert_eq!(outcome.exit, 7);
        let message = outcome.error.unwrap().message;
        assert!(message.contains("do not resend"), "{message}");
        assert!(message.contains("model get"), "{message}");
    }

    #[test]
    fn a_lost_question_pagination_state_points_at_a_fresh_listing() {
        let command = Command::Questions {
            session: "session-1".into(),
            cursor: "old".into(),
        };
        let outcome = core_error_outcome(
            &command,
            whalerein_core::CoreError::Api {
                code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
                message: "restart".into(),
            },
        );
        assert_eq!(outcome.exit, 8);
        let error = outcome.error.unwrap();
        assert_eq!(error.code, "snapshot_required");
        assert!(
            error.message.contains("without --cursor"),
            "{}",
            error.message
        );
    }
}

#[cfg(test)]
mod block_preview_tests {
    use super::*;
    use whalerein_core::conversation::content_block::{Content, TextContent, ToolCallContent};
    use whalerein_core::conversation::{
        AttemptRef, ContentBlock, ConversationMessage, MessageRole,
    };

    fn attempt() -> AttemptRef {
        AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        }
    }
    fn preview(block: &str, text: &str, replace: bool) -> ViewUpdate {
        ViewUpdate::Preview {
            attempt: attempt(),
            block_id: block.into(),
            text: text.into(),
            replace,
            incomplete: false,
        }
    }

    #[test]
    fn a_block_reset_reprints_only_its_block_and_the_commit_skips_delivered_text() {
        let mut renderer = TailRenderer::default();
        assert_eq!(renderer.render(&preview("a", "one", false)), "one");
        assert_eq!(renderer.render(&preview("b", "two", false)), "\ntwo");
        // The reset replaces block b only; block a is untouched.
        assert_eq!(renderer.render(&preview("b", "TWO", true)), "\nTWO");
        let message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock {
                    block_id: "a".into(),
                    content: Some(Content::Text(TextContent { text: "one".into() })),
                },
                ContentBlock {
                    block_id: "b".into(),
                    content: Some(Content::Text(TextContent { text: "TWO".into() })),
                },
                ContentBlock {
                    block_id: "tool".into(),
                    content: Some(Content::ToolCall(ToolCallContent {
                        call_id: "c".into(),
                        name: "Read".into(),
                        summary: "file".into(),
                    })),
                },
            ],
            ..Default::default()
        };
        let out = renderer.render(&ViewUpdate::Message {
            message,
            origin: Some(attempt()),
            preview_prefix_bytes: Some(6),
        });
        assert!(!out.contains("one"), "delivered text was reprinted: {out}");
        assert!(!out.contains("TWO"), "delivered text was reprinted: {out}");
        assert!(out.contains("[tool] Read file"), "{out}");
    }
}
