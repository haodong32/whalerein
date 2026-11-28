//! Shared, transport-independent provisional conversation state.
//! Sync owns durable writes; consumers receive typed display-neutral updates.
//!
//! The types in this module are Core-owned client values. Protocol-generated
//! message types are imported privately and converted at this boundary, so no
//! wire type appears in the public API.
use crate::api::{CoreError, CoreResult};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use whalerein_proto::{live_frame, LiveFrame, MessageUpsert};

pub use crate::session::SessionKey;

/// Generates one Core-owned enum with the wire label and numeric mapping.
/// Unknown values are interpreted by the caller-supplied closure so an outcome
/// can fall back to `Unspecified` while other enums report `Err`.
macro_rules! core_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($variant:ident = $value:expr => $label:literal),+ $(,)?
        },
        $unknown:expr
    ) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[repr(i32)]
        pub enum $name {
            $($variant = $value),+
        }
        impl Default for $name {
            fn default() -> Self {
                Self::Unspecified
            }
        }
        impl $name {
            /// Stable display label used by client output.
            pub fn as_str_name(&self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }
            /// Parses a stable display label.
            pub fn from_str_name(value: &str) -> Option<Self> {
                match value {
                    $($label => Some(Self::$variant)),+,
                    _ => None,
                }
            }
        }
        impl TryFrom<i32> for $name {
            type Error = i32;
            fn try_from(value: i32) -> Result<Self, Self::Error> {
                match value {
                    $($value => Ok(Self::$variant)),+,
                    other => ($unknown)(other),
                }
            }
        }
    };
}

core_enum!(
    /// Normalized author role of a committed conversation message.
    MessageRole {
        Unspecified = 0 => "MESSAGE_ROLE_UNSPECIFIED",
        User = 1 => "USER",
        Assistant = 2 => "ASSISTANT",
        Tool = 3 => "TOOL",
        Notice = 4 => "NOTICE",
        Context = 5 => "CONTEXT",
        Unknown = 6 => "UNKNOWN",
    },
    Err
);

impl MessageRole {
    /// Only genuine user authorship may enter the user lane. Context and
    /// unknown classifications are sanitized at the wire boundary.
    pub fn is_user(self) -> bool {
        matches!(self, Self::User)
    }
}

core_enum!(
    /// Normalized phase of the Source's current run.
    RunPhase {
        Unspecified = 0 => "RUN_PHASE_UNSPECIFIED",
        Idle = 1 => "IDLE",
        Running = 2 => "RUNNING",
        Stopping = 3 => "STOPPING",
        SourceUnavailable = 4 => "SOURCE_UNAVAILABLE",
    },
    Err
);

core_enum!(
    /// Terminal state of one live attempt.
    AttemptOutcome {
        Unspecified = 0 => "ATTEMPT_OUTCOME_UNSPECIFIED",
        Committed = 1 => "COMMITTED",
        Abandoned = 2 => "ABANDONED",
        Interrupted = 3 => "INTERRUPTED",
        AttemptFailed = 4 => "ATTEMPT_FAILED",
    },
    |_| Ok(AttemptOutcome::Unspecified)
);

core_enum!(
    /// Operator-visible state of one approval request.
    ApprovalState {
        Unspecified = 0 => "APPROVAL_STATE_UNSPECIFIED",
        Pending = 1 => "APPROVAL_STATE_PENDING",
        Approved = 2 => "APPROVAL_STATE_APPROVED",
        Rejected = 3 => "APPROVAL_STATE_REJECTED",
        Expired = 4 => "APPROVAL_STATE_EXPIRED",
        Unavailable = 5 => "APPROVAL_STATE_UNAVAILABLE",
    },
    Err
);

core_enum!(
    /// Recorded decision for one approval request.
    ApprovalDecision {
        Unspecified = 0 => "APPROVAL_DECISION_UNSPECIFIED",
        Approve = 1 => "APPROVAL_DECISION_APPROVE",
        Reject = 2 => "APPROVAL_DECISION_REJECT",
    },
    Err
);

core_enum!(
    /// Checked kind of one live or committed content block. A reader never
    /// merges reasoning into an answer block or the reverse.
    BlockKind {
        Unspecified = 0 => "BLOCK_KIND_UNSPECIFIED",
        Text = 1 => "TEXT",
        Reasoning = 2 => "REASONING",
    },
    Err
);

core_enum!(
    /// Evidenced stage of one Source tool call, in ascending order.
    ToolStage {
        Unspecified = 0 => "TOOL_STAGE_UNSPECIFIED",
        Preparation = 1 => "PREPARATION",
        Execution = 2 => "EXECUTION",
        Progress = 3 => "PROGRESS",
        Settlement = 4 => "SETTLEMENT",
    },
    |_| Ok(ToolStage::Unspecified)
);

core_enum!(
    /// Evidenced terminal outcome of one Source tool call.
    ToolOutcome {
        Unspecified = 0 => "TOOL_OUTCOME_UNSPECIFIED",
        Succeeded = 1 => "SUCCEEDED",
        Failed = 2 => "FAILED",
        Interrupted = 3 => "INTERRUPTED",
        Unknown = 4 => "UNKNOWN",
    },
    |_| Ok(ToolOutcome::Unspecified)
);

core_enum!(
    /// Stable Source notice category.
    NoticeCategory {
        Unspecified = 0 => "NOTICE_CATEGORY_UNSPECIFIED",
        Retry = 1 => "RETRY",
        MaxOutput = 2 => "MAX_OUTPUT",
        Compaction = 3 => "COMPACTION",
        Command = 4 => "COMMAND",
        Context = 5 => "CONTEXT",
        Omission = 6 => "OMISSION",
    },
    Err
);

core_enum!(
    /// State established by the Source for one notice.
    NoticeState {
        Unspecified = 0 => "NOTICE_STATE_UNSPECIFIED",
        Scheduled = 1 => "SCHEDULED",
        Started = 2 => "STARTED",
        Cancelled = 3 => "CANCELLED",
        Completed = 4 => "COMPLETED",
        Failed = 5 => "FAILED",
        Reached = 6 => "REACHED",
    },
    |_| Ok(NoticeState::Unspecified)
);

/// Identity of one live or committed attempt within an epoch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AttemptRef {
    pub live_epoch: String,
    pub attempt_id: String,
}

/// One tool-call identity scoped to its Source session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ToolActivityId {
    pub call_id: String,
}

/// Core-normalized state of one tool activity. `Abandoned` is assigned only
/// when the producing Attempt ended without settlement and had preparation
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum ToolState {
    #[default]
    Unknown,
    Preparing,
    Running,
    Succeeded,
    Failed,
    Interrupted,
    Abandoned,
}

impl ToolState {
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Interrupted)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Abandoned => "abandoned",
        }
    }
}

/// One ordered, displayable part of a settled tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResultPart {
    Text(String),
    Attachment(content_block::AttachmentContent),
}

/// One Source tool call activity. Core merges every occurrence by `call_id`
/// into exactly one value per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolActivity {
    pub id: ToolActivityId,
    pub name: String,
    pub summary: Option<String>,
    /// Earliest available committed occurrence order within the folded view.
    pub order: u64,
    pub state: ToolState,
    pub stages: Vec<ToolStage>,
    pub progress: Option<String>,
    pub failure_code: Option<String>,
    pub result: Vec<ToolResultPart>,
    pub outcome: ToolOutcome,
    pub attempt: Option<AttemptRef>,
    /// True when the first observed occurrence proved a result without the
    /// corresponding call record.
    pub call_missing: bool,
}

impl ToolActivity {
    fn derived_state(&self) -> ToolState {
        if self.state == ToolState::Abandoned {
            return ToolState::Abandoned;
        }
        match self.outcome {
            ToolOutcome::Succeeded => return ToolState::Succeeded,
            ToolOutcome::Failed => return ToolState::Failed,
            ToolOutcome::Interrupted => return ToolState::Interrupted,
            _ => {}
        }
        if self.stages.contains(&ToolStage::Settlement) || !self.result.is_empty() {
            return ToolState::Unknown;
        }
        if self.stages.contains(&ToolStage::Execution) || self.progress.is_some() {
            return ToolState::Running;
        }
        if self.stages.contains(&ToolStage::Preparation) {
            return ToolState::Preparing;
        }
        ToolState::Unknown
    }

    fn has_settlement(&self) -> bool {
        self.state.is_settled()
            || self.outcome != ToolOutcome::Unspecified
            || self.stages.contains(&ToolStage::Settlement)
            || !self.result.is_empty()
    }

    fn merge_occurrence(&mut self, mut incoming: ToolActivity) {
        let already_settled = self.has_settlement();
        let incoming_settled = incoming.has_settlement();
        for stage in incoming.stages.drain(..) {
            if !self.stages.contains(&stage) {
                self.stages.push(stage);
            }
        }
        self.stages.sort_unstable();
        self.stages.dedup();
        if self.name.is_empty() && !incoming.name.is_empty() {
            self.name = incoming.name;
        }
        if incoming.summary.is_some() {
            self.summary = incoming.summary;
        }
        if incoming.outcome != ToolOutcome::Unspecified
            && (incoming.outcome != ToolOutcome::Unknown
                || !already_settled
                || self.outcome == ToolOutcome::Unspecified)
        {
            self.outcome = incoming.outcome;
        }
        // A proven result is never removed by a late dispatch, progress or
        // preparation occurrence. A settled later occurrence may replace it.
        if !incoming.result.is_empty() && (incoming_settled || self.result.is_empty()) {
            self.result = incoming.result;
        }
        if incoming_settled {
            self.progress = None;
        } else if !already_settled && incoming.progress.is_some() {
            self.progress = incoming.progress;
        }
        if incoming.failure_code.is_some() && (incoming_settled || self.failure_code.is_none()) {
            self.failure_code = incoming.failure_code;
        }
        if self.attempt.is_none() {
            self.attempt = incoming.attempt;
        }
        self.call_missing |= incoming.call_missing;
        if self.state != ToolState::Abandoned || incoming_settled {
            // A later proven result can settle a retried call whose earlier
            // occurrence was abandoned with preparation alone.
            self.state = ToolState::Unknown;
            self.state = self.derived_state();
        }
        if self.state.is_settled() {
            self.progress = None;
        }
    }
}

/// The single tool-call reducer. A view folds every committed occurrence
/// through it; consumers never correlate native records themselves.
#[derive(Debug, Clone, Default)]
pub struct ToolActivities {
    items: BTreeMap<String, ToolActivity>,
    next_order: u64,
}

impl ToolActivities {
    /// Merge one occurrence and return the canonical activity. Idempotent for
    /// repeated occurrences.
    pub fn merge(&mut self, mut activity: ToolActivity) -> &ToolActivity {
        let call_id = activity.id.call_id.clone();
        if let Some(existing) = self.items.get_mut(&call_id) {
            activity.order = existing.order;
            existing.merge_occurrence(activity);
        } else {
            self.next_order += 1;
            activity.order = self.next_order;
            if !activity.stages.contains(&ToolStage::Preparation)
                && !activity.stages.contains(&ToolStage::Execution)
            {
                // A result, progress or settlement occurrence without its call
                // record still produces a usable settled activity.
                activity.call_missing = true;
            }
            activity.state = activity.derived_state();
            if activity.state.is_settled() {
                activity.progress = None;
            }
            self.items.insert(call_id.clone(), activity);
        }
        self.items.get(&call_id).expect("just inserted")
    }

    /// Fold a page or message's activities through the same reducer.
    pub fn absorb(&mut self, items: impl IntoIterator<Item = ToolActivity>) {
        for activity in items {
            self.merge(activity);
        }
    }

    /// Activities in earliest committed occurrence order.
    pub fn ordered(&self) -> Vec<&ToolActivity> {
        let mut items: Vec<_> = self.items.values().collect();
        items.sort_by_key(|activity| activity.order);
        items
    }

    pub fn get(&self, call_id: &str) -> Option<&ToolActivity> {
        self.items.get(call_id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Mark preparation-only activities owned by an abandoned or interrupted
    /// Attempt. Executed or settled activities are retained.
    pub fn abandon_attempt(&mut self, attempt: &AttemptRef) {
        for activity in self.items.values_mut() {
            if activity.attempt.as_ref() != Some(attempt) {
                continue;
            }
            if activity.state == ToolState::Preparing && activity.result.is_empty() {
                activity.state = ToolState::Abandoned;
                activity.progress = None;
            }
        }
    }
}

/// Content families carried by a committed conversation block.
pub mod content_block {
    use super::{NoticeCategory, NoticeState, ToolActivity};

    /// Plain assistant or user text.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct TextContent {
        pub text: String,
    }

    /// Displayable reasoning content. It is ordered alongside text blocks and
    /// keeps its own committed block identity.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct ReasoningContent {
        pub text: String,
    }

    /// A display-neutral notice from the Source.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct NoticeContent {
        pub category: NoticeCategory,
        pub state: NoticeState,
        pub summary: Option<String>,
    }

    /// Reference to an attachment that may not be locally available.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct AttachmentContent {
        pub media_type: String,
        pub name: String,
        pub size: u64,
        pub available: bool,
        /// Opaque Source occurrence reference; never bytes.
        pub attachment_ref: String,
    }

    /// One typed block payload.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Content {
        Text(TextContent),
        ToolActivity(ToolActivity),
        Notice(NoticeContent),
        Attachment(AttachmentContent),
        Reasoning(ReasoningContent),
    }
}

/// One ordered block of a committed conversation message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentBlock {
    pub block_id: String,
    pub content: Option<content_block::Content>,
}

/// One committed conversation message in display-neutral form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationMessage {
    pub message_id: String,
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    pub authored_at_unix_ms: i64,
    pub delivery_id: Option<String>,
}

/// One typed notice with its ordered placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub message_id: String,
    pub block_id: String,
    /// Zero-based block position within the committed message.
    pub position: usize,
    pub category: NoticeCategory,
    pub state: NoticeState,
    pub summary: Option<String>,
}

impl ConversationMessage {
    /// Typed notices in Source order.
    pub fn notices(&self) -> Vec<Notice> {
        self.blocks
            .iter()
            .enumerate()
            .filter_map(|(position, block)| {
                let Some(content_block::Content::Notice(notice)) = &block.content else {
                    return None;
                };
                Some(Notice {
                    message_id: self.message_id.clone(),
                    block_id: block.block_id.clone(),
                    position,
                    category: notice.category,
                    state: notice.state,
                    summary: notice.summary.clone(),
                })
            })
            .collect()
    }

    /// Tool occurrences in committed block order. Fold them through
    /// [`ToolActivities`] when a consumer needs the canonical activity set.
    pub fn tool_activities(&self) -> Vec<ToolActivity> {
        self.blocks
            .iter()
            .filter_map(|block| match &block.content {
                Some(content_block::Content::ToolActivity(activity)) => Some(activity.clone()),
                _ => None,
            })
            .collect()
    }
}

/// One approval request as exposed to a client.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Approval {
    pub approval_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub call_id: String,
    pub reason: String,
    pub digest: Vec<u8>,
    pub version: i64,
    pub expires_at_unix_ms: i64,
    pub state: ApprovalState,
    pub decision: ApprovalDecision,
    /// Bounded, credential-masked review fact projected by the Host.
    ///
    /// Empty means the Host supplied no such fact; consumers render the value
    /// as-is and never reconstruct it from the tool name, reason or raw
    /// arguments.
    pub review_action: String,
    /// Bounded, credential-masked review fact projected by the Host.
    pub review_target: String,
    /// Bounded, credential-masked review fact projected by the Host.
    pub review_impact: String,
    /// Lowercase review availability token: `complete`, `incomplete` or
    /// `unavailable`. Anything this build cannot establish is `unavailable`.
    pub review_availability: String,
}

/// Map the wire review availability to its canonical lowercase client token.
///
/// An unspecified or unknown value never claims completeness. Both map to
/// `unavailable` so a client hides review facts and withholds Allow.
pub(crate) fn review_availability_token(value: i32) -> &'static str {
    use whalerein_proto::ApprovalReviewAvailability as Proto;
    match Proto::try_from(value) {
        Ok(Proto::ApprovalReviewComplete) => "complete",
        Ok(Proto::ApprovalReviewIncomplete) => "incomplete",
        _ => "unavailable",
    }
}

/// Normalized watch state for one session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionState {
    pub session: Option<SessionKey>,
    pub live_epoch: String,
    pub revision: i64,
    pub history_head: String,
    pub projection_id: String,
    pub run_phase: RunPhase,
    pub approvals: Vec<Approval>,
}

impl From<whalerein_proto::AttemptRef> for AttemptRef {
    fn from(value: whalerein_proto::AttemptRef) -> Self {
        Self {
            live_epoch: value.live_epoch,
            attempt_id: value.attempt_id,
        }
    }
}

impl From<whalerein_proto::TextContent> for content_block::TextContent {
    fn from(value: whalerein_proto::TextContent) -> Self {
        Self { text: value.text }
    }
}

impl From<whalerein_proto::ReasoningContent> for content_block::ReasoningContent {
    fn from(value: whalerein_proto::ReasoningContent) -> Self {
        Self { text: value.text }
    }
}

impl From<whalerein_proto::AttachmentContent> for content_block::AttachmentContent {
    fn from(value: whalerein_proto::AttachmentContent) -> Self {
        Self {
            media_type: value.media_type,
            name: value.name,
            size: value.size,
            available: value.available,
            attachment_ref: value.attachment_ref,
        }
    }
}

fn notice_from_wire(value: whalerein_proto::NoticeContent) -> Option<content_block::NoticeContent> {
    let category = NoticeCategory::try_from(value.category).ok()?;
    if category == NoticeCategory::Unspecified {
        return None;
    }
    Some(content_block::NoticeContent {
        category,
        state: NoticeState::try_from(value.state).unwrap_or_default(),
        summary: value.summary,
    })
}

fn tool_part_from_wire(value: whalerein_proto::ToolResultPart) -> Option<ToolResultPart> {
    use whalerein_proto::tool_result_part::Content as Wire;
    match value.content? {
        Wire::Text(text) => Some(ToolResultPart::Text(text.text)),
        Wire::Attachment(attachment) => Some(ToolResultPart::Attachment(attachment.into())),
    }
}

fn tool_activity_from_wire(value: whalerein_proto::ToolActivity) -> Option<ToolActivity> {
    if value.call_id.is_empty() || value.call_id.len() > 128 {
        return None;
    }
    let stages = value
        .stages
        .into_iter()
        .filter_map(|stage| ToolStage::try_from(stage).ok())
        .filter(|stage| *stage != ToolStage::Unspecified)
        .collect::<Vec<_>>();
    let result = value
        .result
        .into_iter()
        .filter_map(tool_part_from_wire)
        .collect();
    let outcome = ToolOutcome::try_from(value.outcome).unwrap_or_default();
    let mut activity = ToolActivity {
        id: ToolActivityId {
            call_id: value.call_id,
        },
        name: value.name,
        summary: value.summary,
        order: 0,
        state: ToolState::Unknown,
        stages,
        progress: value.progress,
        failure_code: value.failure_code,
        result,
        outcome,
        attempt: None,
        call_missing: false,
    };
    activity.state = activity.derived_state();
    Some(activity)
}

fn block_from_wire(value: whalerein_proto::ContentBlock) -> Option<ContentBlock> {
    use whalerein_proto::content_block::Content as Wire;
    let content = match value.content? {
        Wire::Text(text) => content_block::Content::Text(text.into()),
        Wire::Reasoning(reasoning) => content_block::Content::Reasoning(reasoning.into()),
        Wire::ToolActivity(activity) => {
            content_block::Content::ToolActivity(tool_activity_from_wire(activity)?)
        }
        Wire::Notice(notice) => content_block::Content::Notice(notice_from_wire(notice)?),
        Wire::Attachment(attachment) => content_block::Content::Attachment(attachment.into()),
    };
    Some(ContentBlock {
        block_id: value.block_id,
        content: Some(content),
    })
}

fn role_from_wire(value: i32) -> MessageRole {
    match MessageRole::try_from(value) {
        Ok(MessageRole::Unspecified) | Err(_) => MessageRole::Unknown,
        Ok(role) => role,
    }
}

/// Normalize one canonical committed message. Context and unknown authorship
/// keep only an approved context or omission notice; every body is discarded so
/// private or unclassified material cannot enter the user lane.
impl From<whalerein_proto::ConversationMessage> for ConversationMessage {
    fn from(value: whalerein_proto::ConversationMessage) -> Self {
        let role = role_from_wire(value.role);
        let sanitized = matches!(role, MessageRole::Context | MessageRole::Unknown);
        let blocks = value
            .blocks
            .into_iter()
            .filter_map(block_from_wire)
            .filter(|block| {
                if !sanitized {
                    return true;
                }
                matches!(
                    &block.content,
                    Some(content_block::Content::Notice(notice))
                        if matches!(
                            notice.category,
                            NoticeCategory::Context | NoticeCategory::Omission
                        )
                )
            })
            .collect();
        Self {
            message_id: value.message_id,
            role,
            blocks,
            authored_at_unix_ms: value.authored_at_unix_ms,
            delivery_id: (role == MessageRole::User && !value.delivery_id.is_empty())
                .then_some(value.delivery_id),
        }
    }
}

impl From<whalerein_proto::Approval> for Approval {
    fn from(value: whalerein_proto::Approval) -> Self {
        Self {
            approval_id: value.approval_id,
            session_id: value.session_id,
            tool_name: value.tool_name,
            call_id: value.call_id,
            reason: value.reason,
            digest: value.digest,
            version: value.version,
            expires_at_unix_ms: value.expires_at_unix_ms,
            state: ApprovalState::try_from(value.state).unwrap_or_default(),
            decision: ApprovalDecision::try_from(value.decision).unwrap_or_default(),
            review_action: value.review_action,
            review_target: value.review_target,
            review_impact: value.review_impact,
            review_availability: review_availability_token(value.review_availability).to_owned(),
        }
    }
}

impl From<whalerein_proto::SessionState> for SessionState {
    fn from(value: whalerein_proto::SessionState) -> Self {
        Self {
            session: value.session.map(Into::into),
            live_epoch: value.live_epoch,
            revision: value.revision as i64,
            history_head: value.history_head,
            projection_id: value.projection_id,
            run_phase: RunPhase::try_from(value.run_phase).unwrap_or_default(),
            // Approvals are content. They arrive only through the bounded
            // ApprovalList query and are merged by the observation that emits
            // this state.
            approvals: Vec::new(),
        }
    }
}

const ATTEMPTS: usize = 8;
const PREVIEW_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: usize = 8 * PREVIEW_BYTES;
const SETTLED: usize = 4096;
/// Bound on the delivered committed-message identities kept for reconnect
/// dedupe. It covers a large replayed tail without growing without bound.
const DELIVERED: usize = 16_384;

/// Leading bytes already streamed for a committed message, split by kind so a
/// hidden reasoning prefix never offsets answer rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PreviewPrefix {
    pub text_bytes: usize,
    pub reasoning_bytes: usize,
}

/// Actual branch taken while applying one live frame. Boundary observation
/// records the confirmed disposition instead of guessing from a missing view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveDisposition {
    Applied,
    Duplicate,
    Stale,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewUpdate {
    State(SessionState),
    Preview {
        attempt: AttemptRef,
        block_id: String,
        kind: BlockKind,
        text: String,
        replace: bool,
        incomplete: bool,
    },
    Message {
        message: ConversationMessage,
        origin: Option<AttemptRef>,
        preview_prefix: Option<PreviewPrefix>,
    },
    ToolActivity(ToolActivity),
    AttemptEnded {
        attempt: AttemptRef,
        outcome: AttemptOutcome,
    },
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewBlock {
    block_id: String,
    kind: BlockKind,
    text: String,
}

#[derive(Default)]
struct Attempt {
    last: u64,
    /// Preview text per block, in first-seen order. A block ID has exactly one
    /// checked kind; a later frame cannot change that kind.
    blocks: Vec<PreviewBlock>,
    intact: bool,
}

impl Attempt {
    fn block_mut(&mut self, block_id: &str, kind: BlockKind) -> CoreResult<&mut String> {
        if let Some(index) = self
            .blocks
            .iter()
            .position(|block| block.block_id == block_id)
        {
            if self.blocks[index].kind != kind {
                return Err(CoreError::Security(
                    "live block kind changed for one block identity".into(),
                ));
            }
            return Ok(&mut self.blocks[index].text);
        }
        self.blocks.push(PreviewBlock {
            block_id: block_id.to_owned(),
            kind,
            text: String::new(),
        });
        Ok(&mut self.blocks.last_mut().expect("just pushed").text)
    }

    fn text_len(&self) -> usize {
        self.blocks.iter().map(|block| block.text.len()).sum()
    }

    fn preview_of(&self, block_id: &str, kind: BlockKind) -> Option<&str> {
        self.blocks
            .iter()
            .find(|block| block.block_id == block_id && block.kind == kind)
            .map(|block| block.text.as_str())
    }

    /// Counts the leading run of committed visible blocks that this Attempt
    /// already streamed with the same kind and identity.
    fn preview_prefix(&self, message: &ConversationMessage) -> PreviewPrefix {
        let mut prefix = PreviewPrefix::default();
        for block in &message.blocks {
            let (kind, text) = match &block.content {
                Some(content_block::Content::Text(content)) => (BlockKind::Text, &content.text),
                Some(content_block::Content::Reasoning(content)) => {
                    (BlockKind::Reasoning, &content.text)
                }
                _ => break,
            };
            let Some(preview) = self.preview_of(&block.block_id, kind) else {
                break;
            };
            if !text.starts_with(preview) {
                break;
            }
            match kind {
                BlockKind::Text => prefix.text_bytes += preview.len(),
                BlockKind::Reasoning => prefix.reasoning_bytes += preview.len(),
                BlockKind::Unspecified => {}
            }
        }
        prefix
    }
}

#[derive(Default)]
pub(crate) struct Conversation {
    epoch: String,
    active: HashMap<String, Attempt>,
    completed: VecDeque<(String, Instant)>,
    tools: ToolActivities,
    pending_activity_updates: Vec<ToolActivity>,
    // Committed message identities already delivered to this viewer. This set
    // survives `reset` so a reconnecting watch or a gap recovery cannot print
    // the same committed message twice, while a cold attach baseline can mark
    // history as seen without emitting it.
    delivered: HashSet<String>,
    delivered_order: VecDeque<String>,
}

impl Conversation {
    /// Reset provisional attempt state for a new live epoch.
    ///
    /// Delivered committed identities are intentionally retained: a reconnect
    /// or a snapshot recovery replaces the transport, not what the viewer has
    /// already seen. Explicit `sync` uses a fresh Conversation and therefore
    /// still reports the whole committed history.
    pub(crate) fn reset(&mut self, epoch: &str) -> ViewUpdate {
        let previous_epoch = std::mem::replace(&mut self.epoch, epoch.into());
        let abandoned: Vec<_> = self.active.keys().cloned().collect();
        for attempt_id in abandoned {
            self.tools.abandon_attempt(&AttemptRef {
                live_epoch: previous_epoch.clone(),
                attempt_id,
            });
        }
        self.active.clear();
        self.completed.clear();
        self.pending_activity_updates.clear();
        ViewUpdate::Reset
    }

    /// Mark every active preview as incomplete after a live-class loss. The
    /// next frame for an attempt reports `incomplete`, and the committed
    /// message or an authoritative read repairs the final view.
    pub(crate) fn mark_preview_incomplete(&mut self) {
        for attempt in self.active.values_mut() {
            attempt.intact = false;
        }
    }

    /// Replace every projection-bound view after the Source reports a new
    /// canonical content projection. Unlike a reconnect reset, this discards
    /// delivered identities and settled tool state because they belonged to
    /// the previous namespace.
    pub(crate) fn reset_projection(&mut self, epoch: &str) -> ViewUpdate {
        self.delivered.clear();
        self.delivered_order.clear();
        self.tools = ToolActivities::default();
        self.reset(epoch)
    }

    /// Discard an invalidated committed range while retaining the active
    /// connection's epoch for subsequent live frames.
    pub(crate) fn clear_history_projection(&mut self) {
        let epoch = self.epoch.clone();
        self.reset_projection(&epoch);
    }

    /// Drain tool activity updates produced by committed folds.
    pub(crate) fn take_activity_updates(&mut self) -> Vec<ViewUpdate> {
        self.pending_activity_updates
            .drain(..)
            .map(ViewUpdate::ToolActivity)
            .collect()
    }

    /// Mark a committed message as already visible without emitting it.
    ///
    /// A watch attach replays history to rebuild the local cache; that baseline
    /// is not new output. Later gap recovery can then deliver only messages
    /// that were never observed.
    pub(crate) fn mark_delivered(&mut self, upsert: &MessageUpsert) {
        if let Some(message) = &upsert.message {
            // A cold baseline is not new output, but its committed tool
            // occurrences still belong to the canonical activity view.
            let message: ConversationMessage = message.clone().into();
            self.fold_tools(&message, None, false);
            self.mark_delivered_id(&message.message_id);
        }
    }

    fn fold_tools(
        &mut self,
        message: &ConversationMessage,
        origin: Option<&AttemptRef>,
        emit: bool,
    ) {
        for activity in message.tool_activities() {
            let mut activity = activity;
            if activity.attempt.is_none() {
                activity.attempt = origin.cloned();
            }
            let before = self.tools.get(&activity.id.call_id).cloned();
            let merged = self.tools.merge(activity).clone();
            if emit && before.as_ref() != Some(&merged) {
                self.pending_activity_updates.push(merged);
            }
        }
    }

    /// Mark one committed identity as already visible to this viewer.
    pub(crate) fn mark_delivered_id(&mut self, message_id: &str) {
        if !message_id.is_empty() {
            self.note_delivered(message_id);
        }
    }

    fn note_delivered(&mut self, message_id: &str) -> bool {
        if !self.delivered.insert(message_id.to_owned()) {
            return false;
        }
        self.delivered_order.push_back(message_id.to_owned());
        while self.delivered_order.len() > DELIVERED {
            if let Some(evicted) = self.delivered_order.pop_front() {
                self.delivered.remove(&evicted);
            }
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn live(&mut self, frame: &LiveFrame) -> CoreResult<Option<ViewUpdate>> {
        Ok(self.live_decided(frame)?.0)
    }

    /// Apply one live frame and report the actual branch taken. Callers that
    /// own boundary observation need `duplicate`/`stale` separately from an
    /// applied frame that produced no visible update.
    pub(crate) fn live_decided(
        &mut self,
        frame: &LiveFrame,
    ) -> CoreResult<(Option<ViewUpdate>, LiveDisposition)> {
        let reference = frame
            .attempt
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing attempt identity".into()))?;
        if reference.live_epoch.is_empty()
            || reference.attempt_id.is_empty()
            || reference.attempt_id.len() > 512
            || frame.first_seq == 0
            || frame.last_seq < frame.first_seq
            || frame.payload.is_none()
        {
            return Err(CoreError::Security("invalid live frame".into()));
        }
        let block_kind = match frame.payload.as_ref() {
            Some(live_frame::Payload::Delta(delta)) => Some(
                BlockKind::try_from(delta.kind)
                    .map_err(|_| CoreError::Security("invalid live block kind".into()))?,
            ),
            Some(live_frame::Payload::Reset(reset)) => Some(
                BlockKind::try_from(reset.kind)
                    .map_err(|_| CoreError::Security("invalid live block kind".into()))?,
            ),
            _ => None,
        };
        if let Some(kind) = block_kind {
            if frame.block_id.is_empty() {
                return Err(CoreError::Security("missing text block identity".into()));
            }
            if !matches!(kind, BlockKind::Text | BlockKind::Reasoning) {
                return Err(CoreError::Security("invalid live block kind".into()));
            }
        }
        if reference.live_epoch != self.epoch {
            return Ok((None, LiveDisposition::Stale));
        }
        self.expire();
        if self
            .completed
            .iter()
            .any(|(id, _)| id == &reference.attempt_id)
        {
            return Ok((None, LiveDisposition::Stale));
        }
        if !self.active.contains_key(&reference.attempt_id) && self.active.len() >= ATTEMPTS {
            return Err(CoreError::Unavailable(
                "preview admission budget exceeded".into(),
            ));
        }
        let total: usize = self.active.values().map(Attempt::text_len).sum();
        let attempt = self.active.entry(reference.attempt_id.clone()).or_default();
        if frame.last_seq <= attempt.last {
            return Ok((None, LiveDisposition::Duplicate));
        }
        let contiguous = frame.first_seq == attempt.last + 1;
        let started = matches!(frame.payload, Some(live_frame::Payload::Started(_)));
        if attempt.last == 0 {
            attempt.intact = started && contiguous;
        } else {
            attempt.intact &= contiguous;
        }
        if let Some(kind) = block_kind {
            if attempt
                .blocks
                .iter()
                .any(|block| block.block_id == frame.block_id && block.kind != kind)
            {
                // Reject before consuming the sequence so a corrected frame can
                // still be applied.
                return Err(CoreError::Security(
                    "live block kind changed for one block identity".into(),
                ));
            }
        }
        attempt.last = frame.last_seq;
        match frame.payload.as_ref().expect("validated payload") {
            live_frame::Payload::Started(_) => Ok((None, LiveDisposition::Applied)),
            live_frame::Payload::Delta(delta) => {
                let text = delta.text.as_str();
                let kind = block_kind.expect("validated delta kind");
                let replace = false;
                let current = attempt.block_mut(&frame.block_id, kind)?;
                let replacement = current.len() + text.len();
                let total_after = total - current.len() + replacement;
                if replacement > PREVIEW_BYTES || total_after > TOTAL_BYTES {
                    // A truncated preview cannot be reconciled with its commit.
                    attempt.intact = false;
                } else {
                    current.push_str(text);
                }
                Ok((
                    Some(ViewUpdate::Preview {
                        attempt: reference.clone().into(),
                        block_id: frame.block_id.clone(),
                        kind,
                        text: text.to_owned(),
                        replace,
                        incomplete: !attempt.intact,
                    }),
                    LiveDisposition::Applied,
                ))
            }
            live_frame::Payload::Reset(reset) => {
                let text = reset.text.as_str();
                let kind = block_kind.expect("validated reset kind");
                let replace = true;
                let current = attempt.block_mut(&frame.block_id, kind)?;
                let replacement = text.len();
                let total_after = total - current.len() + replacement;
                if replacement > PREVIEW_BYTES || total_after > TOTAL_BYTES {
                    attempt.intact = false;
                } else {
                    // The reset carries the full authoritative content of this
                    // block; it does not invalidate other blocks.
                    *current = text.to_owned();
                }
                Ok((
                    Some(ViewUpdate::Preview {
                        attempt: reference.clone().into(),
                        block_id: frame.block_id.clone(),
                        kind,
                        text: text.to_owned(),
                        replace,
                        incomplete: !attempt.intact,
                    }),
                    LiveDisposition::Applied,
                ))
            }
            live_frame::Payload::Ended(end) => {
                let outcome = AttemptOutcome::try_from(end.outcome).unwrap_or_default();
                if outcome != AttemptOutcome::Committed {
                    self.tools.abandon_attempt(&reference.clone().into());
                    self.settle(&reference.attempt_id);
                }
                Ok((
                    Some(ViewUpdate::AttemptEnded {
                        attempt: reference.clone().into(),
                        outcome,
                    }),
                    LiveDisposition::Applied,
                ))
            }
        }
    }

    pub(crate) fn committed(&mut self, upsert: &MessageUpsert) -> CoreResult<ViewUpdate> {
        let message = upsert
            .message
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing committed message".into()))?;
        if message.message_id.is_empty()
            || message
                .blocks
                .iter()
                .any(|b| b.block_id.is_empty() || b.content.is_none())
        {
            return Err(CoreError::Security("invalid committed message".into()));
        }
        let origin: Option<AttemptRef> = upsert.origin.clone().map(Into::into);
        let message: ConversationMessage = message.clone().into();
        let mut prefix = None;
        if let Some(origin) = &origin {
            if origin.live_epoch == self.epoch {
                if let Some(attempt) = self.active.get(&origin.attempt_id) {
                    if attempt.intact {
                        prefix = Some(attempt.preview_prefix(&message));
                    }
                }
                self.settle(&origin.attempt_id);
            }
        }
        self.fold_tools(&message, origin.as_ref(), true);
        Ok(ViewUpdate::Message {
            message,
            origin,
            preview_prefix: prefix,
        })
    }

    /// Format one committed message that this viewer has not seen before.
    ///
    /// Returns `None` for a duplicate identity (a replayed batch, a reconnect
    /// snapshot that overlaps live output, or a gap snapshot that repeats
    /// already printed history), so each committed message is emitted exactly
    /// once per watch.
    pub(crate) fn committed_once(
        &mut self,
        upsert: &MessageUpsert,
    ) -> CoreResult<Option<ViewUpdate>> {
        let message = upsert
            .message
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing committed message".into()))?;
        let message_id = message.message_id.clone();
        let update = self.committed(upsert)?;
        if !self.note_delivered(&message_id) {
            return Ok(None);
        }
        Ok(Some(update))
    }

    fn settle(&mut self, id: &str) {
        self.active.remove(id);
        if !self.completed.iter().any(|(known, _)| known == id) {
            self.completed.push_back((id.into(), Instant::now()));
        }
        while self.completed.len() > SETTLED {
            self.completed.pop_front();
        }
    }
    fn expire(&mut self) {
        while self
            .completed
            .front()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(60))
        {
            self.completed.pop_front();
        }
    }
}

/// Answer and notice text of a committed message. Reasoning, tool activity and
/// attachment content are deliberately excluded.
pub fn message_text(message: &ConversationMessage) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            Some(content_block::Content::Text(text)) => Some(text.text.as_str()),
            Some(content_block::Content::Notice(notice)) => notice.summary.as_deref(),
            _ => None,
        })
        .collect()
}

/// Reasoning text of a committed message, in committed order.
pub fn reasoning_text(message: &ConversationMessage) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            Some(content_block::Content::Reasoning(reasoning)) => Some(reasoning.text.as_str()),
            _ => None,
        })
        .collect()
}

impl From<whalerein_proto::ToolActivity> for ToolActivity {
    fn from(value: whalerein_proto::ToolActivity) -> Self {
        tool_activity_from_wire(value).unwrap_or_else(|| ToolActivity {
            id: ToolActivityId::default(),
            name: String::new(),
            summary: None,
            order: 0,
            state: ToolState::Unknown,
            stages: Vec::new(),
            progress: None,
            failure_code: None,
            result: Vec::new(),
            outcome: ToolOutcome::Unspecified,
            attempt: None,
            call_missing: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_proto::BlockKind as WireBlockKind;
    use whalerein_proto::{
        content_block as wire_content_block, live_frame::Payload, tool_result_part as wire_part,
        AttemptEnded, AttemptOutcome as WireAttemptOutcome, AttemptStarted,
        ContentBlock as WireContentBlock, ConversationMessage as WireConversationMessage,
        LiveBlockDelta, LiveBlockReset, MessageRole as WireMessageRole,
        NoticeCategory as WireNoticeCategory, NoticeContent as WireNoticeContent,
        NoticeState as WireNoticeState, ReasoningContent as WireReasoningContent,
        TextContent as WireTextContent, ToolActivity as WireToolActivity,
        ToolOutcome as WireToolOutcome, ToolResultPart as WireToolResultPart,
        ToolStage as WireToolStage,
    };
    fn frame(seq: u64, payload: Payload) -> LiveFrame {
        LiveFrame {
            attempt: Some(whalerein_proto::AttemptRef {
                live_epoch: "epoch".into(),
                attempt_id: "a".into(),
            }),
            block_id: "text".into(),
            first_seq: seq,
            last_seq: seq,
            payload: Some(payload),
        }
    }
    fn delta(seq: u64, kind: whalerein_proto::BlockKind, text: &str) -> LiveFrame {
        frame(
            seq,
            Payload::Delta(LiveBlockDelta {
                kind: kind as i32,
                text: text.into(),
            }),
        )
    }
    fn reset(seq: u64, kind: whalerein_proto::BlockKind, text: &str) -> LiveFrame {
        frame(
            seq,
            Payload::Reset(LiveBlockReset {
                kind: kind as i32,
                text: text.into(),
            }),
        )
    }
    fn text_block(id: &str, text: &str) -> WireContentBlock {
        WireContentBlock {
            block_id: id.into(),
            content: Some(wire_content_block::Content::Text(WireTextContent {
                text: text.into(),
            })),
        }
    }
    fn reasoning_block(id: &str, text: &str) -> WireContentBlock {
        WireContentBlock {
            block_id: id.into(),
            content: Some(wire_content_block::Content::Reasoning(
                WireReasoningContent { text: text.into() },
            )),
        }
    }
    fn message() -> MessageUpsert {
        MessageUpsert {
            message: Some(WireConversationMessage {
                message_id: "m".into(),
                role: WireMessageRole::Assistant as i32,
                blocks: vec![text_block("text", "xy")],
                ..Default::default()
            }),
            origin: frame(1, Payload::Started(AttemptStarted {})).attempt,
        }
    }
    fn activity(
        call_id: &str,
        stages: &[WireToolStage],
        outcome: WireToolOutcome,
        name: &str,
    ) -> WireToolActivity {
        WireToolActivity {
            call_id: call_id.into(),
            stages: stages.iter().map(|stage| *stage as i32).collect(),
            name: name.into(),
            outcome: outcome as i32,
            ..Default::default()
        }
    }
    fn tool_block(id: &str, activity: WireToolActivity) -> WireContentBlock {
        WireContentBlock {
            block_id: id.into(),
            content: Some(wire_content_block::Content::ToolActivity(activity)),
        }
    }
    fn committed(id: &str, blocks: Vec<WireContentBlock>) -> MessageUpsert {
        MessageUpsert {
            message: Some(WireConversationMessage {
                message_id: id.into(),
                role: WireMessageRole::Assistant as i32,
                blocks,
                ..Default::default()
            }),
            origin: None,
        }
    }

    #[test]
    fn wire_approval_review_facts_reach_the_conversation_projection() {
        use whalerein_proto::ApprovalReviewAvailability as Availability;

        let complete: Approval = whalerein_proto::Approval {
            approval_id: "a".into(),
            review_action: "Change [REDACTED] credential scope".into(),
            review_target: "Sources/[REDACTED].swift".into(),
            review_impact: "Replaces the selected file content.".into(),
            review_availability: Availability::ApprovalReviewComplete as i32,
            ..Default::default()
        }
        .into();
        assert_eq!(complete.review_action, "Change [REDACTED] credential scope");
        assert_eq!(complete.review_target, "Sources/[REDACTED].swift");
        assert_eq!(
            complete.review_impact,
            "Replaces the selected file content."
        );
        assert_eq!(complete.review_availability, "complete");

        let absent: Approval = whalerein_proto::Approval::default().into();
        assert!(absent.review_action.is_empty());
        assert!(absent.review_target.is_empty());
        assert!(absent.review_impact.is_empty());
        assert_eq!(absent.review_availability, "unavailable");

        let unknown: Approval = whalerein_proto::Approval {
            review_availability: 99,
            ..Default::default()
        }
        .into();
        assert_eq!(unknown.review_availability, "unavailable");
    }

    #[test]
    fn streamed_text_and_final_commit_share_one_bounded_reducer() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        let one = delta(2, WireBlockKind::Text, "x");
        assert!(matches!(
            state.live(&one).unwrap(),
            Some(ViewUpdate::Preview {
                incomplete: false,
                kind: BlockKind::Text,
                ..
            })
        ));
        assert!(state.live(&one).unwrap().is_none());
        let committed = message();
        let expected: ConversationMessage = committed.message.clone().unwrap().into();
        assert!(matches!(
            state.committed(&committed).unwrap(),
            ViewUpdate::Message {
                message,
                preview_prefix: Some(PreviewPrefix { text_bytes: 1, reasoning_bytes: 0 }),
                ..
            } if message == expected
        ));
        assert!(state
            .live(&delta(3, WireBlockKind::Text, "y"))
            .unwrap()
            .is_none());
        assert!(state.active.is_empty());
    }

    #[test]
    fn interleaved_reasoning_and_text_keep_identity_and_settle_in_source_order() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        let mut reasoning = delta(2, WireBlockKind::Reasoning, "think ");
        reasoning.block_id = "r1".into();
        state.live(&reasoning).unwrap();
        let mut text = delta(3, WireBlockKind::Text, "answer ");
        text.block_id = "t1".into();
        state.live(&text).unwrap();
        let mut reasoning2 = delta(4, WireBlockKind::Reasoning, "more");
        reasoning2.block_id = "r2".into();
        state.live(&reasoning2).unwrap();
        let mut text2 = delta(5, WireBlockKind::Text, "done");
        text2.block_id = "t2".into();
        state.live(&text2).unwrap();
        let mut upsert = committed(
            "m",
            vec![
                reasoning_block("r1", "think "),
                text_block("t1", "answer "),
                reasoning_block("r2", "more"),
                text_block("t2", "done"),
            ],
        );
        upsert.origin = frame(1, Payload::Started(AttemptStarted {})).attempt;
        let update = state.committed(&upsert).unwrap();
        match update {
            ViewUpdate::Message {
                message,
                preview_prefix: Some(prefix),
                ..
            } => {
                assert_eq!(message.blocks.len(), 4);
                assert_eq!(prefix.text_bytes, 11);
                assert_eq!(prefix.reasoning_bytes, 10);
                assert_eq!(reasoning_text(&message), "think more");
                assert_eq!(message_text(&message), "answer done");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn reasoning_disabled_model_produces_only_text_blocks() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state.live(&delta(2, WireBlockKind::Text, "plain")).unwrap();
        let mut upsert = committed("m", vec![text_block("t", "plain answer")]);
        upsert.origin = frame(1, Payload::Started(AttemptStarted {})).attempt;
        let ViewUpdate::Message { message, .. } = state.committed(&upsert).unwrap() else {
            panic!("expected message")
        };
        assert!(reasoning_text(&message).is_empty());
        assert!(message
            .blocks
            .iter()
            .all(|block| !matches!(block.content, Some(content_block::Content::Reasoning(_)))));
    }

    #[test]
    fn kind_change_for_one_block_identity_is_rejected() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state.live(&delta(2, WireBlockKind::Text, "x")).unwrap();
        let error = state
            .live(&delta(3, WireBlockKind::Reasoning, "y"))
            .unwrap_err();
        assert!(matches!(error, CoreError::Security(_)));
        let mut invalid = delta(3, WireBlockKind::Text, "y");
        invalid.block_id.clear();
        assert!(state.live(&invalid).is_err());
        let mut unknown = delta(4, WireBlockKind::Unspecified, "y");
        unknown.payload = Some(Payload::Delta(LiveBlockDelta {
            kind: 0,
            text: "y".into(),
        }));
        assert!(state.live(&unknown).is_err());
    }

    #[test]
    fn rejected_delta_does_not_consume_the_sequence_of_a_corrected_frame() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        let valid = delta(2, WireBlockKind::Text, "x");
        let mut invalid = valid.clone();
        invalid.block_id.clear();
        assert!(state.live(&invalid).is_err());
        assert!(matches!(
            state.live(&valid).unwrap(),
            Some(ViewUpdate::Preview { text, incomplete: false, .. }) if text == "x"
        ));
        assert!(matches!(
            state.committed(&message()).unwrap(),
            ViewUpdate::Message {
                preview_prefix: Some(PreviewPrefix { text_bytes: 1, .. }),
                ..
            }
        ));
    }

    #[test]
    fn a_live_loss_marks_the_next_preview_incomplete() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        assert!(matches!(
            state.live(&delta(2, WireBlockKind::Text, "x")).unwrap(),
            Some(ViewUpdate::Preview {
                incomplete: false,
                ..
            })
        ));
        // A dropped live frame outside the sequence path marks the attempt
        // without failing the reducer; the next frame exposes the hole.
        state.mark_preview_incomplete();
        assert!(matches!(
            state.live(&delta(3, WireBlockKind::Text, "y")).unwrap(),
            Some(ViewUpdate::Preview {
                incomplete: true,
                ..
            })
        ));
    }

    #[test]
    fn gaps_resets_and_commit_before_start_do_not_create_duplicate_authoritative_text() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state.committed(&message()).unwrap();
        assert!(state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap()
            .is_none());
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        assert!(matches!(
            state.live(&reset(3, WireBlockKind::Text, "x")).unwrap(),
            Some(ViewUpdate::Preview {
                replace: true,
                incomplete: true,
                ..
            })
        ));
        assert!(matches!(
            state.committed(&message()).unwrap(),
            ViewUpdate::Message {
                preview_prefix: None,
                ..
            }
        ));
        state.reset("new");
        assert!(state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap()
            .is_none());
    }
    #[test]
    fn invalid_frames_and_message_content_fail_closed() {
        let mut state = Conversation::default();
        state.reset("epoch");
        assert!(state.live(&LiveFrame::default()).is_err());
        let mut invalid = frame(0, Payload::Started(AttemptStarted {}));
        assert!(state.live(&invalid).is_err());
        invalid.first_seq = 1;
        invalid.last_seq = 1;
        invalid.block_id.clear();
        invalid.payload = Some(Payload::Delta(LiveBlockDelta {
            kind: whalerein_proto::BlockKind::Text as i32,
            text: "x".into(),
        }));
        assert!(state.live(&invalid).is_err());
        assert!(state.committed(&MessageUpsert::default()).is_err());
        let mut invalid = message();
        invalid.message.as_mut().unwrap().message_id.clear();
        assert!(state.committed(&invalid).is_err());
    }
    #[test]
    fn block_level_resets_keep_other_blocks_and_the_commit_prefix() {
        fn block_frame(
            seq: u64,
            block: &str,
            kind: BlockKind,
            text: &str,
            replace: bool,
        ) -> LiveFrame {
            let payload = if replace {
                Payload::Reset(LiveBlockReset {
                    kind: kind as i32,
                    text: text.into(),
                })
            } else {
                Payload::Delta(LiveBlockDelta {
                    kind: kind as i32,
                    text: text.into(),
                })
            };
            let mut frame = frame(seq, payload);
            frame.block_id = block.into();
            frame
        }
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state
            .live(&block_frame(2, "a", BlockKind::Text, "hello ", false))
            .unwrap();
        assert!(matches!(
            state
                .live(&block_frame(3, "b", BlockKind::Text, "world", false))
                .unwrap(),
            Some(ViewUpdate::Preview {
                incomplete: false,
                ..
            })
        ));
        // A contiguous block reset replaces only block b; block a and the
        // attempt's integrity survive.
        let reset = state
            .live(&block_frame(4, "b", BlockKind::Text, "there", true))
            .unwrap()
            .unwrap();
        assert!(matches!(
            reset,
            ViewUpdate::Preview {
                replace: true,
                incomplete: false,
                ..
            }
        ));
        let committed = MessageUpsert {
            message: Some(WireConversationMessage {
                message_id: "m".into(),
                role: WireMessageRole::Assistant as i32,
                blocks: vec![
                    text_block("a", "hello "),
                    text_block("b", "there"),
                    tool_block(
                        "tool",
                        activity(
                            "c",
                            &[WireToolStage::Execution],
                            WireToolOutcome::Succeeded,
                            "Read",
                        ),
                    ),
                ],
                ..Default::default()
            }),
            origin: frame(1, Payload::Started(AttemptStarted {})).attempt,
        };
        let update = state.committed(&committed).unwrap();
        assert!(matches!(
            update,
            ViewUpdate::Message {
                preview_prefix: Some(PreviewPrefix {
                    text_bytes: 11,
                    reasoning_bytes: 0
                }),
                ..
            }
        ));
        assert_eq!(state.tools.len(), 1);
        assert_eq!(state.take_activity_updates().len(), 1);
    }

    #[test]
    fn preview_memory_settlement_and_admission_have_explicit_limits() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state
            .live(&delta(
                2,
                WireBlockKind::Text,
                &"x".repeat(PREVIEW_BYTES + 1),
            ))
            .unwrap();
        assert_eq!(state.active["a"].text_len(), 0);
        assert!(!state.active["a"].intact);
        state
            .live(&frame(
                3,
                Payload::Ended(AttemptEnded {
                    outcome: WireAttemptOutcome::Abandoned as i32,
                }),
            ))
            .unwrap();
        assert!(state.active.is_empty());
        for index in 0..ATTEMPTS {
            let mut f = frame(1, Payload::Started(AttemptStarted {}));
            f.attempt.as_mut().unwrap().attempt_id = index.to_string();
            state.live(&f).unwrap();
        }
        let mut extra = frame(1, Payload::Started(AttemptStarted {}));
        extra.attempt.as_mut().unwrap().attempt_id = "extra".into();
        assert!(state.live(&extra).is_err());
        state.completed.clear();
        for index in 0..=SETTLED {
            state.settle(&index.to_string());
        }
        assert_eq!(state.completed.len(), SETTLED);
        state.completed.front_mut().unwrap().1 = Instant::now() - Duration::from_secs(61);
        state.expire();
        assert_eq!(state.completed.len(), SETTLED - 1);
    }

    #[test]
    fn duplicate_and_late_frames_after_commit_do_not_reopen_the_attempt() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state.live(&delta(2, WireBlockKind::Text, "x")).unwrap();
        state.committed(&message()).unwrap();
        assert!(state
            .live(&delta(3, WireBlockKind::Text, "late"))
            .unwrap()
            .is_none());
        assert!(state.active.is_empty());
    }

    #[test]
    fn interrupted_attempt_clears_preview_and_late_frames_cannot_reopen_it() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state
            .live(&delta(2, WireBlockKind::Text, "partial"))
            .unwrap();
        let ended = state
            .live(&frame(
                3,
                Payload::Ended(AttemptEnded {
                    outcome: WireAttemptOutcome::Interrupted as i32,
                }),
            ))
            .unwrap();
        assert!(matches!(
            ended,
            Some(ViewUpdate::AttemptEnded {
                outcome: AttemptOutcome::Interrupted,
                ..
            })
        ));
        assert!(state.active.is_empty());
        assert!(state
            .live(&delta(4, WireBlockKind::Text, "late"))
            .unwrap()
            .is_none());
        // Only a Source commit survives; a fresh Attempt starts cleanly.
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        assert!(matches!(
            state.live(&delta(2, WireBlockKind::Text, "fresh")).unwrap(),
            Some(ViewUpdate::Preview { text, .. }) if text == "fresh"
        ));
    }

    #[test]
    fn parallel_out_of_order_tool_calls_keep_one_activity_and_earliest_order() {
        let mut tools = ToolActivities::default();
        let mut result_b = activity(
            "call-b",
            &[WireToolStage::Settlement],
            WireToolOutcome::Succeeded,
            "",
        );
        result_b.result = vec![WireToolResultPart {
            content: Some(wire_part::Content::Text(WireTextContent {
                text: "b done".into(),
            })),
        }];
        tools.merge(result_b.into());
        tools.merge(
            activity(
                "call-a",
                &[WireToolStage::Preparation],
                WireToolOutcome::Unspecified,
                "Read",
            )
            .into(),
        );
        tools.merge(
            activity(
                "call-b",
                &[WireToolStage::Preparation, WireToolStage::Execution],
                WireToolOutcome::Unspecified,
                "Bash",
            )
            .into(),
        );
        tools.merge(
            activity(
                "call-a",
                &[WireToolStage::Settlement],
                WireToolOutcome::Succeeded,
                "Read",
            )
            .into(),
        );
        assert_eq!(tools.len(), 2);
        let ordered = tools.ordered();
        assert_eq!(ordered[0].id.call_id, "call-b");
        assert_eq!(ordered[1].id.call_id, "call-a");
        assert!(ordered[0].call_missing);
        assert_eq!(ordered[0].state, ToolState::Succeeded);
        assert_eq!(ordered[1].state, ToolState::Succeeded);
    }

    #[test]
    fn tool_stage_merge_and_result_precedence_are_one_activity_per_call() {
        let mut tools = ToolActivities::default();
        let mut dispatch = ToolActivity::from(activity(
            "call-1",
            &[WireToolStage::Preparation, WireToolStage::Execution],
            WireToolOutcome::Unspecified,
            "Read",
        ));
        dispatch.attempt = Some(AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        });
        let merged = tools.merge(dispatch);
        assert_eq!(merged.state, ToolState::Running);
        assert_eq!(
            merged.stages,
            vec![ToolStage::Preparation, ToolStage::Execution]
        );

        let mut late_progress = activity(
            "call-1",
            &[WireToolStage::Progress],
            WireToolOutcome::Unspecified,
            "Read",
        );
        late_progress.progress = Some("half".into());
        assert_eq!(
            tools.merge(late_progress.into()).progress.as_deref(),
            Some("half")
        );

        let mut result = activity(
            "call-1",
            &[WireToolStage::Settlement],
            WireToolOutcome::Succeeded,
            "Read",
        );
        result.result = vec![WireToolResultPart {
            content: Some(wire_part::Content::Text(WireTextContent {
                text: "done".into(),
            })),
        }];
        let merged = tools.merge(result.into()).clone();
        assert_eq!(merged.state, ToolState::Succeeded);
        assert_eq!(merged.result.len(), 1);
        assert!(merged.progress.is_none());
        assert_eq!(tools.len(), 1);

        // A late dispatch must not regress the settled activity or its result.
        let late = activity(
            "call-1",
            &[WireToolStage::Execution],
            WireToolOutcome::Unspecified,
            "Read",
        );
        let merged = tools.merge(late.into()).clone();
        assert_eq!(merged.state, ToolState::Succeeded);
        assert_eq!(merged.result.len(), 1);
        assert_eq!(tools.ordered().len(), 1);
    }

    #[test]
    fn result_first_activity_is_call_missing_and_stable_across_pages() {
        let mut tools = ToolActivities::default();
        let first = ToolActivity::from(activity(
            "call-2",
            &[WireToolStage::Settlement],
            WireToolOutcome::Succeeded,
            "",
        ));
        assert!(tools.merge(first).call_missing);
        let dispatch = ToolActivity::from(activity(
            "call-2",
            &[WireToolStage::Preparation, WireToolStage::Execution],
            WireToolOutcome::Unspecified,
            "Read",
        ));
        let merged = tools.merge(dispatch).clone();
        assert_eq!(merged.name, "Read");
        assert_eq!(merged.state, ToolState::Succeeded);
        assert!(merged.call_missing);
        let mut page = ToolActivities::default();
        page.absorb(tools.ordered().into_iter().cloned());
        assert_eq!(page.len(), 1);
        assert_eq!(page.get("call-2"), tools.get("call-2"));
    }

    #[test]
    fn failed_result_and_failure_code_are_preserved() {
        let mut tools = ToolActivities::default();
        let mut failed = activity(
            "call-3",
            &[WireToolStage::Settlement],
            WireToolOutcome::Failed,
            "Bash",
        );
        failed.failure_code = Some("exit_1".into());
        let merged = tools.merge(failed.into()).clone();
        assert_eq!(merged.state, ToolState::Failed);
        assert_eq!(merged.failure_code.as_deref(), Some("exit_1"));
    }

    #[test]
    fn abandoned_preparation_is_marked_and_settled_activity_is_kept() {
        let mut tools = ToolActivities::default();
        let mut preparing = ToolActivity::from(activity(
            "call-4",
            &[WireToolStage::Preparation],
            WireToolOutcome::Unspecified,
            "Read",
        ));
        preparing.attempt = Some(AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        });
        tools.merge(preparing);
        let mut settled = ToolActivity::from(activity(
            "call-5",
            &[WireToolStage::Preparation, WireToolStage::Settlement],
            WireToolOutcome::Succeeded,
            "Read",
        ));
        settled.attempt = Some(AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        });
        tools.merge(settled);
        tools.abandon_attempt(&AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        });
        assert_eq!(tools.get("call-4").unwrap().state, ToolState::Abandoned);
        assert_eq!(tools.get("call-5").unwrap().state, ToolState::Succeeded);
    }

    #[test]
    fn a_later_result_settles_an_activity_whose_earlier_attempt_was_abandoned() {
        let mut tools = ToolActivities::default();
        let mut preparing = ToolActivity::from(activity(
            "call-6",
            &[WireToolStage::Preparation],
            WireToolOutcome::Unspecified,
            "Read",
        ));
        preparing.attempt = Some(AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        });
        tools.merge(preparing);
        tools.abandon_attempt(&AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "a".into(),
        });
        assert_eq!(tools.get("call-6").unwrap().state, ToolState::Abandoned);
        let result = activity(
            "call-6",
            &[WireToolStage::Execution, WireToolStage::Settlement],
            WireToolOutcome::Succeeded,
            "Read",
        );
        let merged = tools.merge(result.into()).clone();
        assert_eq!(merged.state, ToolState::Succeeded);
    }

    #[test]
    fn cold_baseline_folds_tool_activities_without_emitting_them() {
        let mut state = Conversation::default();
        let baseline = committed(
            "prep",
            vec![tool_block(
                "tool",
                activity(
                    "call-7",
                    &[WireToolStage::Preparation],
                    WireToolOutcome::Unspecified,
                    "Read",
                ),
            )],
        );
        state.mark_delivered(&baseline);
        assert!(state.take_activity_updates().is_empty());
        assert_eq!(
            state.tools.get("call-7").unwrap().state,
            ToolState::Preparing
        );
        let result = committed(
            "result",
            vec![tool_block(
                "tool",
                activity(
                    "call-7",
                    &[WireToolStage::Execution, WireToolStage::Settlement],
                    WireToolOutcome::Succeeded,
                    "Read",
                ),
            )],
        );
        state.committed(&result).unwrap();
        assert_eq!(state.tools.len(), 1);
        assert_eq!(
            state.tools.get("call-7").unwrap().state,
            ToolState::Succeeded
        );
        let updates = state.take_activity_updates();
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], ViewUpdate::ToolActivity(_)));
    }

    #[test]
    fn notice_categories_states_and_context_roles_are_typed_and_sanitized() {
        let notice = |category: WireNoticeCategory, summary: Option<&str>| WireContentBlock {
            block_id: "n".into(),
            content: Some(wire_content_block::Content::Notice(WireNoticeContent {
                category: category as i32,
                state: WireNoticeState::Started as i32,
                summary: summary.map(str::to_owned),
            })),
        };
        let upsert = MessageUpsert {
            message: Some(WireConversationMessage {
                message_id: "context".into(),
                role: WireMessageRole::Context as i32,
                blocks: vec![
                    text_block("private", "secret prompt"),
                    notice(WireNoticeCategory::Context, Some("context")),
                    notice(WireNoticeCategory::Retry, Some("retry")),
                ],
                ..Default::default()
            }),
            origin: None,
        };
        let message: ConversationMessage = upsert.message.unwrap().into();
        assert_eq!(message.role, MessageRole::Context);
        assert_eq!(message.blocks.len(), 1);
        assert_eq!(message.blocks[0].block_id, "n");
        assert!(!message_text(&message).contains("secret"));
        let notices = message.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].category, NoticeCategory::Context);
        assert_eq!(notices[0].state, NoticeState::Started);
        assert_eq!(notices[0].summary.as_deref(), Some("context"));

        let unknown = WireConversationMessage {
            message_id: "unknown".into(),
            role: WireMessageRole::Unknown as i32,
            blocks: vec![text_block("body", "unclassified")],
            ..Default::default()
        };
        let message: ConversationMessage = unknown.into();
        assert_eq!(message.role, MessageRole::Unknown);
        assert!(message.blocks.is_empty());

        let missing = WireConversationMessage {
            message_id: "missing".into(),
            role: 0,
            blocks: vec![text_block("body", "private")],
            ..Default::default()
        };
        let message: ConversationMessage = missing.into();
        assert_eq!(message.role, MessageRole::Unknown);
        assert!(message.blocks.is_empty());

        // Unknown categories are dropped without exposing their payload.
        let invalid = WireConversationMessage {
            message_id: "invalid".into(),
            role: WireMessageRole::Notice as i32,
            blocks: vec![WireContentBlock {
                block_id: "bad".into(),
                content: Some(wire_content_block::Content::Notice(WireNoticeContent {
                    category: 99,
                    state: 99,
                    summary: Some("raw".into()),
                })),
            }],
            ..Default::default()
        };
        let message: ConversationMessage = invalid.into();
        assert!(message.blocks.is_empty());
    }

    #[test]
    fn result_attachment_parts_keep_only_the_opaque_reference() {
        let wire = WireToolActivity {
            call_id: "call-file".into(),
            stages: vec![WireToolStage::Settlement as i32],
            name: "Write".into(),
            outcome: WireToolOutcome::Succeeded as i32,
            result: vec![
                WireToolResultPart {
                    content: Some(wire_part::Content::Text(WireTextContent {
                        text: "saved".into(),
                    })),
                },
                WireToolResultPart {
                    content: Some(wire_part::Content::Attachment(
                        whalerein_proto::AttachmentContent {
                            media_type: "image/png".into(),
                            name: "image.png".into(),
                            size: 3,
                            available: true,
                            attachment_ref: "occurrence-1".into(),
                        },
                    )),
                },
            ],
            ..Default::default()
        };
        let activity = ToolActivity::from(wire);
        assert_eq!(activity.result.len(), 2);
        match &activity.result[1] {
            ToolResultPart::Attachment(attachment) => {
                assert_eq!(attachment.attachment_ref, "occurrence-1");
                assert_eq!(attachment.size, 3);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
