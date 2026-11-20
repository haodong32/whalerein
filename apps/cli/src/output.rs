//! Stable CLI output: the `whalerein/client/v1` JSON envelope and human rendering.

use serde_json::{json, Value};
use whalerein_core::api::{
    CommandHandle, DoctorReport, DownloadOutcome, DownloadProgress, ExportManifest, HostInfo,
    ModelCatalog, PairingHistoryRecord, PendingQueue, QuestionPageInfo, SessionKey, SessionList,
    SessionModel,
};

use std::cell::Cell;

use crate::cli::{Command, ModelAction};

use whalerein_core::conversation::{
    content_block, AttemptOutcome, BlockKind, ConversationMessage, NoticeCategory, NoticeState,
    PreviewPrefix, RunPhase, ToolActivity, ToolResultPart, ViewUpdate,
};
use whalerein_core::history::{HistoryPage, HistorySource, MessageReference};

/// Render one committed message's blocks in source order.
///
/// A preview prefix skips bytes the live stream already printed. Core reports
/// answer and reasoning prefixes separately, so hidden reasoning never offsets
/// visible answer text. Reasoning is omitted unless the caller opted in.
fn render_message_blocks(
    message: &ConversationMessage,
    prefix: Option<PreviewPrefix>,
    show_reasoning: bool,
) -> String {
    let mut lines = Vec::new();
    let mut text_consumed = 0usize;
    let mut reasoning_consumed = 0usize;
    let text_skip = prefix.map_or(0, |prefix| prefix.text_bytes);
    let reasoning_skip = prefix.map_or(0, |prefix| prefix.reasoning_bytes);
    for block in &message.blocks {
        match &block.content {
            Some(content_block::Content::Text(text)) => {
                let kept = suffix_after(&text.text, &mut text_consumed, text_skip);
                if !kept.is_empty() {
                    lines.push(kept);
                }
            }
            Some(content_block::Content::Reasoning(reasoning)) => {
                // Count the prefix even when hidden so a later visible message
                // is not offset by reasoning the user never saw.
                let continued = reasoning_consumed < reasoning_skip;
                let kept = suffix_after(&reasoning.text, &mut reasoning_consumed, reasoning_skip);
                if !show_reasoning || kept.is_empty() {
                    continue;
                }
                if continued {
                    // This block is already on screen; append only its suffix
                    // without repeating the typed marker.
                    lines.push(kept);
                } else {
                    lines.push(format!("[reasoning] {kept}"));
                }
            }
            Some(content_block::Content::Notice(notice)) => {
                let label = notice_label(notice.category, notice.state);
                match notice
                    .summary
                    .as_deref()
                    .filter(|summary| !summary.is_empty())
                {
                    Some(summary) => lines.push(format!("{label} {}", sanitize_field(summary))),
                    None => lines.push(label),
                }
            }
            Some(content_block::Content::ToolActivity(activity)) => {
                lines.push(render_tool_activity(activity));
            }
            Some(content_block::Content::Attachment(attachment)) => {
                lines.push(render_attachment(attachment));
            }
            None => continue,
        }
    }
    // Text blocks can carry internal newlines; joining keeps source order and
    // does not insert a blank line between structured blocks.
    lines.join("\n")
}

/// Committed message body for human output under the reasoning policy.
pub fn render_committed_blocks(message: &ConversationMessage, show_reasoning: bool) -> String {
    render_message_blocks(message, None, show_reasoning)
}

fn notice_category_label(category: NoticeCategory) -> &'static str {
    match category {
        NoticeCategory::Unspecified => "unspecified",
        NoticeCategory::Retry => "retry",
        NoticeCategory::MaxOutput => "max-output",
        NoticeCategory::Compaction => "compaction",
        NoticeCategory::Command => "command",
        NoticeCategory::Context => "context",
        NoticeCategory::Omission => "omission",
    }
}

fn notice_state_label(state: NoticeState) -> &'static str {
    match state {
        NoticeState::Unspecified => "unspecified",
        NoticeState::Scheduled => "scheduled",
        NoticeState::Started => "started",
        NoticeState::Cancelled => "cancelled",
        NoticeState::Completed => "completed",
        NoticeState::Failed => "failed",
        NoticeState::Reached => "reached",
    }
}

fn notice_label(category: NoticeCategory, state: NoticeState) -> String {
    format!(
        "[notice {}/{}]",
        notice_category_label(category),
        notice_state_label(state)
    )
}

fn attachment_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}

fn attachment_display_name(attachment: &content_block::AttachmentContent) -> String {
    if !attachment.name.trim().is_empty() {
        return attachment.name.clone();
    }
    format!(
        "attachment.{}",
        attachment_extension(&attachment.media_type)
    )
}

fn render_attachment(attachment: &content_block::AttachmentContent) -> String {
    format!(
        "[attachment {} ({}, {} bytes{})]",
        sanitize_field(&attachment_display_name(attachment)),
        sanitize_field(&attachment.media_type),
        attachment.size,
        if attachment.available {
            ""
        } else {
            ", unavailable"
        }
    )
}

thread_local! {
    static FULL_TOOL_OUTPUT: Cell<bool> = const { Cell::new(false) };
}

/// Set by the CLI before dispatch; human tool-result rendering only.
pub fn set_full_tool_output(full: bool) {
    FULL_TOOL_OUTPUT.with(|value| value.set(full));
}

fn full_tool_output() -> bool {
    FULL_TOOL_OUTPUT.with(Cell::get)
}

/// Human display keeps a tool result bounded. Machine output stays complete.
const TOOL_RESULT_DISPLAY_BYTES: usize = 8 * 1024;

fn bounded_tool_text(text: &str) -> String {
    if full_tool_output() || text.len() <= TOOL_RESULT_DISPLAY_BYTES {
        return text.to_owned();
    }
    let mut end = 0;
    for (index, _) in text.char_indices() {
        if index > TOOL_RESULT_DISPLAY_BYTES {
            break;
        }
        end = index;
    }
    let hidden = text.len().saturating_sub(end);
    format!(
        "{}\n… [{} bytes hidden; use --full to show complete tool output]",
        &text[..end],
        hidden
    )
}

/// One typed tool activity, including the evidenced state and settled result.
fn render_tool_activity(activity: &ToolActivity) -> String {
    let mut header = format!(
        "[tool {} {}]",
        sanitize_field(&activity.name),
        activity.state.as_str()
    );
    if let Some(summary) = activity
        .summary
        .as_deref()
        .filter(|summary| !summary.is_empty())
    {
        header.push(' ');
        header.push_str(&sanitize_field(summary));
    }
    if activity.call_missing {
        header.push_str(" (call record outside the read window)");
    }
    let mut out = header;
    if let Some(progress) = activity
        .progress
        .as_deref()
        .filter(|progress| !progress.is_empty())
    {
        out.push_str(&format!("\n[tool progress] {}", sanitize_field(progress)));
    }
    if let Some(code) = activity
        .failure_code
        .as_deref()
        .filter(|code| !code.is_empty())
    {
        out.push_str(&format!("\n[tool failure] {}", sanitize_field(code)));
    }
    for part in &activity.result {
        match part {
            ToolResultPart::Text(text) if !text.is_empty() => {
                out.push_str(&format!(
                    "\n[tool result] {}",
                    sanitize_field(&bounded_tool_text(text))
                ));
            }
            ToolResultPart::Text(_) => {}
            ToolResultPart::Attachment(attachment) => {
                out.push('\n');
                out.push_str(&render_attachment(attachment));
            }
        }
    }
    out
}

fn tool_result_json(part: &ToolResultPart) -> Value {
    match part {
        ToolResultPart::Text(text) => json!({"type": "text", "text": text}),
        ToolResultPart::Attachment(attachment) => attachment_json(attachment),
    }
}

fn tool_activity_json(activity: &ToolActivity) -> Value {
    let stages: Vec<&str> = activity
        .stages
        .iter()
        .map(|stage| stage.as_str_name())
        .collect();
    json!({
        "call_id": activity.id.call_id,
        "name": activity.name,
        "summary": activity.summary,
        "state": activity.state.as_str(),
        "stages": stages,
        "progress": activity.progress,
        "failure_code": activity.failure_code,
        "result": activity.result.iter().map(tool_result_json).collect::<Vec<_>>(),
        "call_missing": activity.call_missing,
    })
}

fn attachment_json(attachment: &content_block::AttachmentContent) -> Value {
    json!({
        "name": attachment.name,
        "media_type": attachment.media_type,
        "size": attachment.size,
        "available": attachment.available,
        "reference": attachment.attachment_ref,
    })
}

fn content_block_json(
    block: &whalerein_core::conversation::ContentBlock,
    show_reasoning: bool,
) -> Option<Value> {
    Some(match &block.content {
        Some(content_block::Content::Text(text)) => {
            json!({"block_id": block.block_id, "type": "text", "text": text.text})
        }
        Some(content_block::Content::Reasoning(reasoning)) if show_reasoning => json!({
            "block_id": block.block_id,
            "type": "reasoning",
            "text": reasoning.text,
        }),
        // Hidden reasoning is omitted entirely so default JSON cannot leak it
        // or even its position.
        Some(content_block::Content::Reasoning(_)) => return None,
        Some(content_block::Content::ToolActivity(activity)) => {
            let mut value = tool_activity_json(activity);
            value["block_id"] = json!(block.block_id);
            value["type"] = json!("tool_activity");
            value
        }
        Some(content_block::Content::Notice(notice)) => json!({
            "block_id": block.block_id,
            "type": "notice",
            "category": notice.category.as_str_name(),
            "state": notice.state.as_str_name(),
            "summary": notice.summary,
        }),
        Some(content_block::Content::Attachment(attachment)) => {
            let mut value = attachment_json(attachment);
            value["block_id"] = json!(block.block_id);
            value["type"] = json!("attachment");
            value
        }
        None => json!({"block_id": block.block_id, "type": "unavailable"}),
    })
}

/// Core-owned message value for JSON output. Reasoning is present only when
/// the caller opted in.
pub fn message_json(message: &ConversationMessage, show_reasoning: bool) -> Value {
    json!({
        "message_id": message.message_id,
        "role": message.role.as_str_name(),
        "authored_at_unix_ms": message.authored_at_unix_ms,
        "blocks": message
            .blocks
            .iter()
            .filter_map(|block| content_block_json(block, show_reasoning))
            .collect::<Vec<_>>(),
    })
}

/// Shell-safe quoting for values a user will paste into a supported shell.
///
/// Single quotes preserve every byte; an embedded quote is closed, escaped and
/// reopened. Control characters and command substitutions therefore stay
/// literal arguments instead of becoming shell syntax.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn history_source_label(source: HistorySource) -> &'static str {
    match source {
        HistorySource::Online => "online",
        HistorySource::Cache => "cache",
    }
}

fn attachment_save_command(key: &SessionKey, reference: &str, name: Option<&str>) -> String {
    let name = name
        .map(|name| format!(" --name {}", shell_quote(name)))
        .unwrap_or_default();
    format!(
        "whalerein session download {} --host {} --session {} --to <DIR>{name}",
        shell_quote(reference),
        shell_quote(&key.host_id),
        shell_quote(&key.session_id)
    )
}

/// Duplicate display names would make two save commands collide in one
/// directory. The first keeps the Source suggestion; later ones get a stable
/// media-derived name so each printed command is directly usable.
fn attachment_name_overrides(
    attachments: &[content_block::AttachmentContent],
) -> Vec<Option<String>> {
    let mut counts = std::collections::HashMap::new();
    let mut overrides = Vec::with_capacity(attachments.len());
    let mut position = 0usize;
    for attachment in attachments {
        if !attachment.available || attachment.attachment_ref.is_empty() {
            overrides.push(None);
            continue;
        }
        let display = attachment_display_name(attachment);
        let count = counts.entry(display).or_insert(0usize);
        *count += 1;
        position += 1;
        if *count == 1 {
            overrides.push(None);
        } else {
            overrides.push(Some(format!(
                "attachment-{position}.{}",
                attachment_extension(&attachment.media_type)
            )));
        }
    }
    overrides
}

/// One verified attachment save as machine data.
///
/// `target` is added by the command runner when it is not already present. The
/// opaque reference is echoed so a caller can correlate the save with the
/// history attachment row.
pub fn download_data(key: &SessionKey, reference: &str, outcome: &DownloadOutcome) -> Value {
    json!({
        "target": key,
        "reference": reference,
        "path": outcome.path.to_string_lossy(),
        "name": outcome.name,
        "bytes": outcome.bytes,
        "sha256": outcome.sha256,
        "media_type": outcome.media_type,
        "verified": outcome.verified,
    })
}

/// Human success for one committed save. It is rendered only after Core
/// returned a verified outcome, never while a transfer is still running.
pub fn render_download(outcome: &DownloadOutcome) -> String {
    format!(
        "Saved: {}\nBytes: {}\nMedia type: {}\nSHA-256: {}\nVerified: {}\n",
        outcome.path.to_string_lossy(),
        outcome.bytes,
        sanitize_field(&outcome.media_type),
        outcome.sha256,
        if outcome.verified { "yes" } else { "no" }
    )
}

/// Throttled progress presentation for the stderr stream.
///
/// Emits the declared total once at the start, then at most about ten updates
/// plus the final declaration. It never carries a local path, filename or
/// attachment content. `--json` yields one structured event per line; human
/// mode yields one line with byte counts and a percentage.
#[derive(Debug)]
pub struct DownloadProgressRenderer {
    json: bool,
    started: bool,
    last_received: u64,
    last_total: u64,
    next_received: u64,
}

impl DownloadProgressRenderer {
    pub fn new(json: bool) -> Self {
        Self {
            json,
            started: false,
            last_received: 0,
            last_total: 0,
            next_received: 0,
        }
    }

    /// Return the next bounded progress line, or `None` when this update is
    /// below the throttle threshold or repeats the previous event.
    pub fn report(&mut self, progress: DownloadProgress) -> Option<String> {
        let total = progress.total;
        let step = progress_step(total);
        let repeated =
            self.started && progress.received == self.last_received && total == self.last_total;
        let final_declaration =
            total > 0 && progress.received >= total && self.last_received < total;
        if repeated
            || (self.started && progress.received < self.next_received && !final_declaration)
        {
            return None;
        }
        if !self.started {
            self.started = true;
            self.next_received = step;
        } else {
            self.next_received = progress.received.saturating_add(step).max(step);
        }
        self.last_received = progress.received;
        self.last_total = total;
        Some(self.render(progress))
    }

    fn render(&self, progress: DownloadProgress) -> String {
        if self.json {
            let mut line = json!({
                "event": "download_progress",
                "received": progress.received,
                "total": progress.total,
            })
            .to_string();
            line.push('\n');
            line
        } else if progress.total == 0 {
            "Progress: 0/0 bytes (complete)\n".to_owned()
        } else {
            let percent = progress
                .received
                .saturating_mul(100)
                .checked_div(progress.total)
                .unwrap_or(100);
            format!(
                "Progress: {}/{} bytes ({}%)\n",
                progress.received, progress.total, percent
            )
        }
    }
}

fn progress_step(total: u64) -> u64 {
    const MIN_STEP: u64 = 1024 * 1024;
    (total / 10).max(MIN_STEP).max(1)
}

fn attachment_rows(messages: &[ConversationMessage]) -> Vec<content_block::AttachmentContent> {
    let mut attachments = Vec::new();
    for message in messages {
        for block in &message.blocks {
            match &block.content {
                Some(content_block::Content::Attachment(attachment)) => {
                    attachments.push(attachment.clone());
                }
                Some(content_block::Content::ToolActivity(activity)) => {
                    for part in &activity.result {
                        if let ToolResultPart::Attachment(attachment) = part {
                            attachments.push(attachment.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    attachments
}

fn attachments_json(
    key: &SessionKey,
    attachments: &[content_block::AttachmentContent],
) -> Vec<Value> {
    let overrides = attachment_name_overrides(attachments);
    attachments
        .iter()
        .zip(overrides)
        .map(|(attachment, override_name)| {
            let mut value = attachment_json(attachment);
            value["save_command"] = if attachment.available && !attachment.attachment_ref.is_empty()
            {
                json!(attachment_save_command(
                    key,
                    &attachment.attachment_ref,
                    override_name.as_deref()
                ))
            } else {
                Value::Null
            };
            value
        })
        .collect()
}

fn render_attachment_rows(
    key: &SessionKey,
    attachments: &[content_block::AttachmentContent],
) -> String {
    let mut out = String::new();
    let overrides = attachment_name_overrides(attachments);
    for (attachment, override_name) in attachments.iter().zip(overrides) {
        out.push_str(&format!(
            "Attachment: {} ({}, {} bytes{})\n",
            sanitize_field(&attachment_display_name(attachment)),
            sanitize_field(&attachment.media_type),
            attachment.size,
            if attachment.available {
                ""
            } else {
                ", unavailable"
            }
        ));
        if attachment.available && !attachment.attachment_ref.is_empty() {
            out.push_str(&format!(
                "  Save: {}\n",
                attachment_save_command(key, &attachment.attachment_ref, override_name.as_deref())
            ));
            if attachment.media_type.starts_with("image/") && attachment.size > 32 * 1024 * 1024 {
                out.push_str(&format!(
                    "  Large image: decoding may use about {} MiB of memory.\n",
                    attachment.size.div_ceil(1024 * 1024)
                ));
            }
        } else {
            out.push_str(
                "  The Source did not expose readable bytes for this occurrence; no save action is available.\n",
            );
        }
    }
    out
}

fn history_continue_command(key: &SessionKey, continuation: &str) -> String {
    format!(
        "whalerein session history {} --host {} --continuation {}",
        shell_quote(&key.session_id),
        shell_quote(&key.host_id),
        shell_quote(continuation)
    )
}

/// Human output for one independent history page. Messages are chronological
/// within the page; a continuation points toward older history.
pub fn render_history_page(key: &SessionKey, page: &HistoryPage, show_reasoning: bool) -> String {
    let mut out = format!(
        "Source: {}\nProjection: {}\nSession: {} / {}\n",
        history_source_label(page.source),
        sanitize_field(&page.projection),
        sanitize_field(&key.host_id),
        sanitize_field(&key.session_id)
    );
    if page.messages.is_empty() {
        out.push_str(if page.source == HistorySource::Cache {
            "No committed messages in the local cache.\n"
        } else {
            "No committed messages in this page.\n"
        });
    }
    for message in &page.messages {
        let body = render_message_blocks(message, None, show_reasoning);
        if body.is_empty() {
            continue;
        }
        out.push_str(&format!("[{}] {body}\n", message.role.as_str_name()));
    }
    out.push_str(&render_attachment_rows(
        key,
        &attachment_rows(&page.messages),
    ));
    match &page.continuation {
        Some(continuation) if !page.complete => {
            let token = continuation.to_string();
            out.push_str(&format!(
                "Continuation: {token}\nContinue older history:\n  {}\n",
                history_continue_command(key, &token)
            ));
        }
        _ => out.push_str("End of history: the oldest message in this cut was reached.\n"),
    }
    out
}

/// JSON data for one independent history page.
pub fn history_page_data(key: &SessionKey, page: &HistoryPage, show_reasoning: bool) -> Value {
    let attachments = attachment_rows(&page.messages);
    json!({
        "target": key,
        "source": history_source_label(page.source),
        "projection": page.projection,
        "complete": page.complete,
        "messages": page
            .messages
            .iter()
            .map(|message| message_json(message, show_reasoning))
            .collect::<Vec<_>>(),
        "activities": page.activities.iter().map(tool_activity_json).collect::<Vec<_>>(),
        "attachments": attachments_json(key, &attachments),
        "continuation": page.continuation.as_ref().map(ToString::to_string),
    })
}

/// Human output for one exact committed-message read.
pub fn render_history_message(
    key: &SessionKey,
    reference: &MessageReference,
    message: &ConversationMessage,
    show_reasoning: bool,
) -> String {
    let mut out = format!(
        "Source: online\nProjection: {}\nMessage: {}\n",
        sanitize_field(reference.projection_id()),
        sanitize_field(&message.message_id)
    );
    let body = render_message_blocks(message, None, show_reasoning);
    if !body.is_empty() {
        out.push_str(&format!("[{}] {body}\n", message.role.as_str_name()));
    }
    out.push_str(&render_attachment_rows(
        key,
        &attachment_rows(std::slice::from_ref(message)),
    ));
    out.push_str(&format!("Reference: {}\n", reference.to_token()));
    out
}

/// JSON data for one exact committed-message read.
pub fn history_message_data(
    key: &SessionKey,
    reference: &MessageReference,
    message: &ConversationMessage,
    show_reasoning: bool,
) -> Value {
    let attachments = attachment_rows(std::slice::from_ref(message));
    json!({
        "target": key,
        "source": "online",
        "projection": reference.projection_id(),
        "message": message_json(message, show_reasoning),
        "attachments": attachments_json(key, &attachments),
        "reference": reference.to_token(),
    })
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
        match &self.error {
            Some(error) => {
                let mut value = json!({
                    "schema": "whalerein/client/v1",
                    "command": self.command,
                    "error": {
                        "code": error.code,
                        "message": error.message,
                        "action": error.action,
                    }
                });
                if let Some(data) = &self.data {
                    value["data"] = data.clone();
                }
                if let Some(id) = &error.diagnostic_id {
                    value["error"]["diagnostic_id"] = json!(id);
                }
                if let Some(layer) = &error.layer {
                    value["error"]["layer"] = json!(layer);
                }
                value
            }
            None => json!({
                "schema": "whalerein/client/v1",
                "command": self.command,
                "data": self.data,
            }),
        }
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
        format!(
            "{}\n",
            serde_json::to_string_pretty(&self.data).unwrap_or_default()
        )
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
    // Attachment failures name the preserved destination state and the exact
    // recovery step. Read the diagnostic detail before the typed error moves.
    let detail = error.to_string();
    // Guidance follows the shared action contract. `upgrade` is terminal: the
    // message names the side to upgrade and never suggests retrying the same
    // request.
    let message = match (code.as_str(), command) {
        (_, Command::SessionDownload { session, .. }) => {
            download_failure_message(&detail, &code, session)
        }
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
        (
            "snapshot_required" | "invalid_argument",
            Command::History {
                session,
                cache: true,
                continuation: Some(_),
                ..
            },
        ) => format!(
            "{}; the history continuation is stale, expired, or belongs to another session — restart traversal with `whalerein session history {} --cache` (without --continuation)",
            error,
            shell_quote(session)
        ),
        (
            "snapshot_required" | "invalid_argument",
            Command::History {
                session,
                continuation: Some(_),
                ..
            },
        ) => format!(
            "{}; the history continuation is stale, expired, or belongs to another session — restart traversal with `whalerein session history {}` (without --continuation)",
            error,
            shell_quote(session)
        ),
        (
            "snapshot_required" | "invalid_argument" | "not_found",
            Command::History {
                session,
                message: Some(_),
                ..
            },
        ) => format!(
            "{}; the committed message reference is stale or unavailable — refresh history with `whalerein session history {}` and retry the exact read. Do not resend the message",
            error,
            shell_quote(session)
        ),
        ("snapshot_required", Command::History { session, .. }) => format!(
            "{}; the content projection changed — re-run `whalerein session history {}` to start a new traversal",
            error,
            shell_quote(session)
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
    CliOutcome {
        command: command.name().to_owned(),
        data: None,
        error: Some(CliError {
            code,
            message,
            action: error.action().to_owned(),
            diagnostic_id: None,
            layer: Some(error.source_layer().to_owned()),
        }),
        human: String::new(),
        exit,
    }
}

/// Actionable guidance for one failed attachment download.
///
/// Every branch states whether a file was written. Retry language means a new
/// transfer from offset zero; integrity and interruption failures never resume
/// a partial transfer. The history hint re-reads the attachment row that owns
/// the opaque reference.
fn download_failure_message(detail: &str, code: &str, session: &str) -> String {
    let history = format!("`whalerein session history {}`", shell_quote(session));
    let name_rejected = detail.contains("file name") || detail.contains("filename");
    match code {
        "unsupported_capability" => format!(
            "{detail}; this Host cannot read attachments. No bytes were read and no file was written. Upgrade the Host plugin or client (do not retry this request). Inspect the attachment row with {history}."
        ),
        "protocol_version_incompatible" => format!(
            "{detail}; the CLI and Host plugin do not share a supported protocol version, so no file was written. Upgrade both sides (do not retry this request). Inspect the attachment row with {history}."
        ),
        "unauthorized" | "forbidden" => format!(
            "{detail}; the Host rejected this attachment read and no file was written. Verify Device access with `whalerein status`, then inspect the attachment row with {history}."
        ),
        "payload_too_large" => format!(
            "{detail}; the attachment exceeds the effective download limit before transfer and no file was written. Raise the configured limit or choose another attachment. Inspect {history}."
        ),
        "rate_limited" => format!(
            "{detail}; the download concurrency or reservation budget is exhausted and no file was written. Wait for active downloads to finish, then run the same download command again for a fresh transfer. Inspect {history}."
        ),
        "timeout" => format!(
            "{detail}; the download deadline expired and no completed file was saved. Temporary data was removed. Run the same download command again to start a fresh transfer. Inspect {history}."
        ),
        "cancelled" => format!(
            "{detail}; no completed file was saved and temporary data was removed. Run the same download command again to start a fresh transfer."
        ),
        "payload_digest_mismatch" => format!(
            "{detail}; integrity verification failed and no completed file was saved. Temporary data was removed. Run the same download command again for a fresh transfer; if it fails again the stored attachment may be corrupt. Inspect {history}."
        ),
        "conflict" => format!(
            "{detail}; an existing file was preserved and nothing was replaced. Re-run with `--force` to replace it explicitly, or choose another `--to` directory."
        ),
        "invalid_argument" if name_rejected => format!(
            "{detail}; the Source-suggested name is not safe for a local file and no file was written. Pass an explicit safe name with `--name <NAME>` and retry the download. Inspect {history}."
        ),
        "invalid_argument" if detail.contains("destination") => format!(
            "{detail}; choose an existing destination directory with `--to <DIR>`. No file was written. Inspect {history}."
        ),
        "invalid_argument" => format!(
            "{detail}; verify the attachment reference, Host and session from the history save action, then retry. No file was written. Inspect {history}."
        ),
        "not_found" => format!(
            "{detail}; the attachment occurrence is no longer available. Refresh the attachment row with {history} and use its current save action."
        ),
        "internal" if detail.contains("free space") => format!(
            "{detail}; the destination has no free space. No completed file was saved and temporary data was removed. Free space, then run the same download command again for a fresh transfer."
        ),
        "internal" if detail.contains("not writable") => format!(
            "{detail}; the destination is not writable. Choose a writable directory with `--to <DIR>`; no completed file was saved and temporary data was removed."
        ),
        "security_error" => format!(
            "{detail}; the attachment scope did not match this Host or session and no file was written. Refresh the attachment row with {history} and retry its save action."
        ),
        _ => format!(
            "{detail}; no completed file was saved and temporary data was removed. Inspect the attachment row with {history} and retry the same download command for a fresh transfer."
        ),
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
            ("known", _) if host.presence == "unavailable" => "status unavailable",
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
///
/// A hidden reasoning preview returns `None` so default JSON cannot carry it.
pub fn tail_event_json(update: &ViewUpdate, show_reasoning: bool) -> Option<Value> {
    let event = match update {
        ViewUpdate::Preview {
            attempt,
            block_id,
            kind,
            text,
            replace,
            incomplete,
        } => {
            if *kind == BlockKind::Reasoning && !show_reasoning {
                return None;
            }
            json!({"type":"preview","attempt_id":attempt.attempt_id,"live_epoch":attempt.live_epoch,"block_id":block_id,"kind":kind.as_str_name(),"text":text,"replace":replace,"incomplete":incomplete})
        }
        ViewUpdate::Message {
            message,
            origin,
            preview_prefix,
        } => {
            let blocks: Vec<Value> = message
                .blocks
                .iter()
                .filter_map(|block| content_block_json(block, show_reasoning))
                .collect();
            json!({
                "type": "message",
                "message_id": message.message_id,
                "role": message.role.as_str_name(),
                "blocks": blocks,
                "authored_at_unix_ms": message.authored_at_unix_ms,
                "origin": origin.as_ref().map(|attempt| json!({
                    "live_epoch": attempt.live_epoch,
                    "attempt_id": attempt.attempt_id,
                })),
                "preview_prefix": preview_prefix.map(|prefix| json!({
                    "text_bytes": prefix.text_bytes,
                    "reasoning_bytes": prefix.reasoning_bytes,
                })),
            })
        }
        ViewUpdate::ToolActivity(activity) => {
            let mut value = tool_activity_json(activity);
            value["type"] = json!("tool_activity");
            value
        }
        ViewUpdate::AttemptEnded { attempt, outcome } => {
            json!({"type":"attempt_ended","attempt_id":attempt.attempt_id,"live_epoch":attempt.live_epoch,"outcome":outcome.as_str_name()})
        }
        ViewUpdate::State(state) => {
            let approvals: Vec<Value> = state.approvals.iter().map(|approval| json!({"approval_id":approval.approval_id,"session_id":approval.session_id,"tool_name":approval.tool_name,"call_id":approval.call_id,"reason":approval.reason,"digest":hex::encode(&approval.digest),"version":approval.version,"expires_at_unix_ms":approval.expires_at_unix_ms,"state":approval.state.as_str_name()})).collect();
            json!({"type":"state","run_phase":state.run_phase.as_str_name(),"projection_id":state.projection_id,"approvals":approvals})
        }
        ViewUpdate::Reset => json!({"type":"reset"}),
    };
    Some(json!({"schema":"whalerein/client/v1","command":"session.watch","data":{"event":event}}))
}

/// Formatting owns terminal boundaries only; Core owns preview ordering and completion.
#[derive(Default)]
pub struct TailRenderer {
    line_open: bool,
    attempt: Option<String>,
    block: Option<String>,
    kind: Option<BlockKind>,
    last_state: Option<String>,
    show_reasoning: bool,
}
impl TailRenderer {
    pub fn new(show_reasoning: bool) -> Self {
        Self {
            show_reasoning,
            ..Self::default()
        }
    }

    pub fn render(&mut self, update: &ViewUpdate) -> String {
        match update {
            ViewUpdate::Preview {
                attempt,
                block_id,
                kind,
                text,
                replace,
                ..
            } => {
                if *kind == BlockKind::Reasoning && !self.show_reasoning {
                    // Hidden reasoning is presentation-only: it must not open a
                    // line, terminate a previous one, or complete `--once`.
                    return String::new();
                }
                // Each preview block is its own line. A block-level reset
                // closes and reprints only that block, never the message.
                let new_block = self.block.as_deref() != Some(block_id) || self.kind != Some(*kind);
                let prefix = if self.attempt.as_deref() != Some(&attempt.attempt_id)
                    || *replace
                    || new_block
                {
                    self.finish_line()
                } else {
                    String::new()
                };
                let marker = if *kind == BlockKind::Reasoning && (new_block || *replace) {
                    "[reasoning] "
                } else {
                    ""
                };
                self.attempt = Some(attempt.attempt_id.clone());
                self.block = Some(block_id.clone());
                self.kind = Some(*kind);
                self.line_open |= !text.is_empty();
                format!("{prefix}{marker}{text}")
            }
            ViewUpdate::Message {
                message,
                preview_prefix,
                ..
            } => {
                if let Some(prefix) = preview_prefix {
                    let visible_prefix = prefix.text_bytes > 0
                        || (self.show_reasoning && prefix.reasoning_bytes > 0);
                    if visible_prefix {
                        // The previewed prefix is already on screen. Emit only
                        // the suffix on the same line, then terminate it.
                        self.line_open = false;
                        self.attempt = None;
                        self.block = None;
                        self.kind = None;
                        let body =
                            render_message_blocks(message, Some(*prefix), self.show_reasoning);
                        if body.is_empty() {
                            return "\n".into();
                        }
                        return format!("{body}\n");
                    }
                    // The commit did not continue any visible preview (for
                    // example a hidden reasoning prefix). Close the provisional
                    // line and render the message as new visible output.
                    let closed = self.finish_line();
                    self.line_open = false;
                    self.attempt = None;
                    self.block = None;
                    self.kind = None;
                    let body = render_message_blocks(message, Some(*prefix), self.show_reasoning);
                    if body.is_empty() {
                        return if closed.is_empty() {
                            "\n".into()
                        } else {
                            closed
                        };
                    }
                    return format!("{closed}[{}] {body}\n", message.role.as_str_name());
                }
                let prefix = self.finish_line();
                self.attempt = None;
                self.block = None;
                self.kind = None;
                let body = render_message_blocks(message, None, self.show_reasoning);
                let role = message.role.as_str_name();
                if body.is_empty() {
                    format!("{prefix}[{role}]\n")
                } else {
                    format!("{prefix}[{role}] {body}\n")
                }
            }
            ViewUpdate::ToolActivity(activity) => {
                let prefix = self.finish_line();
                format!("{prefix}{}\n", render_tool_activity(activity))
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
                self.kind = None;
                self.finish_line()
            }
            ViewUpdate::AttemptEnded { outcome, .. } if *outcome != AttemptOutcome::Committed => {
                self.attempt = None;
                self.block = None;
                self.kind = None;
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

/// True when an update carries user-visible content under the presentation
/// policy. State/reset metadata and hidden reasoning never finish `--once`.
pub fn is_tail_output(update: &ViewUpdate, show_reasoning: bool) -> bool {
    match update {
        ViewUpdate::Message { message, .. } => {
            message.blocks.iter().any(|block| match &block.content {
                Some(content_block::Content::Text(text)) => !text.text.is_empty(),
                Some(content_block::Content::Reasoning(reasoning)) => {
                    show_reasoning && !reasoning.text.is_empty()
                }
                Some(content_block::Content::ToolActivity(_))
                | Some(content_block::Content::Attachment(_))
                | Some(content_block::Content::Notice(_)) => true,
                None => false,
            })
        }
        ViewUpdate::Preview { kind, text, .. } => {
            !text.is_empty() && (*kind != BlockKind::Reasoning || show_reasoning)
        }
        // An interrupted/abandoned turn is the only output of that attempt:
        // `tail --once` must surface it instead of timing out silently.
        ViewUpdate::AttemptEnded { outcome, .. } => *outcome != AttemptOutcome::Committed,
        _ => false,
    }
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
        let outcome = CliOutcome::local_failure(&command(), "unauthorized", "no", 4);
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/client/v1");
        assert_eq!(value["error"]["code"], "unauthorized");
        assert_eq!(value["error"]["message"], "no");
        assert_eq!(outcome.exit, 4);
        assert_eq!(outcome.render(), "error: no (unauthorized)\n");
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
            {"hostId":"host-id-6", "displayName":"Other", "online":false, "enrolled":true, "observation":"known", "enrollmentState":"enrollment_ready", "presence":"offline"},
            {"hostId":"host-id-7", "displayName":"Other", "online":false, "enrolled":true, "observation":"known", "enrollmentState":"enrollment_ready", "presence":"unavailable"}
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
                "host-id-7\tOther\tstatus unavailable\tunavailable\n",
            )
        );
        assert_eq!(json!(cloud)[0]["host_id"], "host-id-1");
        assert!(render_account_hosts(&[]).starts_with("No computers"));
    }

    #[test]
    fn explicit_unavailable_presence_stays_distinct_from_known_offline_and_old_cloud() {
        let explicit: Vec<whalerein_core::api::AccountHost> = serde_json::from_value(json!([
            {"hostId":"host-unavailable", "displayName":"Computer", "enrolled":true, "online":false,
             "observation":"known", "enrollmentState":"enrollment_ready", "presence":"unavailable"},
            {"hostId":"host-offline", "displayName":"Computer", "enrolled":true, "online":false,
             "observation":"known", "enrollmentState":"enrollment_ready", "presence":"offline"}
        ]))
        .unwrap();
        let unavailable = serde_json::to_value(&explicit[0]).unwrap();
        let offline = serde_json::to_value(&explicit[1]).unwrap();
        assert_eq!(unavailable["presence"], "unavailable");
        assert_eq!(offline["presence"], "offline");
        assert_ne!(unavailable["presence"], offline["presence"]);
        let human = render_account_hosts(&explicit);
        assert!(
            human.contains("host-unavailable\tComputer\tstatus unavailable"),
            "{human}"
        );
        assert!(human.contains("host-offline\tComputer\toffline"), "{human}");

        // An older Cloud omits presence. Absence is not an explicit
        // "unavailable": a known online/offline observation still renders.
        let legacy: Vec<whalerein_core::api::AccountHost> = serde_json::from_value(json!([
            {"hostId":"host-legacy-online", "displayName":"Computer", "enrolled":true, "online":true,
             "observation":"known", "enrollmentState":"enrollment_ready"},
            {"hostId":"host-legacy-offline", "displayName":"Computer", "enrolled":true, "online":false,
             "observation":"known", "enrollmentState":"enrollment_ready"}
        ])).unwrap();
        assert_eq!(legacy[0].presence, "");
        let legacy_value = serde_json::to_value(&legacy[0]).unwrap();
        assert!(
            legacy_value.get("presence").is_none(),
            "an omitted older-Cloud field stays omitted"
        );
        let legacy_human = render_account_hosts(&legacy);
        assert!(
            legacy_human.contains("host-legacy-online\tComputer\tonline"),
            "{legacy_human}"
        );
        assert!(
            legacy_human.contains("host-legacy-offline\tComputer\toffline"),
            "{legacy_human}"
        );
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
    fn text_preview(text: &str, replace: bool) -> ViewUpdate {
        ViewUpdate::Preview {
            attempt: attempt(),
            block_id: "text".into(),
            kind: BlockKind::Text,
            text: text.into(),
            replace,
            incomplete: false,
        }
    }
    fn reasoning_preview(text: &str) -> ViewUpdate {
        ViewUpdate::Preview {
            attempt: attempt(),
            block_id: "reasoning".into(),
            kind: BlockKind::Reasoning,
            text: text.into(),
            replace: false,
            incomplete: false,
        }
    }
    fn reasoning_message() -> ConversationMessage {
        use whalerein_core::conversation::content_block::{Content, ReasoningContent, TextContent};
        use whalerein_core::conversation::ContentBlock;
        ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock {
                    block_id: "reasoning".into(),
                    content: Some(Content::Reasoning(ReasoningContent {
                        text: "private chain".into(),
                    })),
                },
                ContentBlock {
                    block_id: "text".into(),
                    content: Some(Content::Text(TextContent {
                        text: "answer".into(),
                    })),
                },
            ],
            authored_at_unix_ms: 1,
        }
    }

    #[test]
    fn human_output_uses_core_prefix_decisions_and_closes_provisional_lines() {
        let mut renderer = TailRenderer::default();
        let preview = text_preview("x", false);
        assert_eq!(renderer.render(&preview).len(), 1);
        assert!(is_tail_output(&preview, false));
        assert_eq!(
            renderer.render(&ViewUpdate::Message {
                message: message(),
                origin: Some(attempt()),
                preview_prefix: Some(PreviewPrefix::default())
            }),
            "\n"
        );
        assert!(renderer.finish_line().is_empty());
        renderer.render(&preview);
        assert_eq!(renderer.render(&ViewUpdate::Reset), "\n");
        assert!(!is_tail_output(&ViewUpdate::Reset, false));
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
                preview_prefix: None
            })
            .contains("ASSISTANT"));
    }

    #[test]
    fn reasoning_is_hidden_by_default_and_shown_only_on_request() {
        let mut hidden = TailRenderer::default();
        assert_eq!(hidden.render(&reasoning_preview("private")), "");
        assert!(!is_tail_output(&reasoning_preview("private"), false));
        assert!(is_tail_output(&reasoning_preview("private"), true));
        let mut shown = TailRenderer::new(true);
        assert_eq!(
            shown.render(&reasoning_preview("private")),
            "[reasoning] private"
        );
        assert_eq!(shown.render(&text_preview("answer", false)), "\nanswer");
        // Hidden reasoning must not terminate or join a visible line.
        let mut hidden = TailRenderer::default();
        assert_eq!(hidden.render(&text_preview("one", false)), "one");
        assert_eq!(hidden.render(&reasoning_preview("secret")), "");
        assert_eq!(hidden.render(&text_preview("two", false)), "two");
    }

    #[test]
    fn hidden_reasoning_does_not_leak_into_default_json() {
        let mut hidden = TailRenderer::default();
        assert!(tail_event_json(&reasoning_preview("secret"), false).is_none());
        assert!(hidden.render(&reasoning_preview("secret")).is_empty());
        let value = tail_event_json(&reasoning_preview("secret"), true).unwrap();
        assert_eq!(value["data"]["event"]["kind"], "REASONING");
        assert_eq!(value["data"]["event"]["text"], "secret");

        let hidden_message = tail_event_json(
            &ViewUpdate::Message {
                message: reasoning_message(),
                origin: None,
                preview_prefix: None,
            },
            false,
        )
        .unwrap();
        let text = serde_json::to_string(&hidden_message).unwrap();
        assert!(!text.contains("private chain"), "{text}");
        assert!(text.contains("answer"), "{text}");
        let shown_message = tail_event_json(
            &ViewUpdate::Message {
                message: reasoning_message(),
                origin: None,
                preview_prefix: None,
            },
            true,
        )
        .unwrap();
        assert_eq!(
            shown_message["data"]["event"]["blocks"][0]["type"],
            "reasoning"
        );
        assert_eq!(
            shown_message["data"]["event"]["blocks"][0]["text"],
            "private chain"
        );
    }

    #[test]
    fn a_hidden_reasoning_only_message_does_not_satisfy_once() {
        use whalerein_core::conversation::content_block::{Content, ReasoningContent};
        use whalerein_core::conversation::ContentBlock;
        let update = ViewUpdate::Message {
            message: ConversationMessage {
                message_id: "m".into(),
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock {
                    block_id: "reasoning".into(),
                    content: Some(Content::Reasoning(ReasoningContent {
                        text: "private".into(),
                    })),
                }],
                authored_at_unix_ms: 1,
            },
            origin: None,
            preview_prefix: None,
        };
        assert!(!is_tail_output(&update, false));
        assert!(is_tail_output(&update, true));
    }

    #[test]
    fn the_reasoning_prefix_never_offsets_answer_text() {
        use whalerein_core::conversation::content_block::{Content, ReasoningContent, TextContent};
        use whalerein_core::conversation::ContentBlock;
        let message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock {
                    block_id: "reasoning".into(),
                    content: Some(Content::Reasoning(ReasoningContent { text: "abc".into() })),
                },
                ContentBlock {
                    block_id: "text".into(),
                    content: Some(Content::Text(TextContent { text: "xyz".into() })),
                },
            ],
            ..Default::default()
        };
        let prefix = PreviewPrefix {
            text_bytes: 2,
            reasoning_bytes: 0,
        };
        let out = render_message_blocks(&message, Some(prefix), true);
        assert_eq!(out, "[reasoning] abc\nz", "{out}");
        let previewed = PreviewPrefix {
            text_bytes: 0,
            reasoning_bytes: 1,
        };
        let out = render_message_blocks(&message, Some(previewed), true);
        assert_eq!(out, "bc\nxyz", "{out}");
        let out = render_message_blocks(&message, Some(prefix), false);
        assert_eq!(out, "z", "{out}");
    }

    #[test]
    fn an_unmatched_preview_prefix_closes_the_line_before_the_committed_message() {
        use whalerein_core::conversation::content_block::{Content, ReasoningContent, TextContent};
        use whalerein_core::conversation::ContentBlock;
        let mut renderer = TailRenderer::default();
        assert_eq!(
            renderer.render(&text_preview("provisional", false)),
            "provisional"
        );
        let message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock {
                    block_id: "reasoning".into(),
                    content: Some(Content::Reasoning(ReasoningContent {
                        text: "private".into(),
                    })),
                },
                ContentBlock {
                    block_id: "text".into(),
                    content: Some(Content::Text(TextContent {
                        text: "answer".into(),
                    })),
                },
            ],
            ..Default::default()
        };
        let out = renderer.render(&ViewUpdate::Message {
            message,
            origin: Some(attempt()),
            preview_prefix: Some(PreviewPrefix::default()),
        });
        assert_eq!(out, "\n[ASSISTANT] answer\n", "{out}");
        assert!(!out.contains("provisionalanswer"), "{out}");
    }

    #[test]
    fn human_tail_renders_blocks_in_source_order_with_typed_activity_and_notices() {
        use whalerein_core::conversation::content_block::{
            AttachmentContent, Content, NoticeContent, TextContent,
        };
        use whalerein_core::conversation::{
            ContentBlock, NoticeCategory, NoticeState, ToolActivity, ToolActivityId, ToolOutcome,
            ToolResultPart, ToolStage, ToolState,
        };
        let activity = ToolActivity {
            id: ToolActivityId {
                call_id: "c".into(),
            },
            name: "Read".into(),
            summary: Some("src/main.rs".into()),
            order: 1,
            state: ToolState::Succeeded,
            stages: vec![ToolStage::Preparation, ToolStage::Settlement],
            progress: None,
            failure_code: None,
            result: vec![ToolResultPart::Text("done".into())],
            outcome: ToolOutcome::Succeeded,
            attempt: Some(attempt()),
            call_missing: false,
        };
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
                    content: Some(Content::ToolActivity(activity)),
                },
                ContentBlock {
                    block_id: "file".into(),
                    content: Some(Content::Attachment(AttachmentContent {
                        media_type: "image/png".into(),
                        name: "shot.png".into(),
                        size: 42,
                        available: true,
                        attachment_ref: "ref-1".into(),
                    })),
                },
                ContentBlock {
                    block_id: "notice".into(),
                    content: Some(Content::Notice(NoticeContent {
                        category: NoticeCategory::Retry,
                        state: NoticeState::Started,
                        summary: Some("source resumed".into()),
                    })),
                },
            ],
            ..Default::default()
        };
        let mut renderer = TailRenderer::default();
        let out = renderer.render(&ViewUpdate::Message {
            message,
            origin: None,
            preview_prefix: None,
        });
        let positions = [
            out.find("hello").unwrap(),
            out.find("[tool Read succeeded] src/main.rs").unwrap(),
            out.find("[tool result] done").unwrap(),
            out.find("[attachment shot.png").unwrap(),
            out.find("[notice retry/started] source resumed").unwrap(),
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
        assert!(is_tail_output(&interrupted, false));
        assert_eq!(renderer.render(&interrupted), "[attempt interrupted]\n");
        // A committed attempt is described by its Message, not a synthetic line.
        assert!(!is_tail_output(
            &ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Committed,
            },
            false
        ));
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
            kind: BlockKind::Text,
            text: "x".into(),
            replace: false,
            incomplete: true,
        };
        let value = tail_event_json(&preview, false).unwrap();
        assert_eq!(value["data"]["event"]["type"], "preview");
        assert_eq!(value["data"]["event"]["kind"], "TEXT");
        assert!(value["data"]["event"]["incomplete"].as_bool().unwrap());
        let value = tail_event_json(
            &ViewUpdate::Message {
                message: message(),
                origin: Some(attempt()),
                preview_prefix: None,
            },
            false,
        )
        .unwrap();
        assert_eq!(value["data"]["event"]["origin"]["attempt_id"], "a");
        assert!(value["data"]["event"].get("data_json").is_none());
        let ended = tail_event_json(
            &ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Committed,
            },
            false,
        )
        .unwrap();
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
        assert!(!is_tail_output(&state, false));
        assert_eq!(
            tail_event_json(&state, false).unwrap()["data"]["event"]["type"],
            "state"
        );
        assert_eq!(
            tail_event_json(&ViewUpdate::Reset, false).unwrap()["data"]["event"]["type"],
            "reset"
        );
        assert!(!is_tail_output(&text_preview("", false), false));
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
        use content_block::{Content, ReasoningContent};
        use whalerein_core::conversation::{
            NoticeCategory, NoticeState, ToolActivity, ToolActivityId, ToolOutcome, ToolState,
        };
        let mut message = ConversationMessage {
            message_id: "m".into(),
            role: MessageRole::Assistant,
            ..Default::default()
        };
        for content in [
            Some(Content::Text(Default::default())),
            Some(Content::ToolActivity(ToolActivity {
                id: ToolActivityId {
                    call_id: "c".into(),
                },
                name: "Read".into(),
                summary: None,
                order: 1,
                state: ToolState::Running,
                stages: vec![],
                progress: None,
                failure_code: None,
                result: vec![],
                outcome: ToolOutcome::Unspecified,
                attempt: None,
                call_missing: false,
            })),
            Some(Content::Notice(Default::default())),
            Some(Content::Attachment(Default::default())),
            Some(Content::Reasoning(ReasoningContent {
                text: "private".into(),
            })),
            None,
        ] {
            message.blocks.push(Default::default());
            let id = message.blocks.len().to_string();
            let block = message.blocks.last_mut().unwrap();
            block.block_id = id;
            block.content = content;
        }
        let value = tail_event_json(
            &ViewUpdate::Message {
                message,
                origin: None,
                preview_prefix: None,
            },
            false,
        )
        .unwrap();
        // Hidden reasoning is omitted rather than represented as a block.
        assert_eq!(
            value["data"]["event"]["blocks"].as_array().unwrap().len(),
            5
        );
        assert_eq!(value["data"]["event"]["blocks"][1]["type"], "tool_activity");
        // Notice defaults carry explicit categories rather than free text.
        let _ = (
            NoticeCategory::Retry,
            NoticeState::Started,
            ToolOutcome::Succeeded,
        );
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
            s.projection_id = "projection-1".into();
        }
        let value = tail_event_json(&state, false).unwrap();
        assert_eq!(value["data"]["event"]["run_phase"], "IDLE");
        assert_eq!(value["data"]["event"]["projection_id"], "projection-1");
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
    fn local_core_errors_keep_their_code_and_exit_in_the_cli_outcome() {
        use whalerein_core::CoreError::*;

        for (error, code, exit) in [
            (Config("fixture".into()), "invalid_configuration", 2),
            (PairingCancelled, "cancelled", 2),
            (NotPaired, "not_paired", 4),
            (Revoked, "revoked", 4),
            (HostOffline, "host_offline", 5),
            (HostRejected, "rejected", 6),
            (Timeout, "timeout", 7),
            (UnknownResult, "unknown", 7),
            (Cache("fixture".into()), "cache_error", 1),
            (Security("fixture".into()), "security_error", 1),
        ] {
            let outcome = core_error_outcome(&Command::Hosts, error);
            assert_eq!(outcome.error.unwrap().code, code);
            assert_eq!(outcome.exit, exit, "{code}");
        }
    }

    #[test]
    fn the_cli_error_contract_matches_the_proto_json_table() {
        use whalerein_core::CoreError::{Api, Host};

        let table: serde_json::Value =
            serde_json::from_str(include_str!("../../../proto/v1/error_codes.json")).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for entry in table["codes"].as_array().unwrap() {
            let name = entry["name"].as_str().unwrap();
            let exit = entry["exitCode"].as_u64().unwrap() as u8;
            let action = entry["action"].as_str().unwrap();
            assert_eq!(error_action(name), action, "action for {name}");
            let wire = whalerein_proto::ErrorCode::from_str_name(name).expect(name);
            assert_eq!(whalerein_core::api::error_code_exit_code(wire), exit);
            assert_eq!(whalerein_core::api::error_code_action(wire), action);
            assert_eq!(wire.as_str_name(), name);
            for (error, layer) in [
                (
                    Api {
                        code: Some(wire),
                        message: "fixture".into(),
                    },
                    "access",
                ),
                (
                    Host {
                        code: Some(wire),
                        message: "fixture".into(),
                    },
                    "host",
                ),
            ] {
                let outcome = core_error_outcome(&Command::Hosts, error);
                let envelope = outcome.to_json();
                assert_eq!(outcome.exit, exit, "{name}");
                assert_eq!(envelope["error"]["code"], name);
                assert_eq!(envelope["error"]["action"], action);
                assert_eq!(envelope["error"]["layer"], layer);
            }
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
    fn lifecycle_reasons_preserve_wrapped_wire_codes() {
        // PROCESS_INTERRUPTED is an availability failure, never a silent
        // success and never a retry suggestion.
        let handle = lifecycle("PROCESS_INTERRUPTED");
        assert_eq!(lifecycle_exit_code(&handle), 5);
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
            stamp: None,
            complete: true,
            encoded_bytes: 0,
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
            !is_tail_output(&state, false),
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
    use whalerein_core::conversation::content_block::{Content, TextContent};
    use whalerein_core::conversation::{
        AttemptRef, ContentBlock, ConversationMessage, MessageRole, ToolActivity, ToolActivityId,
        ToolOutcome, ToolState,
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
            kind: BlockKind::Text,
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
                    content: Some(Content::ToolActivity(ToolActivity {
                        id: ToolActivityId {
                            call_id: "c".into(),
                        },
                        name: "Read".into(),
                        summary: Some("file".into()),
                        order: 1,
                        state: ToolState::Succeeded,
                        stages: vec![],
                        progress: None,
                        failure_code: None,
                        result: vec![],
                        outcome: ToolOutcome::Succeeded,
                        attempt: None,
                        call_missing: false,
                    })),
                },
            ],
            ..Default::default()
        };
        let out = renderer.render(&ViewUpdate::Message {
            message,
            origin: Some(attempt()),
            preview_prefix: Some(PreviewPrefix {
                text_bytes: 6,
                reasoning_bytes: 0,
            }),
        });
        assert!(!out.contains("one"), "delivered text was reprinted: {out}");
        assert!(!out.contains("TWO"), "delivered text was reprinted: {out}");
        assert!(out.contains("[tool Read succeeded] file"), "{out}");
    }
}

#[cfg(test)]
mod history_output_tests {
    use super::*;
    use whalerein_core::conversation::content_block::{
        AttachmentContent, Content, ReasoningContent, TextContent,
    };
    use whalerein_core::conversation::{ContentBlock, ConversationMessage, MessageRole};
    use whalerein_core::history::HistoryContinuation;

    fn key() -> SessionKey {
        SessionKey::new("host-1", "session-1").unwrap()
    }

    fn message(id: &str, text: &str, reasoning: Option<&str>) -> ConversationMessage {
        let mut blocks = Vec::new();
        if let Some(reasoning) = reasoning {
            blocks.push(ContentBlock {
                block_id: "reasoning".into(),
                content: Some(Content::Reasoning(ReasoningContent {
                    text: reasoning.into(),
                })),
            });
        }
        blocks.push(ContentBlock {
            block_id: "text".into(),
            content: Some(Content::Text(TextContent { text: text.into() })),
        });
        ConversationMessage {
            message_id: id.into(),
            role: MessageRole::Assistant,
            blocks,
            authored_at_unix_ms: 1,
        }
    }

    fn page() -> HistoryPage {
        HistoryPage {
            source: HistorySource::Online,
            projection: "projection-1".into(),
            complete: false,
            messages: vec![
                message("m-2", "second", Some("private reasoning")),
                message("m-3", "third", None),
            ],
            activities: vec![],
            continuation: Some("continuation-token".parse::<HistoryContinuation>().unwrap()),
        }
    }

    #[test]
    fn history_human_defaults_to_text_and_labels_its_source_and_continuation() {
        let text = render_history_page(&key(), &page(), false);
        assert!(text.contains("Source: online"), "{text}");
        assert!(text.contains("Projection: projection-1"), "{text}");
        assert!(text.contains("[ASSISTANT] second"), "{text}");
        assert!(text.contains("[ASSISTANT] third"), "{text}");
        assert!(!text.contains("private reasoning"), "{text}");
        assert!(!text.contains("[reasoning]"), "{text}");
        assert!(text.contains("Continuation: continuation-token"), "{text}");
        assert!(text.contains("--continuation continuation-token"), "{text}");
        let shown = render_history_page(&key(), &page(), true);
        assert!(shown.contains("[reasoning] private reasoning"), "{shown}");
    }

    #[test]
    fn history_json_identifies_source_continuation_and_attachment_save_action() {
        let mut page = page();
        page.messages[0].blocks.push(ContentBlock {
            block_id: "file".into(),
            content: Some(Content::Attachment(AttachmentContent {
                media_type: "image/png".into(),
                name: "shot.png".into(),
                size: 42,
                available: true,
                attachment_ref: "opaque-ref".into(),
            })),
        });
        let value = history_page_data(&key(), &page, false);
        assert_eq!(value["source"], "online");
        assert_eq!(value["projection"], "projection-1");
        assert_eq!(value["complete"], false);
        assert_eq!(value["continuation"], "continuation-token");
        assert_eq!(value["messages"][0]["message_id"], "m-2");
        assert_eq!(value["messages"][0]["blocks"].as_array().unwrap().len(), 2);
        assert_eq!(value["messages"][0]["blocks"][1]["type"], "attachment");
        assert_eq!(value["attachments"][0]["name"], "shot.png");
        assert_eq!(value["attachments"][0]["reference"], "opaque-ref");
        let save = value["attachments"][0]["save_command"].as_str().unwrap();
        assert!(
            save.starts_with("whalerein session download opaque-ref"),
            "{save}"
        );
        assert!(save.contains("--host host-1"), "{save}");
        assert!(save.contains("--session session-1"), "{save}");
        assert!(!value.to_string().contains("private reasoning"));
    }

    #[test]
    fn exact_history_output_uses_the_same_reasoning_policy() {
        let reference = MessageReference::new(key(), "m-2", "locator:m-2", "projection-1").unwrap();
        let message = message("m-2", "second", Some("private reasoning"));
        let value = history_message_data(&key(), &reference, &message, false);
        assert_eq!(value["source"], "online");
        assert_eq!(value["message"]["message_id"], "m-2");
        assert!(!value.to_string().contains("private reasoning"));
        let text = render_history_message(&key(), &reference, &message, false);
        assert!(text.contains("Source: online"), "{text}");
        assert!(!text.contains("private reasoning"), "{text}");
        assert!(text.contains("Reference: "), "{text}");
        let shown = history_message_data(&key(), &reference, &message, true);
        assert!(shown.to_string().contains("private reasoning"));
    }

    #[test]
    fn shell_quoting_neutralizes_hostile_scoped_arguments() {
        assert_eq!(shell_quote("plain-value_1"), "plain-value_1");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_quote("x; echo pwned"), "'x; echo pwned'");
        assert_eq!(shell_quote("line\nbreak"), "'line\nbreak'");
    }

    #[test]
    fn an_expired_history_continuation_points_at_a_restart() {
        let command = Command::History {
            session: "session-1".into(),
            cache: false,
            continuation: Some("stale".into()),
            message: None,
            max_messages: None,
            max_bytes: None,
        };
        let outcome = core_error_outcome(
            &command,
            whalerein_core::CoreError::Api {
                code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
                message: "history changed".into(),
            },
        );
        assert_eq!(outcome.exit, 8);
        let error = outcome.error.unwrap();
        assert_eq!(error.code, "snapshot_required");
        assert!(
            error.message.contains("without --continuation"),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains("whalerein session history session-1"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_stale_exact_reference_points_at_a_refresh_without_resubmission() {
        let command = Command::History {
            session: "session-1".into(),
            cache: false,
            continuation: None,
            message: Some("stale".into()),
            max_messages: None,
            max_bytes: None,
        };
        let outcome = core_error_outcome(
            &command,
            whalerein_core::CoreError::Api {
                code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
                message: "stale".into(),
            },
        );
        let message = outcome.error.unwrap().message;
        assert!(message.contains("refresh history"), "{message}");
        assert!(message.contains("Do not resend"), "{message}");
    }
    #[test]
    fn download_progress_is_bounded_and_keeps_json_structured() {
        let mut json = DownloadProgressRenderer::new(true);
        let mut lines = Vec::new();
        for part in 0..=(100 * 1024 * 1024 / 4096) {
            let received = part * 4096;
            if let Some(line) = json.report(DownloadProgress {
                received,
                total: 100 * 1024 * 1024,
            }) {
                lines.push(line);
            }
        }
        // The commit notification repeats the final count and must not add a line.
        assert!(json
            .report(DownloadProgress {
                received: 100 * 1024 * 1024,
                total: 100 * 1024 * 1024,
            })
            .is_none());
        assert!(lines.len() <= 12, "{}", lines.len());
        let first: Value = serde_json::from_str(lines.first().unwrap()).unwrap();
        assert_eq!(first["event"], "download_progress");
        assert_eq!(first["received"], 0);
        assert_eq!(first["total"], 100 * 1024 * 1024);
        let last: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(last["received"], 100 * 1024 * 1024);
        assert!(lines.iter().all(|line| line.ends_with('\n')));

        let mut human = DownloadProgressRenderer::new(false);
        let start = human
            .report(DownloadProgress {
                received: 0,
                total: 3,
            })
            .unwrap();
        assert_eq!(start, "Progress: 0/3 bytes (0%)\n");
        let done = human
            .report(DownloadProgress {
                received: 3,
                total: 3,
            })
            .unwrap();
        assert_eq!(done, "Progress: 3/3 bytes (100%)\n");
        assert!(human
            .report(DownloadProgress {
                received: 3,
                total: 3,
            })
            .is_none());
    }

    #[test]
    fn empty_download_progress_declares_one_event() {
        let mut human = DownloadProgressRenderer::new(false);
        assert_eq!(
            human
                .report(DownloadProgress {
                    received: 0,
                    total: 0
                })
                .unwrap(),
            "Progress: 0/0 bytes (complete)\n"
        );
        assert!(human
            .report(DownloadProgress {
                received: 0,
                total: 0
            })
            .is_none());
    }

    #[test]
    fn download_failure_guidance_names_recovery_and_preserved_data() {
        let cases = [
            ("conflict", "existing file was preserved", "--force"),
            ("timeout", "fresh transfer", "session history session-1"),
            (
                "invalid_argument",
                "attachment file name is not safe",
                "--name",
            ),
            (
                "internal",
                "attachment destination has no free space",
                "no free space",
            ),
            (
                "payload_digest_mismatch",
                "integrity verification failed",
                "corrupt",
            ),
            (
                "unsupported_capability",
                "cannot read attachments",
                "Upgrade",
            ),
            ("rate_limited", "budget is exhausted", "Wait"),
            ("cancelled", "no completed file", "fresh transfer"),
        ];
        for (code, detail, expected) in cases {
            let message = download_failure_message(detail, code, "session-1");
            assert!(
                message.contains(expected),
                "{code}: expected {expected:?} in {message:?}"
            );
            assert!(!message.contains("/tmp/private"), "{code}: {message}");
        }
        let destination = download_failure_message(
            "attachment destination does not exist",
            "invalid_argument",
            "s",
        );
        assert!(destination.contains("--to <DIR>"), "{destination}");
        let unauthorized = download_failure_message("unauthorized", "unauthorized", "s");
        assert!(unauthorized.contains("whalerein status"), "{unauthorized}");
    }

    #[test]
    fn an_unnamed_attachment_displays_and_saves_under_a_media_derived_name() {
        let attachment = content_block::AttachmentContent {
            media_type: "image/png".into(),
            name: String::new(),
            size: 3,
            available: true,
            attachment_ref: "ref-1".into(),
        };
        assert!(render_attachment(&attachment).contains("attachment.png"));
        let key = SessionKey::new("host-1", "session-1").unwrap();
        let rows = render_attachment_rows(&key, &[attachment]);
        assert!(rows.contains("Attachment: attachment.png"), "{rows}");
        assert!(rows.contains("Save:"), "{rows}");
    }

    #[test]
    fn long_tool_results_are_bounded_unless_full_output_is_requested() {
        let text = "x".repeat(TOOL_RESULT_DISPLAY_BYTES + 100);
        let activity = ToolActivity {
            id: whalerein_core::conversation::ToolActivityId {
                call_id: "c".into(),
            },
            name: "Read".into(),
            summary: None,
            order: 1,
            state: whalerein_core::conversation::ToolState::Succeeded,
            stages: vec![whalerein_core::conversation::ToolStage::Settlement],
            progress: None,
            failure_code: None,
            result: vec![ToolResultPart::Text(text.clone())],
            outcome: whalerein_core::conversation::ToolOutcome::Succeeded,
            attempt: None,
            call_missing: false,
        };
        set_full_tool_output(false);
        let bounded = render_tool_activity(&activity);
        assert!(bounded.contains("bytes hidden"), "{bounded}");
        assert!(
            !bounded.contains(&text),
            "bounded output must not contain the full result"
        );
        set_full_tool_output(true);
        let complete = render_tool_activity(&activity);
        assert!(complete.contains(&text));
        set_full_tool_output(false);
    }

    #[test]
    fn duplicate_attachment_names_get_distinct_saves_and_unavailable_rows_explain() {
        let key = SessionKey::new("host-1", "session-1").unwrap();
        let first = content_block::AttachmentContent {
            media_type: "application/octet-stream".into(),
            name: "report.bin".into(),
            size: 4,
            available: true,
            attachment_ref: "ref-1".into(),
        };
        let mut second = first.clone();
        second.attachment_ref = "ref-2".into();
        let unavailable = content_block::AttachmentContent {
            media_type: "application/octet-stream".into(),
            name: "gone.bin".into(),
            size: 4,
            available: false,
            attachment_ref: "ref-3".into(),
        };
        let rows = render_attachment_rows(&key, &[first.clone(), second, unavailable]);
        assert!(rows.contains("--name attachment-2.bin"), "{rows}");
        assert!(rows.contains("did not expose readable bytes"), "{rows}");
        let values = attachments_json(&key, &[first]);
        assert!(values[0]["save_command"]
            .as_str()
            .unwrap()
            .contains("session download"));
        let large = content_block::AttachmentContent {
            media_type: "image/png".into(),
            name: "huge.png".into(),
            size: 40 * 1024 * 1024,
            available: true,
            attachment_ref: "ref-4".into(),
        };
        let rows = render_attachment_rows(&key, &[large]);
        assert!(rows.contains("Large image"), "{rows}");
    }
}
