//! Flat, transport-neutral projections of Core state for the mobile clients.
//!
//! Every conversion from protocol-backed Core types is total: unknown enum
//! values become the explicit `"unknown"` string instead of panicking. These
//! types carry no credentials, invitation secrets or ciphertext.

use std::collections::HashMap;

use whalerein_core::api as core_api;
use whalerein_core::api::{
    ApprovalInfo, DeliveryMode, HostInfo, InputKind, OnlineStatus, PairingPrompt,
    QuestionBatchInfo, QuestionFreshness, QuestionPageInfo, SendReply, SessionCapabilities,
    SessionKey, SessionSummary,
};
use whalerein_core::attachments::{AttachmentReference, DownloadOutcome, DownloadProgress};
use whalerein_core::conversation::{
    content_block, ApprovalState, AttemptOutcome, AttemptRef, BlockKind, ConversationMessage,
    MessageRole, NoticeCategory, NoticeState, PreviewPrefix, RunPhase, ToolActivity,
    ToolResultPart, ToolStage, ToolState, ViewUpdate,
};
use whalerein_core::history::{HistoryPage, HistorySource as CoreHistorySource, MessageReference};

/// One pending native iOS sign-in attempt. Only the hosted URL and the
/// attempt identity cross the boundary; state, PKCE material and the local
/// credential stay in Core's SecretStore.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudAuthAttemptView {
    pub url: String,
    pub attempt_id: String,
}

/// The verified Cloud account and workspace bound to this instance.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudAccountView {
    pub account_id: String,
    pub email: String,
    pub issuer: String,
    pub subject: String,
    pub workspace_id: String,
}

impl From<core_api::SignedInAccount> for CloudAccountView {
    fn from(value: core_api::SignedInAccount) -> Self {
        Self {
            account_id: value.account.id,
            email: value.account.email,
            issuer: value.account.issuer,
            subject: value.account.subject,
            workspace_id: value.workspace.id,
        }
    }
}

/// Local account state: the verified account (if any) and whether a prior
/// local sign-out still needs remote confirmation.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudAccountStatusView {
    pub account: Option<CloudAccountView>,
    pub logout_pending: bool,
}

/// One Cloud app session visible to the signed-in account. No token is exposed.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudAccountSessionView {
    pub session_id: String,
    pub instance_id: String,
    pub expires_at: u64,
    pub state: String,
}

impl From<core_api::AccountSessionInfo> for CloudAccountSessionView {
    fn from(value: core_api::AccountSessionInfo) -> Self {
        Self {
            session_id: value.session_id,
            instance_id: value.instance_id,
            expires_at: value.expires_at,
            state: value.state.as_str().to_owned(),
        }
    }
}

/// One computer in the signed-in account's Cloud directory.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudDirectoryHostView {
    pub host_id: String,
    pub display_name: String,
    pub enrolled: bool,
    pub online: bool,
    pub presence: String,
    pub observation: String,
    pub enrollment_state: String,
    pub pairing_eligible: bool,
    pub device_access: String,
}

impl From<core_api::AccountHost> for CloudDirectoryHostView {
    fn from(value: core_api::AccountHost) -> Self {
        Self {
            host_id: value.host_id,
            display_name: value.display_name,
            enrolled: value.enrolled,
            online: value.online,
            presence: value.presence,
            observation: value.observation,
            enrollment_state: value.enrollment_state,
            pairing_eligible: value.pairing_eligible,
            device_access: value.device_access,
        }
    }
}

/// The result of connecting one eligible Cloud computer. The access URL and
/// any device token remain inside Core.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudConnectResultView {
    pub operation_id: Option<String>,
    pub host_id: String,
    pub device_id: String,
    pub fingerprint: String,
}

impl From<core_api::PairingOutcome> for CloudConnectResultView {
    fn from(value: core_api::PairingOutcome) -> Self {
        Self {
            operation_id: value.operation_id,
            host_id: value.host_id,
            device_id: value.device_id,
            fingerprint: value.fingerprint,
        }
    }
}

/// A paired Host as reported by Access. `fingerprint` is the confirmed pin.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HostSummary {
    pub host_id: String,
    pub online: bool,
    pub fingerprint: String,
    pub protocol_version: u32,
}

impl From<HostInfo> for HostSummary {
    fn from(value: HostInfo) -> Self {
        Self {
            host_id: value.host_id,
            online: value.online,
            fingerprint: value.fingerprint,
            protocol_version: value.protocol_version,
        }
    }
}

/// One session row for the session list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionSummaryView {
    pub session_id: String,
    pub title: String,
    pub updated_at_unix_ms: i64,
    pub cwd: String,
    /// True while the Source reports an active turn.
    pub running: bool,
    /// Source capabilities, when the Host reported them for this session.
    pub capabilities: Option<SessionCapabilitiesView>,
}

impl From<SessionSummary> for SessionSummaryView {
    fn from(value: SessionSummary) -> Self {
        Self {
            session_id: value.session_id,
            title: value.title,
            updated_at_unix_ms: value.updated_at_unix_ms,
            cwd: value.cwd,
            running: value.running,
            capabilities: value.source_capabilities.map(SessionCapabilitiesView::from),
        }
    }
}

/// Source capabilities for one session, used to gate composer actions.
///
/// Values mirror Core's `SessionCapabilities`; `delivery_modes` contains
/// stable lowercase mode names (`start_if_idle`, `enqueue_after_run`,
/// `steer`). Unknown modes are preserved in lowercase instead of being
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionCapabilitiesView {
    pub history: bool,
    pub resumable_history: bool,
    pub live_text: bool,
    pub interrupt: bool,
    pub approvals: bool,
    /// The Source can stream displayable reasoning blocks.
    pub live_reasoning: bool,
    /// The Source reports typed tool activities.
    pub tool_activity: bool,
    /// The Source reports typed conversation notices.
    pub conversation_notices: bool,
    /// The Source supports attachment metadata and reads.
    pub attachments: bool,
    /// The Source supports independent bounded history pages.
    pub history_page: bool,
    /// The Source supports exact committed-message reads.
    pub history_exact: bool,
    /// The Source supports typed send follow-up replies.
    pub send_followup: bool,
    pub delivery_modes: Vec<String>,
    pub model_selection: bool,
    pub pending_queue_read: bool,
    pub pending_queue_edit: bool,
    pub pending_queue_remove: bool,
    pub pending_queue_steer: bool,
    pub questions_read: bool,
    pub questions_answer_open: bool,
    pub questions_answer_continued: bool,
    pub session_fork: bool,
}

impl From<SessionCapabilities> for SessionCapabilitiesView {
    fn from(value: SessionCapabilities) -> Self {
        Self {
            history: value.history,
            resumable_history: value.resumable_history,
            live_text: value.live_text,
            interrupt: value.interrupt,
            approvals: value.approvals,
            live_reasoning: value.live_reasoning,
            tool_activity: value.tool_activity,
            conversation_notices: value.conversation_notices,
            attachments: value.attachments,
            history_page: value.history_page,
            history_exact: value.history_exact,
            send_followup: value.send_followup,
            delivery_modes: value
                .delivery_modes
                .into_iter()
                .map(|mode| normalize_delivery_mode(&mode))
                .collect(),
            model_selection: value.model_selection,
            pending_queue_read: value.pending_queue_read,
            pending_queue_edit: value.pending_queue_edit,
            pending_queue_remove: value.pending_queue_remove,
            pending_queue_steer: value.pending_queue_steer,
            questions_read: value.questions_read,
            questions_answer_open: value.questions_answer_open,
            questions_answer_continued: value.questions_answer_continued,
            session_fork: value.session_fork,
        }
    }
}

/// Lowercase, stable delivery-mode name. Unknown values keep their identity.
fn normalize_delivery_mode(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "start_if_idle" | "delivery_mode_start_if_idle" => "start_if_idle".into(),
        "enqueue_after_run" | "delivery_mode_enqueue_after_run" => "enqueue_after_run".into(),
        "steer" | "delivery_mode_steer" => "steer".into(),
        other => other.to_owned(),
    }
}

/// Host identity the user must confirm before pairing commits.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairingPromptView {
    pub host_id: String,
    pub access_url: String,
    pub fingerprint: String,
    pub fingerprint_display: String,
}

impl From<PairingPrompt> for PairingPromptView {
    fn from(value: PairingPrompt) -> Self {
        Self {
            host_id: value.host_id,
            access_url: value.access_url,
            fingerprint: value.fingerprint,
            fingerprint_display: value.fingerprint_display,
        }
    }
}

/// A pending approval with the exact version and digest the user reviewed.
///
/// `digest_hex` is the digest that must be sent back to
/// `resolveApproval`; the pair `(version, digest_hex)` binds the decision to
/// the reviewed request.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ApprovalView {
    pub approval_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub call_id: String,
    pub reason: String,
    pub version: i64,
    pub expires_at_unix_ms: i64,
    pub digest_hex: String,
    pub state: String,
}

impl From<ApprovalInfo> for ApprovalView {
    fn from(value: ApprovalInfo) -> Self {
        Self {
            approval_id: value.approval_id,
            session_id: value.session_id,
            tool_name: value.tool_name,
            call_id: value.call_id,
            reason: value.reason,
            version: value.version,
            expires_at_unix_ms: value.expires_at_unix_ms,
            digest_hex: hex::encode(value.digest),
            state: normalize_approval_state(&value.state),
        }
    }
}

fn normalize_approval_state(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "pending" | "approval_state_pending" => "pending".into(),
        "approved" | "approval_state_approved" => "approved".into(),
        "rejected" | "approval_state_rejected" => "rejected".into(),
        "expired" | "approval_state_expired" => "expired".into(),
        "unavailable" | "approval_state_unavailable" => "unavailable".into(),
        _ => "unknown".into(),
    }
}

fn approval_state_name(value: ApprovalState) -> String {
    normalize_approval_state(value.as_str_name())
}

/// One ordered, displayable part of a settled tool result.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ToolResultPartView {
    Text {
        text: String,
    },
    Attachment {
        media_type: String,
        name: String,
        size: u64,
        available: bool,
        /// Opaque Source occurrence reference used to request a verified
        /// download. It is never bytes or an authorization token.
        attachment_ref: String,
    },
}

impl From<&ToolResultPart> for ToolResultPartView {
    fn from(value: &ToolResultPart) -> Self {
        match value {
            ToolResultPart::Text(text) => Self::Text { text: text.clone() },
            ToolResultPart::Attachment(attachment) => Self::Attachment {
                media_type: attachment.media_type.clone(),
                name: attachment.name.clone(),
                size: attachment.size,
                available: attachment.available,
                attachment_ref: attachment.attachment_ref.clone(),
            },
        }
    }
}

/// One canonical tool activity. Core merges repeated occurrences by `call_id`;
/// the binding never correlates call and result records itself.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ToolActivityView {
    pub call_id: String,
    pub name: String,
    pub summary: Option<String>,
    /// Stable lowercase state: `preparing`, `running`, `succeeded`, `failed`,
    /// `interrupted`, `abandoned` or `unknown`.
    pub state: String,
    /// Evidenced stages in ascending order, as stable lowercase names.
    pub stages: Vec<String>,
    pub progress: Option<String>,
    pub failure_code: Option<String>,
    pub result: Vec<ToolResultPartView>,
    /// True when the first observed occurrence proved a result without the
    /// corresponding call record.
    pub call_missing: bool,
}

impl From<&ToolActivity> for ToolActivityView {
    fn from(value: &ToolActivity) -> Self {
        Self {
            call_id: value.id.call_id.clone(),
            name: value.name.clone(),
            summary: value.summary.clone(),
            state: tool_state_name(value.state),
            stages: value.stages.iter().copied().map(tool_stage_name).collect(),
            progress: value.progress.clone(),
            failure_code: value.failure_code.clone(),
            result: value.result.iter().map(ToolResultPartView::from).collect(),
            call_missing: value.call_missing,
        }
    }
}

fn tool_state_name(value: ToolState) -> String {
    value.as_str().to_owned()
}

fn tool_stage_name(value: ToolStage) -> String {
    match value {
        ToolStage::Preparation => "preparation",
        ToolStage::Execution => "execution",
        ToolStage::Progress => "progress",
        ToolStage::Settlement => "settlement",
        ToolStage::Unspecified => "unknown",
    }
    .to_owned()
}

/// One typed notice. `summary` is the only display body; Core never exposes raw
/// command arguments, compaction input or an unclassified notice body.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NoticeView {
    pub category: String,
    pub state: String,
    pub summary: Option<String>,
}

impl From<&content_block::NoticeContent> for NoticeView {
    fn from(value: &content_block::NoticeContent) -> Self {
        Self {
            category: notice_category_name(value.category),
            state: notice_state_name(value.state),
            summary: value.summary.clone(),
        }
    }
}

fn notice_category_name(value: NoticeCategory) -> String {
    match value {
        NoticeCategory::Retry => "retry",
        NoticeCategory::MaxOutput => "max_output",
        NoticeCategory::Compaction => "compaction",
        NoticeCategory::Command => "command",
        NoticeCategory::Context => "context",
        NoticeCategory::Omission => "omission",
        NoticeCategory::Unspecified => "unknown",
    }
    .to_owned()
}

fn notice_state_name(value: NoticeState) -> String {
    match value {
        NoticeState::Scheduled => "scheduled",
        NoticeState::Started => "started",
        NoticeState::Cancelled => "cancelled",
        NoticeState::Completed => "completed",
        NoticeState::Failed => "failed",
        NoticeState::Reached => "reached",
        NoticeState::Unspecified => "unknown",
    }
    .to_owned()
}

/// One content block of a committed message. `kind` is stable text:
/// `text`, `reasoning`, `tool`, `notice`, `attachment` or `unknown`.
///
/// `tool_activity` is present for tool blocks, `notice` for notice blocks and
/// `attachment` for attachment blocks; `text` stays the display body so
/// existing clients keep rendering.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BlockView {
    pub block_id: String,
    pub kind: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub tool_activity: Option<ToolActivityView>,
    pub notice: Option<NoticeView>,
    pub attachment: Option<AttachmentView>,
}

/// One canonical committed attachment occurrence. `attachment_ref` is opaque
/// Source evidence, not a path, digest or authorization token.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentView {
    pub media_type: String,
    pub name: String,
    pub size: u64,
    pub available: bool,
    pub attachment_ref: String,
}

impl From<&content_block::AttachmentContent> for AttachmentView {
    fn from(value: &content_block::AttachmentContent) -> Self {
        Self {
            media_type: value.media_type.clone(),
            name: value.name.clone(),
            size: value.size,
            available: value.available,
            attachment_ref: value.attachment_ref.clone(),
        }
    }
}

/// A committed conversation message.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MessageView {
    pub message_id: String,
    pub role: String,
    pub blocks: Vec<BlockView>,
    pub created_at_unix_ms: i64,
}

impl From<&ConversationMessage> for MessageView {
    fn from(value: &ConversationMessage) -> Self {
        message_view(value)
    }
}

/// The single committed-message projection shared by live events and cached
/// history replay. Keeping one function prevents role/block drift.
pub(crate) fn message_view(message: &ConversationMessage) -> MessageView {
    MessageView {
        message_id: message.message_id.clone(),
        role: message_role_name(message.role),
        blocks: message
            .blocks
            .iter()
            .map(|block| block_view(&block.block_id, block.content.as_ref()))
            .collect(),
        created_at_unix_ms: message.authored_at_unix_ms,
    }
}

fn block_view(block_id: &str, content: Option<&content_block::Content>) -> BlockView {
    let mut view = BlockView {
        block_id: block_id.to_owned(),
        kind: "unknown".into(),
        text: String::new(),
        tool_name: None,
        tool_activity: None,
        notice: None,
        attachment: None,
    };
    match content {
        Some(content_block::Content::Text(text)) => {
            view.kind = "text".into();
            view.text = text.text.clone();
        }
        Some(content_block::Content::Reasoning(reasoning)) => {
            view.kind = "reasoning".into();
            view.text = reasoning.text.clone();
        }
        Some(content_block::Content::ToolActivity(activity)) => {
            view.kind = "tool".into();
            view.text = tool_activity_text(activity);
            if !activity.name.is_empty() {
                view.tool_name = Some(activity.name.clone());
            }
            view.tool_activity = Some(ToolActivityView::from(activity));
        }
        Some(content_block::Content::Notice(notice)) => {
            view.kind = "notice".into();
            view.text = notice.summary.clone().unwrap_or_default();
            view.notice = Some(NoticeView::from(notice));
        }
        Some(content_block::Content::Attachment(attachment)) => {
            view.kind = "attachment".into();
            view.text = attachment.name.clone();
            view.attachment = Some(AttachmentView::from(attachment));
        }
        None => {}
    }
    view
}

/// Display body for a tool block: settled result text when one exists, then
/// the call summary or progress. It never invents a state.
fn tool_activity_text(activity: &ToolActivity) -> String {
    if !activity.result.is_empty() {
        return activity
            .result
            .iter()
            .map(|part| match part {
                ToolResultPart::Text(text) => text.clone(),
                ToolResultPart::Attachment(attachment) => attachment.name.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    activity
        .summary
        .clone()
        .or_else(|| activity.progress.clone())
        .unwrap_or_default()
}

fn message_role_name(value: MessageRole) -> String {
    match value {
        MessageRole::User => "user".into(),
        MessageRole::Assistant => "assistant".into(),
        MessageRole::Tool => "tool".into(),
        MessageRole::Notice => "notice".into(),
        MessageRole::Context => "context".into(),
        MessageRole::Unspecified | MessageRole::Unknown => "unknown".into(),
    }
}

fn block_kind_name(value: BlockKind) -> String {
    match value {
        BlockKind::Text => "text",
        BlockKind::Reasoning => "reasoning",
        BlockKind::Unspecified => "unknown",
    }
    .to_owned()
}

fn run_phase_name(value: RunPhase) -> String {
    match value {
        RunPhase::Idle => "idle".into(),
        RunPhase::Running => "running".into(),
        RunPhase::Stopping => "stopping".into(),
        RunPhase::SourceUnavailable => "source_unavailable".into(),
        RunPhase::Unspecified => "unknown".into(),
    }
}

/// A turn is active while RUNNING or STOPPING; `run_phase` distinguishes them.
fn run_phase_running(value: RunPhase) -> bool {
    matches!(value, RunPhase::Running | RunPhase::Stopping)
}

fn attempt_outcome_name(value: AttemptOutcome) -> String {
    match value {
        AttemptOutcome::Committed => "committed".into(),
        AttemptOutcome::Abandoned => "abandoned".into(),
        AttemptOutcome::Interrupted => "interrupted".into(),
        AttemptOutcome::AttemptFailed => "attempt_failed".into(),
        AttemptOutcome::Unspecified => "unknown".into(),
    }
}

/// Result of a send, stop or approval control. `status` is the stable Core
/// online status: `received`, `host_accepted`, `succeeded`, `host_offline`,
/// `rejected`, `failed` or `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CommandView {
    pub request_id: String,
    pub command_id: String,
    pub status: String,
    pub message_id: Option<String>,
    pub delivery_id: Option<String>,
}

impl From<whalerein_core::api::CommandHandle> for CommandView {
    fn from(value: whalerein_core::api::CommandHandle) -> Self {
        Self {
            request_id: value.request_id,
            command_id: value.command_id,
            status: value.status.as_str().to_owned(),
            message_id: value.message_id,
            delivery_id: value.delivery_id,
        }
    }
}

/// A durable retained operation. `kind` is `send`, `control` or `pairing`.
/// `status` is the shared recovery state: `submitted`, `accepted`, `uncertain`,
/// `failed` or `confirmed`. The receipt is the immediate submission result; the
/// reply, content and read error are only populated by an explicit follow-up.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RetainedOperationView {
    pub operation_id: String,
    pub kind: String,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
    pub status: String,
    pub receipt: Option<CommandView>,
    pub reply: Option<MessageReplyView>,
    pub content: Option<MessageView>,
    pub read_error: Option<String>,
}

/// Map one Core command status onto the shared recovery status vocabulary.
/// A command that has not reported a receipt is `submitted`; a lost or
/// unconfirmed result is `uncertain`, never a fabricated success.
pub(crate) fn operation_status_name(status: OnlineStatus) -> &'static str {
    match status {
        OnlineStatus::Received => "submitted",
        OnlineStatus::HostAccepted => "accepted",
        OnlineStatus::Succeeded => "confirmed",
        OnlineStatus::Rejected | OnlineStatus::Failed => "failed",
        OnlineStatus::HostOffline | OnlineStatus::Unknown => "uncertain",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendOperationView {
    pub operation_id: String,
    pub host_id: String,
    pub session_id: String,
    pub receipt: Option<CommandView>,
    pub reply: Option<MessageReplyView>,
}

/// Host-qualified session identity carried by typed send results.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionKeyView {
    pub host_id: String,
    pub session_id: String,
}

impl From<SessionKey> for SessionKeyView {
    fn from(value: SessionKey) -> Self {
        Self {
            host_id: value.host_id,
            session_id: value.session_id,
        }
    }
}

impl From<&SessionKey> for SessionKeyView {
    fn from(value: &SessionKey) -> Self {
        Self {
            host_id: value.host_id.clone(),
            session_id: value.session_id.clone(),
        }
    }
}

/// Scoped reference to a canonical committed message. Resolve its body through
/// Core history; the reference itself never carries content. `source_locator`
/// is the opaque Source identity required to reconstruct the exact reference.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MessageReferenceView {
    pub session: SessionKeyView,
    pub message_id: String,
    pub source_locator: String,
    pub projection_id: String,
}

impl From<&MessageReference> for MessageReferenceView {
    fn from(value: &MessageReference) -> Self {
        Self {
            session: SessionKeyView::from(value.session()),
            message_id: value.message_id().to_owned(),
            source_locator: value.source_locator().to_owned(),
            projection_id: value.projection_id().to_owned(),
        }
    }
}

impl MessageReferenceView {
    /// Rebuild the Core reference from its typed fields.
    pub(crate) fn to_core(&self) -> Result<MessageReference, ()> {
        if self.message_id.is_empty()
            || self.message_id.len() > 512
            || self.source_locator.is_empty()
            || self.source_locator.len() > 4096
            || self.projection_id.is_empty()
            || self.projection_id.len() > 128
        {
            return Err(());
        }
        let session = SessionKey::new(
            self.session.host_id.clone(),
            self.session.session_id.clone(),
        )
        .map_err(|_| ())?;
        MessageReference::new(
            session,
            self.message_id.clone(),
            self.source_locator.clone(),
            self.projection_id.clone(),
        )
        .map_err(|_| ())
    }
}

/// One bounded committed-history page. `source` is `online` or `cache`; the
/// opaque `continuation` is only valid for the original Host/session scope and
/// content projection. Messages are chronological within the page; later pages
/// step toward older history.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HistoryPageView {
    pub source: String,
    pub projection: String,
    pub complete: bool,
    pub messages: Vec<MessageView>,
    pub activities: Vec<ToolActivityView>,
    pub continuation: Option<String>,
}

impl From<&HistoryPage> for HistoryPageView {
    fn from(value: &HistoryPage) -> Self {
        Self {
            source: match value.source {
                CoreHistorySource::Online => "online".into(),
                CoreHistorySource::Cache => "cache".into(),
            },
            projection: value.projection.clone(),
            complete: value.complete,
            messages: value.messages.iter().map(message_view).collect(),
            activities: value
                .activities
                .iter()
                .map(ToolActivityView::from)
                .collect(),
            continuation: value
                .continuation
                .as_ref()
                .map(std::string::ToString::to_string),
        }
    }
}

/// Scoped committed attachment occurrence used to request a verified save.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentReferenceView {
    pub session: SessionKeyView,
    pub attachment_ref: String,
}

impl AttachmentReferenceView {
    pub(crate) fn to_core(&self) -> Result<AttachmentReference, ()> {
        let session = SessionKey::new(
            self.session.host_id.clone(),
            self.session.session_id.clone(),
        )
        .map_err(|_| ())?;
        Ok(AttachmentReference::new(
            session,
            self.attachment_ref.clone(),
        ))
    }
}

/// One verified attachment save into an app-owned destination directory.
/// `file_name` absent uses the Source suggestion or a media-derived name.
/// `overwrite` false preserves an existing final file.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentDownloadRequestView {
    pub reference: AttachmentReferenceView,
    pub destination: String,
    pub file_name: Option<String>,
    pub overwrite: bool,
}

/// Bounded transfer progress. The first event declares the total with
/// `received == 0`; the final verified commit is only reported through the
/// download result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentProgressView {
    pub received: u64,
    pub total: u64,
}

impl From<DownloadProgress> for AttachmentProgressView {
    fn from(value: DownloadProgress) -> Self {
        Self {
            received: value.received,
            total: value.total,
        }
    }
}

/// One verified local save. `path` exists only after verification and an
/// atomic commit; `verified` is always true for a returned download.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentDownloadView {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub media_type: String,
    pub verified: bool,
}

impl From<DownloadOutcome> for AttachmentDownloadView {
    fn from(value: DownloadOutcome) -> Self {
        Self {
            path: value.path.to_string_lossy().into_owned(),
            name: value.name,
            bytes: value.bytes,
            sha256: value.sha256,
            media_type: value.media_type,
            verified: value.verified,
        }
    }
}

/// State of one app-started attachment transfer. `state` is `running`,
/// `succeeded`, `failed` or `cancelled`. `error_code` is a stable mobile error
/// code and never carries Core text.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentTransferView {
    pub download_id: String,
    pub state: String,
    pub progress: AttachmentProgressView,
    pub download: Option<AttachmentDownloadView>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

/// Proof-backed outcome of following one submitted message. `Committed` names
/// the canonical message; the body stays in history.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MessageReplyView {
    Pending,
    Committed {
        reference: MessageReferenceView,
    },
    InputRequired {
        /// Stable lowercase kinds: `approval`, `question` or `other`.
        kinds: Vec<String>,
        operation: String,
        session: SessionKeyView,
    },
    Uncertain,
}

impl From<SendReply> for MessageReplyView {
    fn from(value: SendReply) -> Self {
        match value {
            SendReply::Pending => Self::Pending,
            SendReply::Committed { reference } => Self::Committed {
                reference: MessageReferenceView::from(&reference),
            },
            SendReply::InputRequired {
                kinds,
                operation,
                session,
            } => Self::InputRequired {
                kinds: kinds.into_iter().map(input_kind_name).collect(),
                operation,
                session: SessionKeyView::from(session),
            },
            SendReply::Uncertain => Self::Uncertain,
        }
    }
}

fn input_kind_name(value: InputKind) -> String {
    match value {
        InputKind::Approval => "approval",
        InputKind::Question => "question",
        InputKind::Other => "other",
    }
    .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ClientStatusView {
    pub config_path: String,
    pub data_dir: String,
    pub service: String,
    pub testing_environment: Option<String>,
    pub instance_id: Option<String>,
    pub account_state: String,
    pub remote_signout_pending: bool,
    pub default_host: Option<String>,
    pub device_access: String,
}

impl From<whalerein_core::api::ClientStatus> for ClientStatusView {
    fn from(value: whalerein_core::api::ClientStatus) -> Self {
        use whalerein_core::api::{AccountState, ClientService, DeviceAccess};
        Self {
            config_path: value.config.to_string_lossy().into_owned(),
            data_dir: value.data_dir.to_string_lossy().into_owned(),
            service: match value.service {
                ClientService::WhaleReinCloud => "whalerein_cloud",
                ClientService::Standalone => "standalone",
            }
            .into(),
            testing_environment: value.testing_environment.map(str::to_owned),
            instance_id: value.instance_id,
            account_state: match value.account_state {
                AccountState::NotApplicable => "not_applicable",
                AccountState::SignedIn => "signed_in",
                AccountState::SignedOut => "signed_out",
            }
            .into(),
            remote_signout_pending: value.remote_signout_pending,
            default_host: value.default_host,
            device_access: match value.device_access {
                DeviceAccess::Unpaired => "unpaired",
                DeviceAccess::Ready => "ready",
                DeviceAccess::RevocationPending => "revocation_pending",
                DeviceAccess::Revoked => "revoked",
                DeviceAccess::Unavailable => "unavailable",
                DeviceAccess::SignInRequired => "sign_in_required",
            }
            .into(),
        }
    }
}

/// Redacted diagnostics shown before/after an explicit share.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DiagnosticsReportView {
    /// Redacted JSON document. It never contains content, drafts, tool
    /// arguments, credentials, tokens, private keys or full invitation URIs.
    pub json: String,
    /// Fields the report promises to redact.
    pub redaction_fields: Vec<String>,
    /// Stable code of the most recent local failure, when one is retained.
    pub last_failure_code: Option<String>,
    pub app_version: String,
    pub core_version: String,
}

/// Delivery choice for `send`. The app decides which mode applies; `send`
/// maps to Core's start-if-idle behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DeliveryChoice {
    Steer,
    Queue,
    Send,
}

impl DeliveryChoice {
    pub(crate) fn to_core(self) -> DeliveryMode {
        match self {
            Self::Steer => DeliveryMode::Steer,
            Self::Queue => DeliveryMode::EnqueueAfterRun,
            Self::Send => DeliveryMode::StartIfIdle,
        }
    }
}

/// Full scoped identity of one live or committed Attempt.
///
/// `live_epoch` fences observations of the same session across watch
/// replacement; `attempt_id` is only unique inside that epoch. A committed
/// message carries the attempt that produced it, which can belong to a foreign
/// or already-ended attempt (for example a replayed or recovered message).
#[derive(Debug, Clone, PartialEq, Eq, Default, uniffi::Record)]
pub struct AttemptRefView {
    pub live_epoch: String,
    pub attempt_id: String,
}

impl From<&AttemptRef> for AttemptRefView {
    fn from(value: &AttemptRef) -> Self {
        Self {
            live_epoch: value.live_epoch.clone(),
            attempt_id: value.attempt_id.clone(),
        }
    }
}

/// Leading preview bytes Core replaced with a committed message, split by
/// kind so a hidden reasoning prefix never offsets answer rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Record)]
pub struct PreviewPrefixView {
    pub text_bytes: u64,
    pub reasoning_bytes: u64,
}

impl From<PreviewPrefix> for PreviewPrefixView {
    fn from(value: PreviewPrefix) -> Self {
        Self {
            text_bytes: value.text_bytes as u64,
            reasoning_bytes: value.reasoning_bytes as u64,
        }
    }
}

/// Ordered events produced by a running watch and by pairing.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ConversationEvent {
    State {
        /// Core's observation epoch for the watched session.
        live_epoch: String,
        run_phase: String,
        /// True while a turn is running or stopping.
        running: bool,
        revision: u64,
        approvals: Vec<ApprovalView>,
    },
    HistoryReset,
    Message {
        message: MessageView,
        /// The Attempt that produced this canonical message, when Core
        /// attributed one. A late or replayed commit can name another attempt.
        origin: Option<AttemptRefView>,
        /// Preview bytes already shown that this commit replaced.
        preview_prefix: Option<PreviewPrefixView>,
    },
    Preview {
        /// Full scoped attempt identity; never merge blocks across attempts.
        attempt: AttemptRefView,
        /// Stable block identity inside the attempt.
        block_id: String,
        /// The checked kind of the previewed block: `text` or `reasoning`.
        /// Callers must not merge reasoning into an answer block.
        kind: String,
        text: String,
        replace: bool,
        incomplete: bool,
    },
    /// One material change to a canonical tool activity. Core has already
    /// merged the call and result occurrences.
    ToolActivity {
        activity: ToolActivityView,
    },
    AttemptEnded {
        attempt: AttemptRefView,
        outcome: String,
    },
    Connection {
        host_id: String,
        status: String,
    },
    PairingPrompt {
        prompt: PairingPromptView,
    },
    PairingFinished {
        host_id: Option<String>,
        code: Option<String>,
    },
    CommandStatus {
        command: CommandView,
    },
    ApprovalChanged {
        approval: ApprovalView,
    },
    /// Core-owned question state for the active observation. Only `current`
    /// establishes that no pending questions exist.
    Questions {
        view: QuestionsView,
    },
    Error {
        code: String,
        message: String,
    },
    /// The bounded queue discarded this many older events before this poll.
    DroppedEvents {
        count: u64,
    },
    WatchEnded,
}

/// Translates one Core view update. `seen_approvals` remembers the last
/// version/digest per approval so a changed request can be flagged.
pub(crate) fn convert_update(
    update: &ViewUpdate,
    seen_approvals: &mut Option<HashMap<String, (i64, String)>>,
) -> Vec<ConversationEvent> {
    match update {
        ViewUpdate::State(state) => {
            let approvals: Vec<ApprovalView> = state
                .approvals
                .iter()
                .map(|approval| ApprovalView {
                    approval_id: approval.approval_id.clone(),
                    session_id: approval.session_id.clone(),
                    tool_name: approval.tool_name.clone(),
                    call_id: approval.call_id.clone(),
                    reason: approval.reason.clone(),
                    version: approval.version,
                    expires_at_unix_ms: approval.expires_at_unix_ms,
                    digest_hex: hex::encode(&approval.digest),
                    state: approval_state_name(approval.state),
                })
                .collect();
            let mut events = vec![ConversationEvent::State {
                live_epoch: state.live_epoch.clone(),
                run_phase: run_phase_name(state.run_phase),
                running: run_phase_running(state.run_phase),
                revision: state.revision as u64,
                approvals: approvals.clone(),
            }];
            let mut next = HashMap::with_capacity(approvals.len());
            for approval in &approvals {
                let fingerprint = (approval.version, approval.digest_hex.clone());
                let changed = seen_approvals
                    .as_ref()
                    .is_some_and(|seen| seen.get(&approval.approval_id) != Some(&fingerprint));
                if changed {
                    events.push(ConversationEvent::ApprovalChanged {
                        approval: approval.clone(),
                    });
                }
                next.insert(approval.approval_id.clone(), fingerprint);
            }
            *seen_approvals = Some(next);
            events
        }
        ViewUpdate::Reset => vec![ConversationEvent::HistoryReset],
        ViewUpdate::Message {
            message,
            origin,
            preview_prefix,
        } => vec![ConversationEvent::Message {
            message: message_view(message),
            origin: origin.as_ref().map(AttemptRefView::from),
            preview_prefix: preview_prefix.map(PreviewPrefixView::from),
        }],
        ViewUpdate::Preview {
            attempt,
            block_id,
            kind,
            text,
            replace,
            incomplete,
        } => vec![ConversationEvent::Preview {
            attempt: AttemptRefView::from(attempt),
            block_id: block_id.clone(),
            kind: block_kind_name(*kind),
            text: text.clone(),
            replace: *replace,
            incomplete: *incomplete,
        }],
        ViewUpdate::ToolActivity(activity) => vec![ConversationEvent::ToolActivity {
            activity: ToolActivityView::from(activity),
        }],
        ViewUpdate::AttemptEnded { attempt, outcome } => vec![ConversationEvent::AttemptEnded {
            attempt: AttemptRefView::from(attempt),
            outcome: attempt_outcome_name(*outcome),
        }],
    }
}

// ---------------------------------------------------------------------------
// Pending questions
//
// These are Core-owned projections. A client never parses native or wire
// structures: the stamp, the batch guard and the freshness label are already
// decided by Core observation. `QuestionBatchView` carries the exact guard a
// submission must revalidate, so an observed card alone never authorizes an
// answer.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionOptionView {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionSubjectView {
    pub name: String,
    pub icon: Option<String>,
}

/// Normalized native intent. Translation maps are keyed by locale tag.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionIntentView {
    pub kind: String,
    pub approve: String,
    pub reject: Option<String>,
    pub always: Option<String>,
    pub call_id: Option<String>,
    pub subject: Option<QuestionSubjectView>,
    pub display_question: Option<HashMap<String, String>>,
    pub display_detail: Option<HashMap<String, String>>,
    pub display_approve: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionItemView {
    pub id: String,
    pub question: String,
    pub detail: Option<String>,
    pub header: Option<String>,
    pub options: Vec<QuestionOptionView>,
    pub multi_select: bool,
    pub intent: Option<QuestionIntentView>,
}

/// One pending batch with the exact guards a submission must echo back.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionBatchView {
    pub session_id: String,
    pub batch_id: String,
    pub source_epoch: String,
    pub version: u64,
    pub digest_hex: String,
    pub phase: String,
    pub questions: Vec<QuestionItemView>,
    pub answerable: bool,
    pub content_unavailable: bool,
}

/// Source observation epoch plus monotonic normalized-inventory revision.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionStampView {
    pub observation_epoch: String,
    pub revision: u64,
}

/// One authoritative Core question view. `freshness` is a stable lowercase
/// label: `loading`, `current`, `stale`, `unavailable` or `unsupported`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionsView {
    pub freshness: String,
    pub stamp: Option<QuestionStampView>,
    pub batches: Vec<QuestionBatchView>,
    pub error: Option<String>,
    /// True when the complete Core view was too large to copy into the mobile
    /// event queue. `batches` is then empty and `batch_count` is authoritative;
    /// the UI must read pages on demand and never render this as empty.
    pub truncated_for_display: bool,
    /// Number of batches in the Core-owned view, even when the content is not
    /// inlined. Zero only when the view genuinely has no batches.
    pub batch_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionPageView {
    pub batches: Vec<QuestionBatchView>,
    pub next_cursor: String,
    pub stamp: Option<QuestionStampView>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionAnswerView {
    pub id: String,
    pub selected: Vec<String>,
    pub custom: Option<String>,
}

/// Receipt for asking Core to cancel one in-flight open answer.
///
/// `cancellation_requested == false` never means the answer did not happen: it
/// may have completed already or not yet reached the Host. `command_id` keeps
/// the original attempt identity so the caller can recover its native result.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionCancelReceiptView {
    pub command_id: String,
    pub cancellation_requested: bool,
}

impl From<QuestionAnswerView> for core_api::QuestionAnswer {
    fn from(value: QuestionAnswerView) -> Self {
        Self {
            id: value.id,
            selected: value.selected,
            custom: value.custom,
        }
    }
}

pub(crate) fn question_freshness_name(value: QuestionFreshness) -> String {
    match value {
        QuestionFreshness::Loading => "loading",
        QuestionFreshness::Current => "current",
        QuestionFreshness::Stale => "stale",
        QuestionFreshness::Unavailable => "unavailable",
        QuestionFreshness::Unsupported => "unsupported",
    }
    .to_owned()
}

fn question_stamp_view(value: &core_api::QuestionStamp) -> QuestionStampView {
    QuestionStampView {
        observation_epoch: value.observation_epoch.clone(),
        revision: value.revision,
    }
}

pub(crate) fn question_batch_view(value: &QuestionBatchInfo) -> QuestionBatchView {
    QuestionBatchView {
        session_id: value.session_id.clone(),
        batch_id: value.batch_id.clone(),
        source_epoch: value.source_epoch.clone(),
        version: value.version,
        digest_hex: hex::encode(&value.digest),
        phase: value.phase.clone(),
        questions: value
            .questions
            .iter()
            .map(|item| QuestionItemView {
                id: item.id.clone(),
                question: item.question.clone(),
                detail: item.detail.clone(),
                header: item.header.clone(),
                options: item
                    .options
                    .iter()
                    .map(|option| QuestionOptionView {
                        label: option.label.clone(),
                        description: option.description.clone(),
                    })
                    .collect(),
                multi_select: item.multi_select,
                intent: item.intent.as_ref().map(|intent| QuestionIntentView {
                    kind: intent.kind.clone(),
                    approve: intent.approve.clone(),
                    reject: intent.reject.clone(),
                    always: intent.always.clone(),
                    call_id: intent.call_id.clone(),
                    subject: intent.subject.as_ref().map(|subject| QuestionSubjectView {
                        name: subject.name.clone(),
                        icon: subject.icon.clone(),
                    }),
                    display_question: intent
                        .display
                        .as_ref()
                        .and_then(|display| display.question.clone()),
                    display_detail: intent
                        .display
                        .as_ref()
                        .and_then(|display| display.detail.clone()),
                    display_approve: intent
                        .display
                        .as_ref()
                        .and_then(|display| display.approve.clone()),
                }),
            })
            .collect(),
        answerable: value.answerable,
        content_unavailable: value.content_unavailable,
    }
}

pub(crate) fn questions_view(value: &core_api::QuestionView) -> QuestionsView {
    QuestionsView {
        freshness: question_freshness_name(value.freshness),
        stamp: value.stamp.as_ref().map(question_stamp_view),
        batches: value.batches.iter().map(question_batch_view).collect(),
        error: value.error.clone(),
        truncated_for_display: false,
        batch_count: value.batches.len() as u64,
    }
}

/// Approximate encoded/UniFFI-visible size of one question view. It counts
/// UTF-8 bytes plus fixed per-record overhead and is intentionally conservative;
/// the exact wire accounting lives in Core.
pub(crate) const MAX_INLINE_QUESTION_VIEW_BYTES: usize = 1024 * 1024;

fn string_bytes(value: &str) -> usize {
    value.len()
}

fn option_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, |text| string_bytes(text))
}

fn translations_bytes(value: &Option<std::collections::HashMap<String, String>>) -> usize {
    value.as_ref().map_or(0, |translations| {
        translations
            .iter()
            .map(|(key, text)| key.len() + text.len())
            .sum()
    })
}

fn question_batch_bytes(batch: &QuestionBatchView) -> usize {
    let mut bytes = 128
        + string_bytes(&batch.session_id)
        + string_bytes(&batch.batch_id)
        + string_bytes(&batch.source_epoch)
        + batch.digest_hex.len()
        + string_bytes(&batch.phase);
    for item in &batch.questions {
        bytes += 64
            + string_bytes(&item.id)
            + string_bytes(&item.question)
            + option_bytes(&item.detail)
            + option_bytes(&item.header);
        for option in &item.options {
            bytes += 24 + string_bytes(&option.label) + option_bytes(&option.description);
        }
        if let Some(intent) = &item.intent {
            bytes += 96
                + string_bytes(&intent.kind)
                + string_bytes(&intent.approve)
                + option_bytes(&intent.reject)
                + option_bytes(&intent.always)
                + option_bytes(&intent.call_id)
                + intent
                    .subject
                    .as_ref()
                    .map_or(0, |subject| string_bytes(&subject.name))
                + translations_bytes(&intent.display_question)
                + translations_bytes(&intent.display_detail)
                + translations_bytes(&intent.display_approve);
        }
    }
    bytes
}

pub(crate) fn questions_view_bytes(view: &QuestionsView) -> usize {
    64 + option_bytes(&view.error) + view.batches.iter().map(question_batch_bytes).sum::<usize>()
}

/// Keep the event queue bounded by bytes, not only by event count. A complete
/// view above the inline budget is replaced by a bounded summary so a large
/// collection cannot be copied hundreds of times into the 512-entry queue.
pub(crate) fn bounded_question_view(mut view: QuestionsView) -> QuestionsView {
    view.batch_count = view.batches.len() as u64;
    if questions_view_bytes(&view) > MAX_INLINE_QUESTION_VIEW_BYTES {
        view.batches.clear();
        view.truncated_for_display = true;
        view.error = Some("question_view_too_large_for_display".into());
    }
    view
}

pub(crate) fn question_page_view(value: &QuestionPageInfo) -> QuestionPageView {
    QuestionPageView {
        batches: value.batches.iter().map(question_batch_view).collect(),
        next_cursor: value.next_cursor.clone(),
        stamp: value.stamp.as_ref().map(question_stamp_view),
        complete: value.complete,
    }
}

/// Rebuild the Core guard value from exactly what the client reviewed. Only the
/// guard fields are used for submission; content is never re-sent to Core.
pub(crate) fn question_guard(value: &QuestionBatchView) -> core_api::QuestionBatchInfo {
    QuestionBatchInfo {
        session_id: value.session_id.clone(),
        batch_id: value.batch_id.clone(),
        source_epoch: value.source_epoch.clone(),
        version: value.version,
        digest: hex::decode(&value.digest_hex).unwrap_or_default(),
        phase: value.phase.clone(),
        questions: Vec::new(),
        answerable: value.answerable,
        content_unavailable: value.content_unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_core::conversation::content_block::{
        AttachmentContent, NoticeContent, ReasoningContent, TextContent,
    };
    use whalerein_core::conversation::{
        Approval, ApprovalDecision, AttemptRef, ContentBlock, SessionKey, SessionState, ToolOutcome,
    };

    fn message_with(
        role: MessageRole,
        content: Option<content_block::Content>,
    ) -> ConversationMessage {
        ConversationMessage {
            message_id: "m-1".into(),
            role,
            blocks: vec![ContentBlock {
                block_id: "b-1".into(),
                content,
            }],
            authored_at_unix_ms: 7,
        }
    }

    fn approval(state: ApprovalState, version: i64) -> Approval {
        Approval {
            approval_id: "a-1".into(),
            session_id: "s-1".into(),
            tool_name: "shell".into(),
            call_id: "c-1".into(),
            reason: "run the requested command".into(),
            digest: vec![0xab; 32],
            version,
            expires_at_unix_ms: 1,
            state,
            decision: ApprovalDecision::Unspecified,
        }
    }

    fn state(approvals: Vec<Approval>, run_phase: RunPhase) -> SessionState {
        SessionState {
            session: Some(SessionKey {
                host_id: "h".into(),
                session_id: "s".into(),
            }),
            live_epoch: "epoch".into(),
            revision: 3,
            history_head: "head".into(),
            projection_id: "projection-1".into(),
            run_phase,
            approvals,
        }
    }

    fn tool_activity(state: ToolState, result: Vec<ToolResultPart>) -> ToolActivity {
        ToolActivity {
            id: whalerein_core::conversation::ToolActivityId {
                call_id: "c".into(),
            },
            name: "shell".into(),
            summary: Some("ls".into()),
            order: 1,
            state,
            stages: vec![ToolStage::Execution, ToolStage::Settlement],
            progress: None,
            failure_code: None,
            result,
            outcome: ToolOutcome::Succeeded,
            attempt: None,
            call_missing: false,
        }
    }

    #[test]
    fn unknown_role_and_missing_block_content_are_total() {
        let message = message_with(MessageRole::try_from(999).unwrap_or_default(), None);
        let view = MessageView::from(&message);
        assert_eq!(view.role, "unknown");
        assert_eq!(view.message_id, "m-1");
        assert_eq!(view.created_at_unix_ms, 7);
        assert_eq!(view.blocks.len(), 1);
        assert_eq!(view.blocks[0].block_id, "b-1");
        assert_eq!(view.blocks[0].kind, "unknown");
        assert!(view.blocks[0].text.is_empty());
        assert!(view.blocks[0].tool_name.is_none());
        assert!(view.blocks[0].tool_activity.is_none());
        assert!(view.blocks[0].notice.is_none());
        assert_eq!(
            message_role_name(MessageRole::try_from(-5).unwrap_or_default()),
            "unknown"
        );
        // Context and explicit unknown roles never become user content.
        assert_eq!(message_role_name(MessageRole::Context), "context");
        assert_eq!(message_role_name(MessageRole::Unknown), "unknown");
        assert!(!MessageRole::Context.is_user());
        assert!(!MessageRole::Unknown.is_user());
    }

    #[test]
    fn block_kinds_map_without_panicking() {
        let reasoning = block_view(
            "b",
            Some(&content_block::Content::Reasoning(ReasoningContent {
                text: "thinking".into(),
            })),
        );
        assert_eq!(reasoning.kind, "reasoning");
        assert_eq!(reasoning.text, "thinking");
        assert!(reasoning.tool_activity.is_none());
        assert!(reasoning.notice.is_none());

        let notice = block_view(
            "b",
            Some(&content_block::Content::Notice(NoticeContent {
                category: NoticeCategory::Retry,
                state: NoticeState::Started,
                summary: Some("retrying".into()),
            })),
        );
        assert_eq!(notice.kind, "notice");
        assert_eq!(notice.text, "retrying");
        let typed_notice = notice.notice.expect("typed notice");
        assert_eq!(typed_notice.category, "retry");
        assert_eq!(typed_notice.state, "started");
        assert_eq!(typed_notice.summary.as_deref(), Some("retrying"));

        let missing_summary = block_view(
            "b",
            Some(&content_block::Content::Notice(NoticeContent {
                category: NoticeCategory::Compaction,
                state: NoticeState::Completed,
                summary: None,
            })),
        );
        assert_eq!(missing_summary.kind, "notice");
        assert!(missing_summary.text.is_empty());
        assert_eq!(
            missing_summary
                .notice
                .as_ref()
                .map(|notice| notice.category.as_str()),
            Some("compaction")
        );

        let text = block_view(
            "b",
            Some(&content_block::Content::Text(TextContent {
                text: "hello".into(),
            })),
        );
        assert_eq!(text.kind, "text");
        assert_eq!(text.text, "hello");

        let attachment = block_view(
            "b",
            Some(&content_block::Content::Attachment(AttachmentContent {
                media_type: "text/plain".into(),
                name: "file.txt".into(),
                size: 3,
                available: true,
                attachment_ref: "ref".into(),
            })),
        );
        assert_eq!(attachment.kind, "attachment");
        assert_eq!(attachment.text, "file.txt");
        assert!(attachment.tool_activity.is_none());
        assert!(attachment.notice.is_none());
    }

    #[test]
    fn typed_tool_activity_keeps_merge_and_settlement_fields() {
        let result_first = content_block::Content::ToolActivity(tool_activity(
            ToolState::Succeeded,
            vec![
                ToolResultPart::Text("out".into()),
                ToolResultPart::Attachment(AttachmentContent {
                    media_type: "text/plain".into(),
                    name: "out.txt".into(),
                    size: 3,
                    available: true,
                    attachment_ref: "ref".into(),
                }),
            ],
        ));
        let view = block_view("b", Some(&result_first));
        assert_eq!(view.kind, "tool");
        assert_eq!(view.text, "out\nout.txt");
        assert_eq!(view.tool_name.as_deref(), Some("shell"));
        let activity = view.tool_activity.expect("typed activity");
        assert_eq!(activity.call_id, "c");
        assert_eq!(activity.state, "succeeded");
        assert_eq!(activity.stages, vec!["execution", "settlement"]);
        assert!(!activity.call_missing);
        assert_eq!(activity.result.len(), 2);
        assert_eq!(
            activity.result[0],
            ToolResultPartView::Text { text: "out".into() }
        );

        let update = ViewUpdate::ToolActivity(tool_activity(ToolState::Running, Vec::new()));
        let mut seen = None;
        assert_eq!(
            convert_update(&update, &mut seen),
            vec![ConversationEvent::ToolActivity {
                activity: ToolActivityView {
                    call_id: "c".into(),
                    name: "shell".into(),
                    summary: Some("ls".into()),
                    state: "running".into(),
                    stages: vec!["execution".into(), "settlement".into()],
                    progress: None,
                    failure_code: None,
                    result: Vec::new(),
                    call_missing: false,
                }
            }]
        );
    }

    #[test]
    fn state_and_attempt_unknown_enum_values_are_total() {
        let update = ViewUpdate::State(state(
            vec![approval(
                ApprovalState::try_from(999).unwrap_or_default(),
                1,
            )],
            RunPhase::try_from(999).unwrap_or_default(),
        ));
        let mut seen = None;
        let events = convert_update(&update, &mut seen);
        match &events[0] {
            ConversationEvent::State {
                live_epoch,
                run_phase,
                running,
                revision,
                approvals,
            } => {
                assert_eq!(live_epoch, "epoch");
                assert_eq!(run_phase, "unknown");
                assert!(!running);
                assert_eq!(*revision, 3);
                assert_eq!(approvals.len(), 1);
                assert_eq!(approvals[0].state, "unknown");
                assert_eq!(approvals[0].digest_hex, "ab".repeat(32));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let update = ViewUpdate::AttemptEnded {
            attempt: AttemptRef {
                live_epoch: "e".into(),
                attempt_id: "a".into(),
            },
            outcome: AttemptOutcome::try_from(4242).unwrap_or_default(),
        };
        let events = convert_update(&update, &mut seen);
        assert_eq!(
            events,
            vec![ConversationEvent::AttemptEnded {
                attempt: AttemptRefView {
                    live_epoch: "e".into(),
                    attempt_id: "a".into(),
                },
                outcome: "unknown".into()
            }]
        );
        assert_eq!(
            convert_update(&ViewUpdate::Reset, &mut seen),
            vec![ConversationEvent::HistoryReset]
        );
    }

    #[test]
    fn conversion_is_deterministic_and_approval_changes_are_flagged_once() {
        let mut seen = None;
        let first = state(vec![approval(ApprovalState::Pending, 4)], RunPhase::Running);
        let update = ViewUpdate::State(first.clone());
        let events = convert_update(&update, &mut seen);
        assert_eq!(events.len(), 1, "baseline does not flag a change");
        assert_eq!(events, convert_update(&update, &mut seen));

        // A newer revision of the same approval is flagged.
        let mut changed = first.clone();
        changed.revision = 5;
        changed.approvals = vec![approval(ApprovalState::Pending, 5)];
        let events = convert_update(&ViewUpdate::State(changed), &mut seen);
        assert!(matches!(
            events.as_slice(),
            [
                ConversationEvent::State { .. },
                ConversationEvent::ApprovalChanged { approval }
            ] if approval.version == 5
        ));
        // Repeating the same state does not duplicate the change event.
        let mut repeated = first;
        repeated.revision = 6;
        repeated.approvals = vec![approval(ApprovalState::Pending, 5)];
        let events = convert_update(&ViewUpdate::State(repeated), &mut seen);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn message_and_preview_conversions_keep_ids_and_flags() {
        let message = message_with(
            MessageRole::User,
            Some(content_block::Content::Text(TextContent {
                text: "hi".into(),
            })),
        );
        let update = ViewUpdate::Message {
            message: message.clone(),
            origin: None,
            preview_prefix: None,
        };
        let mut seen = None;
        assert_eq!(
            convert_update(&update, &mut seen),
            vec![ConversationEvent::Message {
                message: MessageView {
                    message_id: "m-1".into(),
                    role: "user".into(),
                    blocks: vec![BlockView {
                        block_id: "b-1".into(),
                        kind: "text".into(),
                        text: "hi".into(),
                        tool_name: None,
                        tool_activity: None,
                        notice: None,
                        attachment: None,
                    }],
                    created_at_unix_ms: 7,
                },
                origin: None,
                preview_prefix: None,
            }]
        );

        let update = ViewUpdate::Preview {
            attempt: AttemptRef {
                live_epoch: "e".into(),
                attempt_id: "a".into(),
            },
            block_id: "b".into(),
            kind: BlockKind::Reasoning,
            text: "stream".into(),
            replace: true,
            incomplete: true,
        };
        assert_eq!(
            convert_update(&update, &mut seen),
            vec![ConversationEvent::Preview {
                attempt: AttemptRefView {
                    live_epoch: "e".into(),
                    attempt_id: "a".into(),
                },
                block_id: "b".into(),
                kind: "reasoning".into(),
                text: "stream".into(),
                replace: true,
                incomplete: true,
            }]
        );
    }

    #[test]
    fn cached_history_and_live_message_projection_are_identical() {
        let mut message = message_with(
            MessageRole::Assistant,
            Some(content_block::Content::Text(TextContent {
                text: "answer".into(),
            })),
        );
        message.blocks.push(ContentBlock {
            block_id: "b-2".into(),
            content: Some(content_block::Content::ToolActivity(tool_activity(
                ToolState::Succeeded,
                vec![ToolResultPart::Text("ls output".into())],
            ))),
        });
        let direct = message_view(&message);
        assert_eq!(MessageView::from(&message), direct);
        let mut seen = None;
        let events = convert_update(
            &ViewUpdate::Message {
                message: message.clone(),
                origin: None,
                preview_prefix: None,
            },
            &mut seen,
        );
        match &events[0] {
            ConversationEvent::Message {
                message, origin, ..
            } => {
                assert_eq!(message, &direct);
                assert!(origin.is_none());
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(direct.role, "assistant");
        assert_eq!(direct.blocks.len(), 2);
        assert_eq!(direct.blocks[1].kind, "tool");
        assert_eq!(direct.blocks[1].tool_name.as_deref(), Some("shell"));
        assert_eq!(
            direct.blocks[1]
                .tool_activity
                .as_ref()
                .map(|activity| activity.state.as_str()),
            Some("succeeded")
        );
    }

    #[test]
    fn message_reference_view_round_trips_the_exact_scoped_reference() {
        let session = SessionKey {
            host_id: "h".into(),
            session_id: "s".into(),
        };
        let reference =
            MessageReference::new(session.clone(), "message-1", "locator-1", "projection-1")
                .expect("reference");
        let view = MessageReferenceView::from(&reference);
        assert_eq!(view.source_locator, "locator-1");
        assert_eq!(view.message_id, "message-1");
        let rebuilt = MessageReference::new(
            SessionKey {
                host_id: view.session.host_id.clone(),
                session_id: view.session.session_id.clone(),
            },
            view.message_id.clone(),
            view.source_locator.clone(),
            view.projection_id.clone(),
        )
        .expect("rebuilt reference");
        assert_eq!(rebuilt, reference);
        assert_eq!(rebuilt.session(), &session);
        assert_eq!(rebuilt.source_locator(), "locator-1");
    }

    #[test]
    fn send_reply_states_map_to_the_typed_surface() {
        let session = SessionKey {
            host_id: "h".into(),
            session_id: "s".into(),
        };
        let reference =
            MessageReference::new(session.clone(), "message-1", "locator-1", "projection-1")
                .expect("reference");
        let committed: MessageReplyView = SendReply::Committed { reference }.into();
        assert_eq!(
            committed,
            MessageReplyView::Committed {
                reference: MessageReferenceView {
                    session: SessionKeyView {
                        host_id: "h".into(),
                        session_id: "s".into(),
                    },
                    message_id: "message-1".into(),
                    source_locator: "locator-1".into(),
                    projection_id: "projection-1".into(),
                }
            }
        );

        assert_eq!(
            MessageReplyView::from(SendReply::Pending),
            MessageReplyView::Pending
        );
        assert_eq!(
            MessageReplyView::from(SendReply::Uncertain),
            MessageReplyView::Uncertain
        );
        assert_eq!(
            MessageReplyView::from(SendReply::InputRequired {
                kinds: vec![InputKind::Approval, InputKind::Question, InputKind::Other],
                operation: "operation-1".into(),
                session,
            }),
            MessageReplyView::InputRequired {
                kinds: vec!["approval".into(), "question".into(), "other".into()],
                operation: "operation-1".into(),
                session: SessionKeyView {
                    host_id: "h".into(),
                    session_id: "s".into(),
                },
            }
        );
    }

    #[test]
    fn approval_info_digest_and_state_are_projected() {
        let info = ApprovalInfo {
            approval_id: "a".into(),
            session_id: "s".into(),
            tool_name: "shell".into(),
            call_id: "c".into(),
            reason: "why".into(),
            digest: vec![0xcd; 32],
            version: 9,
            expires_at_unix_ms: 11,
            state: "Pending".into(),
            decision: "Unspecified".into(),
        };
        let view = ApprovalView::from(info);
        assert_eq!(view.digest_hex, "cd".repeat(32));
        assert_eq!(view.state, "pending");
        assert_eq!(view.version, 9);
    }

    #[test]
    fn summaries_keep_identity_and_ordering_fields() {
        let host = HostSummary::from(HostInfo {
            host_id: "h".into(),
            online: true,
            protocol_version: 1,
            fingerprint: "f".into(),
        });
        assert_eq!(host.host_id, "h");
        assert!(host.online);

        let session = SessionSummaryView::from(SessionSummary {
            session_id: "s".into(),
            title: "title".into(),
            cwd: "/tmp".into(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            running: true,
            agent_available: true,
            blank: false,
            parent_session_id: String::new(),
            origin: "cli".into(),
            history_cursor: "c".into(),
            source_capabilities: Some(SessionCapabilities {
                history: true,
                resumable_history: false,
                live_text: true,
                interrupt: true,
                approvals: true,
                live_reasoning: true,
                tool_activity: true,
                conversation_notices: true,
                attachments: true,
                history_page: true,
                history_exact: true,
                send_followup: true,
                delivery_modes: vec![
                    "STEER".into(),
                    "ENQUEUE_AFTER_RUN".into(),
                    "START_IF_IDLE".into(),
                ],
                model_selection: true,
                pending_queue_read: true,
                pending_queue_edit: false,
                pending_queue_remove: false,
                pending_queue_steer: true,
                questions_read: true,
                questions_answer_open: false,
                questions_answer_continued: false,
                session_fork: true,
            }),
            queued_prompts: 0,
        });
        assert_eq!(session.updated_at_unix_ms, 2);
        assert_eq!(session.session_id, "s");
        assert!(session.running);
        let capabilities = session.capabilities.expect("capabilities");
        assert!(capabilities.history);
        assert!(capabilities.live_text);
        assert!(capabilities.interrupt);
        assert!(capabilities.approvals);
        assert!(capabilities.model_selection);
        assert!(capabilities.pending_queue_read);
        assert!(!capabilities.pending_queue_edit);
        assert!(capabilities.pending_queue_steer);
        assert!(capabilities.questions_read);
        assert!(!capabilities.questions_answer_open);
        assert!(capabilities.session_fork);
        assert_eq!(
            capabilities.delivery_modes,
            vec!["steer", "enqueue_after_run", "start_if_idle"]
        );
    }

    #[test]
    fn capability_mapping_is_complete_and_total() {
        let source = SessionCapabilities {
            history: true,
            resumable_history: true,
            live_text: true,
            interrupt: true,
            approvals: true,
            live_reasoning: true,
            tool_activity: true,
            conversation_notices: true,
            attachments: true,
            history_page: true,
            history_exact: true,
            send_followup: true,
            delivery_modes: vec![
                "STEER".into(),
                "DELIVERY_MODE_ENQUEUE_AFTER_RUN".into(),
                "start_if_idle".into(),
                "FUTURE_MODE".into(),
            ],
            model_selection: true,
            pending_queue_read: true,
            pending_queue_edit: true,
            pending_queue_remove: true,
            pending_queue_steer: true,
            questions_read: true,
            questions_answer_open: true,
            questions_answer_continued: true,
            session_fork: true,
        };
        let view = SessionCapabilitiesView::from(source);
        assert!(view.history);
        assert!(view.resumable_history);
        assert!(view.live_text);
        assert!(view.interrupt);
        assert!(view.approvals);
        assert!(view.live_reasoning);
        assert!(view.tool_activity);
        assert!(view.conversation_notices);
        assert!(view.attachments);
        assert!(view.history_page);
        assert!(view.history_exact);
        assert!(view.send_followup);
        assert!(view.model_selection);
        assert!(view.pending_queue_read);
        assert!(view.pending_queue_edit);
        assert!(view.pending_queue_remove);
        assert!(view.pending_queue_steer);
        assert!(view.questions_read);
        assert!(view.questions_answer_open);
        assert!(view.questions_answer_continued);
        assert!(view.session_fork);
        assert_eq!(
            view.delivery_modes,
            vec!["steer", "enqueue_after_run", "start_if_idle", "future_mode"]
        );

        let empty = SessionCapabilitiesView::from(SessionCapabilities {
            history: false,
            resumable_history: false,
            live_text: false,
            interrupt: false,
            approvals: false,
            live_reasoning: false,
            tool_activity: false,
            conversation_notices: false,
            attachments: false,
            history_page: false,
            history_exact: false,
            send_followup: false,
            delivery_modes: Vec::new(),
            model_selection: false,
            pending_queue_read: false,
            pending_queue_edit: false,
            pending_queue_remove: false,
            pending_queue_steer: false,
            questions_read: false,
            questions_answer_open: false,
            questions_answer_continued: false,
            session_fork: false,
        });
        assert!(empty.delivery_modes.is_empty());
        assert!(!empty.interrupt);
    }

    #[test]
    fn running_derives_from_the_run_phase_without_panicking() {
        let cases = [
            (1, "idle", false),
            (2, "running", true),
            (3, "stopping", true),
            (4, "source_unavailable", false),
            (0, "unknown", false),
            (4242, "unknown", false),
        ];
        for (phase, name, running) in cases {
            let mut seen = None;
            let phase = RunPhase::try_from(phase).unwrap_or_default();
            let events = convert_update(&ViewUpdate::State(state(Vec::new(), phase)), &mut seen);
            match &events[0] {
                ConversationEvent::State {
                    run_phase,
                    running: actual,
                    ..
                } => {
                    assert_eq!(run_phase, name);
                    assert_eq!(*actual, running);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[test]
    fn a_large_question_view_is_replaced_by_a_bounded_summary() {
        let view = QuestionsView {
            freshness: "current".into(),
            stamp: Some(QuestionStampView {
                observation_epoch: "epoch-1".into(),
                revision: 3,
            }),
            batches: vec![QuestionBatchView {
                session_id: "session-1".into(),
                batch_id: "batch-1".into(),
                source_epoch: "epoch-1".into(),
                version: 3,
                digest_hex: "00".repeat(32),
                phase: "QUESTION_OPEN".into(),
                questions: vec![QuestionItemView {
                    id: "q1".into(),
                    question: "x".repeat(MAX_INLINE_QUESTION_VIEW_BYTES + 1),
                    detail: None,
                    header: None,
                    options: Vec::new(),
                    multi_select: false,
                    intent: None,
                }],
                answerable: true,
                content_unavailable: false,
            }],
            error: None,
            truncated_for_display: false,
            batch_count: 0,
        };
        let bounded = bounded_question_view(view);
        assert!(bounded.truncated_for_display);
        assert_eq!(bounded.batch_count, 1);
        assert!(bounded.batches.is_empty());
        assert_eq!(
            bounded.error.as_deref(),
            Some("question_view_too_large_for_display")
        );
        assert_eq!(bounded.freshness, "current");
    }
    #[test]
    fn preview_append_and_replace_keep_attempt_and_block_identity() {
        let attempt = AttemptRef {
            live_epoch: "epoch-1".into(),
            attempt_id: "attempt-1".into(),
        };
        let mut seen = None;
        let append = ViewUpdate::Preview {
            attempt: attempt.clone(),
            block_id: "block-1".into(),
            kind: BlockKind::Text,
            text: "hel".into(),
            replace: false,
            incomplete: true,
        };
        let replace = ViewUpdate::Preview {
            attempt: attempt.clone(),
            block_id: "block-1".into(),
            kind: BlockKind::Text,
            text: "hello".into(),
            replace: true,
            incomplete: false,
        };
        assert_eq!(
            convert_update(&append, &mut seen),
            vec![ConversationEvent::Preview {
                attempt: AttemptRefView {
                    live_epoch: "epoch-1".into(),
                    attempt_id: "attempt-1".into(),
                },
                block_id: "block-1".into(),
                kind: "text".into(),
                text: "hel".into(),
                replace: false,
                incomplete: true,
            }]
        );
        assert_eq!(
            convert_update(&replace, &mut seen),
            vec![ConversationEvent::Preview {
                attempt: AttemptRefView {
                    live_epoch: "epoch-1".into(),
                    attempt_id: "attempt-1".into(),
                },
                block_id: "block-1".into(),
                kind: "text".into(),
                text: "hello".into(),
                replace: true,
                incomplete: false,
            }]
        );
    }

    #[test]
    fn reasoning_and_answer_previews_keep_separate_block_identities() {
        let attempt = AttemptRef {
            live_epoch: "epoch-1".into(),
            attempt_id: "attempt-1".into(),
        };
        let mut seen = None;
        for (block_id, kind, name) in [
            ("reasoning-1", BlockKind::Reasoning, "reasoning"),
            ("answer-1", BlockKind::Text, "text"),
        ] {
            let events = convert_update(
                &ViewUpdate::Preview {
                    attempt: attempt.clone(),
                    block_id: block_id.into(),
                    kind,
                    text: "stream".into(),
                    replace: false,
                    incomplete: true,
                },
                &mut seen,
            );
            match &events[0] {
                ConversationEvent::Preview {
                    block_id: actual,
                    kind: actual_kind,
                    ..
                } => {
                    assert_eq!(actual, block_id);
                    assert_eq!(actual_kind, name);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[test]
    fn late_commit_names_origin_and_preview_prefix_without_guessing() {
        let message = message_with(
            MessageRole::Assistant,
            Some(content_block::Content::Text(TextContent {
                text: "answer".into(),
            })),
        );
        let mut seen = None;
        let events = convert_update(
            &ViewUpdate::Message {
                message,
                origin: Some(AttemptRef {
                    live_epoch: "epoch-1".into(),
                    attempt_id: "attempt-1".into(),
                }),
                preview_prefix: Some(PreviewPrefix {
                    text_bytes: 4,
                    reasoning_bytes: 2,
                }),
            },
            &mut seen,
        );
        match &events[0] {
            ConversationEvent::Message {
                origin,
                preview_prefix,
                ..
            } => {
                assert_eq!(
                    origin,
                    &Some(AttemptRefView {
                        live_epoch: "epoch-1".into(),
                        attempt_id: "attempt-1".into(),
                    })
                );
                assert_eq!(
                    preview_prefix,
                    &Some(PreviewPrefixView {
                        text_bytes: 4,
                        reasoning_bytes: 2,
                    })
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn foreign_origin_is_preserved_as_evidence() {
        let message = message_with(
            MessageRole::Assistant,
            Some(content_block::Content::Text(TextContent {
                text: "replayed".into(),
            })),
        );
        let mut seen = None;
        let events = convert_update(
            &ViewUpdate::Message {
                message,
                origin: Some(AttemptRef {
                    live_epoch: "foreign-epoch".into(),
                    attempt_id: "foreign-attempt".into(),
                }),
                preview_prefix: None,
            },
            &mut seen,
        );
        match &events[0] {
            ConversationEvent::Message { origin, .. } => assert_eq!(
                origin,
                &Some(AttemptRefView {
                    live_epoch: "foreign-epoch".into(),
                    attempt_id: "foreign-attempt".into(),
                })
            ),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn an_attempt_end_before_its_message_keeps_both_identities() {
        let attempt = AttemptRef {
            live_epoch: "epoch-1".into(),
            attempt_id: "attempt-1".into(),
        };
        let mut seen = None;
        let ended = convert_update(
            &ViewUpdate::AttemptEnded {
                attempt: attempt.clone(),
                outcome: AttemptOutcome::Committed,
            },
            &mut seen,
        );
        let committed = convert_update(
            &ViewUpdate::Message {
                message: message_with(
                    MessageRole::Assistant,
                    Some(content_block::Content::Text(TextContent {
                        text: "final".into(),
                    })),
                ),
                origin: Some(attempt.clone()),
                preview_prefix: None,
            },
            &mut seen,
        );
        match (&ended[0], &committed[0]) {
            (
                ConversationEvent::AttemptEnded {
                    attempt: ended_attempt,
                    outcome,
                },
                ConversationEvent::Message {
                    origin: Some(origin),
                    ..
                },
            ) => {
                assert_eq!(ended_attempt, &AttemptRefView::from(&attempt));
                assert_eq!(outcome, "committed");
                assert_eq!(origin, &AttemptRefView::from(&attempt));
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }
}
