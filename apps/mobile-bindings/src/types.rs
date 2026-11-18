//! Flat, transport-neutral projections of Core state for the mobile clients.
//!
//! Every conversion from protocol-backed Core types is total: unknown enum
//! values become the explicit `"unknown"` string instead of panicking. These
//! types carry no credentials, invitation secrets or ciphertext.

use std::collections::HashMap;

use whalerein_core::api as core_api;
use whalerein_core::api::{
    ApprovalInfo, DeliveryMode, HostInfo, PairingPrompt, QuestionBatchInfo, QuestionFreshness,
    QuestionPageInfo, SessionCapabilities, SessionSummary,
};
use whalerein_core::conversation::{
    content_block, ApprovalState, AttemptOutcome, ConversationMessage, MessageRole, RunPhase,
    ViewUpdate,
};

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

/// One content block of a committed message. `kind` is stable text:
/// `text`, `tool`, `notice`, `attachment` or `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BlockView {
    pub block_id: String,
    pub kind: String,
    pub text: String,
    pub tool_name: Option<String>,
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
    let (kind, text, tool_name) = match content {
        Some(content_block::Content::Text(text)) => ("text", text.text.clone(), None),
        Some(content_block::Content::ToolCall(call)) => {
            ("tool", call.summary.clone(), Some(call.name.clone()))
        }
        Some(content_block::Content::ToolResult(result)) => ("tool", result.text.clone(), None),
        Some(content_block::Content::Notice(notice)) => ("notice", notice.text.clone(), None),
        Some(content_block::Content::Attachment(attachment)) => {
            ("attachment", attachment.name.clone(), None)
        }
        None => ("unknown", String::new(), None),
    };
    BlockView {
        block_id: block_id.to_owned(),
        kind: kind.to_owned(),
        text,
        tool_name,
    }
}

fn message_role_name(value: MessageRole) -> String {
    match value {
        MessageRole::User => "user".into(),
        MessageRole::Assistant => "assistant".into(),
        MessageRole::Tool => "tool".into(),
        MessageRole::Notice => "notice".into(),
        MessageRole::Unspecified => "unknown".into(),
    }
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

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendOperationView {
    pub operation_id: String,
    pub host_id: String,
    pub session_id: String,
    pub receipt: Option<CommandView>,
    pub reply: Option<MessageReplyView>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MessageReplyView {
    Committed { attempt_id: String, text: String },
    InputRequired,
    Uncertain,
}

impl From<whalerein_core::api::MessageReply> for MessageReplyView {
    fn from(value: whalerein_core::api::MessageReply) -> Self {
        use whalerein_core::api::MessageReply;
        match value {
            MessageReply::Committed { attempt_id, text } => Self::Committed { attempt_id, text },
            MessageReply::InputRequired => Self::InputRequired,
            MessageReply::Uncertain => Self::Uncertain,
        }
    }
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

/// Ordered events produced by a running watch and by pairing.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ConversationEvent {
    State {
        run_phase: String,
        /// True while a turn is running or stopping.
        running: bool,
        revision: u64,
        approvals: Vec<ApprovalView>,
    },
    HistoryReset,
    Message {
        message: MessageView,
    },
    Preview {
        attempt_id: String,
        text: String,
        replace: bool,
        incomplete: bool,
    },
    AttemptEnded {
        attempt_id: String,
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
        ViewUpdate::Message { message, .. } => vec![ConversationEvent::Message {
            message: message_view(message),
        }],
        ViewUpdate::Preview {
            attempt,
            text,
            replace,
            incomplete,
            ..
        } => vec![ConversationEvent::Preview {
            attempt_id: attempt.attempt_id.clone(),
            text: text.clone(),
            replace: *replace,
            incomplete: *incomplete,
        }],
        ViewUpdate::AttemptEnded { attempt, outcome } => vec![ConversationEvent::AttemptEnded {
            attempt_id: attempt.attempt_id.clone(),
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
        AttachmentContent, NoticeContent, TextContent, ToolCallContent, ToolResultContent,
    };
    use whalerein_core::conversation::{
        Approval, ApprovalDecision, AttemptRef, ContentBlock, SessionKey, SessionState,
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
            run_phase,
            approvals,
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
        assert_eq!(
            message_role_name(MessageRole::try_from(-5).unwrap_or_default()),
            "unknown"
        );
    }

    #[test]
    fn block_kinds_map_without_panicking() {
        let cases = [
            (
                content_block::Content::Text(TextContent {
                    text: "hello".into(),
                }),
                "text",
                None,
            ),
            (
                content_block::Content::ToolCall(ToolCallContent {
                    call_id: "c".into(),
                    name: "shell".into(),
                    summary: "ls".into(),
                }),
                "tool",
                Some("shell"),
            ),
            (
                content_block::Content::ToolResult(ToolResultContent {
                    call_id: "c".into(),
                    text: "out".into(),
                    is_error: false,
                }),
                "tool",
                None,
            ),
            (
                content_block::Content::Notice(NoticeContent {
                    category: "info".into(),
                    text: "note".into(),
                }),
                "notice",
                None,
            ),
            (
                content_block::Content::Attachment(AttachmentContent {
                    media_type: "text/plain".into(),
                    name: "file.txt".into(),
                    size: 3,
                    available: true,
                }),
                "attachment",
                None,
            ),
        ];
        for (content, kind, tool_name) in cases {
            let view = block_view("b", Some(&content));
            assert_eq!(view.kind, kind);
            assert_eq!(view.tool_name.as_deref(), tool_name);
        }
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
                run_phase,
                running,
                revision,
                approvals,
            } => {
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
                attempt_id: "a".into(),
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
            preview_prefix_bytes: None,
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
                    }],
                    created_at_unix_ms: 7,
                }
            }]
        );

        let update = ViewUpdate::Preview {
            attempt: AttemptRef {
                live_epoch: "e".into(),
                attempt_id: "a".into(),
            },
            block_id: "b".into(),
            text: "stream".into(),
            replace: true,
            incomplete: true,
        };
        assert_eq!(
            convert_update(&update, &mut seen),
            vec![ConversationEvent::Preview {
                attempt_id: "a".into(),
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
            content: Some(content_block::Content::ToolCall(ToolCallContent {
                call_id: "c-2".into(),
                name: "shell".into(),
                summary: "ls".into(),
            })),
        });
        let direct = message_view(&message);
        assert_eq!(MessageView::from(&message), direct);
        let mut seen = None;
        let events = convert_update(
            &ViewUpdate::Message {
                message: message.clone(),
                origin: None,
                preview_prefix_bytes: None,
            },
            &mut seen,
        );
        match &events[0] {
            ConversationEvent::Message { message } => assert_eq!(message, &direct),
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(direct.role, "assistant");
        assert_eq!(direct.blocks.len(), 2);
        assert_eq!(direct.blocks[1].kind, "tool");
        assert_eq!(direct.blocks[1].tool_name.as_deref(), Some("shell"));
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
}
