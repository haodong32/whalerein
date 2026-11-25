//! Stable Core-facing domain types.
//!
//! These types are not HTTP DTOs. They are the projection that CLI and future
//! bindings consume, so proto changes stay inside Core.

use serde::{Deserialize, Serialize};

/// Explicit wire label for an enum projection.
///
/// A value this build does not know becomes `UNKNOWN_ENUM_<number>` instead of
/// the zero value, so a newer peer is visible instead of being rendered as a
/// valid unspecified value. This is the Core side of the unknown-enum policy in
/// `proto/v1/WIRE.md`.
fn enum_label<E>(value: i32, known_name: impl Fn(E) -> &'static str) -> String
where
    E: TryFrom<i32>,
{
    match E::try_from(value) {
        Ok(known) => known_name(known).to_owned(),
        Err(_) => format!("UNKNOWN_ENUM_{value}"),
    }
}

/// Delivery choice for a new user message.
///
/// The wire enum stays behind the protocol boundary; callers choose a Core
/// value and unknown peers are rejected before submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    StartIfIdle,
    EnqueueAfterRun,
    Steer,
}

impl DeliveryMode {
    pub fn as_str_name(self) -> &'static str {
        match self {
            Self::StartIfIdle => "START_IF_IDLE",
            Self::EnqueueAfterRun => "ENQUEUE_AFTER_RUN",
            Self::Steer => "STEER",
        }
    }
}

impl From<DeliveryMode> for whalerein_proto::DeliveryMode {
    fn from(value: DeliveryMode) -> Self {
        match value {
            DeliveryMode::StartIfIdle => Self::StartIfIdle,
            DeliveryMode::EnqueueAfterRun => Self::EnqueueAfterRun,
            DeliveryMode::Steer => Self::Steer,
        }
    }
}

/// A question phase the client can act on without reading the wire enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuestionPhase {
    Open,
    Continued,
    Settled,
    ReplyQueued,
}

impl QuestionPhase {
    /// Parse one wire phase label. Unknown or unavailable phases return `None`.
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "QUESTION_OPEN" => Some(Self::Open),
            "QUESTION_CONTINUED" => Some(Self::Continued),
            "QUESTION_SETTLED" => Some(Self::Settled),
            "QUESTION_REPLY_QUEUED" => Some(Self::ReplyQueued),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "QUESTION_OPEN",
            Self::Continued => "QUESTION_CONTINUED",
            Self::Settled => "QUESTION_SETTLED",
            Self::ReplyQueued => "QUESTION_REPLY_QUEUED",
        }
    }

    pub fn is_answerable(self) -> bool {
        matches!(self, Self::Open | Self::Continued)
    }

    pub fn receipt_phase(self) -> &'static str {
        match self {
            Self::Open => "QUESTION_SETTLED",
            _ => "QUESTION_REPLY_QUEUED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineStatus {
    Received,
    HostOffline,
    Rejected,
    HostAccepted,
    Succeeded,
    Failed,
    Unknown,
}

impl OnlineStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::HostOffline => "host_offline",
            Self::Rejected => "rejected",
            Self::HostAccepted => "host_accepted",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::HostOffline | Self::Rejected | Self::Succeeded | Self::Failed | Self::Unknown
        )
    }

    pub fn from_proto(value: i32) -> Option<Self> {
        use whalerein_proto::OnlineStatus as Proto;
        Some(match Proto::try_from(value).ok()? {
            Proto::Received => Self::Received,
            Proto::HostOffline => Self::HostOffline,
            Proto::Rejected => Self::Rejected,
            Proto::HostAccepted => Self::HostAccepted,
            Proto::Succeeded => Self::Succeeded,
            Proto::Failed => Self::Failed,
            Proto::Unknown => Self::Unknown,
            Proto::Unspecified => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub host_id: String,
    pub online: bool,
    pub protocol_version: u32,
    pub fingerprint: String,
}

impl From<whalerein_proto::HostInfo> for HostInfo {
    fn from(value: whalerein_proto::HostInfo) -> Self {
        Self {
            host_id: value.host_id,
            online: value.online,
            protocol_version: value.protocol_version,
            fingerprint: value.fingerprint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalInfo {
    pub approval_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub call_id: String,
    pub reason: String,
    #[serde(with = "hex_bytes")]
    pub digest: Vec<u8>,
    pub version: i64,
    pub expires_at_unix_ms: i64,
    pub state: String,
    pub decision: String,
}

impl From<whalerein_proto::Approval> for ApprovalInfo {
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
            state: enum_label::<whalerein_proto::ApprovalState>(value.state, |state| {
                state.as_str_name()
            }),
            decision: enum_label::<whalerein_proto::ApprovalDecision>(value.decision, |decision| {
                decision.as_str_name()
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCapabilities {
    pub history: bool,
    pub resumable_history: bool,
    pub live_text: bool,
    pub interrupt: bool,
    pub approvals: bool,
    #[serde(default)]
    pub live_reasoning: bool,
    #[serde(default)]
    pub tool_activity: bool,
    #[serde(default)]
    pub conversation_notices: bool,
    #[serde(default)]
    pub attachments: bool,
    #[serde(default)]
    pub history_page: bool,
    #[serde(default)]
    pub history_exact: bool,
    #[serde(default)]
    pub send_followup: bool,
    pub delivery_modes: Vec<String>,
    #[serde(default)]
    pub model_selection: bool,
    #[serde(default)]
    pub pending_queue_read: bool,
    #[serde(default)]
    pub pending_queue_edit: bool,
    #[serde(default)]
    pub pending_queue_remove: bool,
    #[serde(default)]
    pub pending_queue_steer: bool,
    #[serde(default)]
    pub questions_read: bool,
    #[serde(default)]
    pub questions_answer_open: bool,
    #[serde(default)]
    pub questions_answer_continued: bool,
    #[serde(default)]
    pub session_fork: bool,
}
impl From<whalerein_proto::SourceCapabilities> for SessionCapabilities {
    fn from(c: whalerein_proto::SourceCapabilities) -> Self {
        Self {
            history: c.history,
            resumable_history: c.resumable_history,
            live_text: c.live_text,
            interrupt: c.interrupt,
            approvals: c.approvals,
            live_reasoning: c.live_reasoning,
            tool_activity: c.tool_activity,
            conversation_notices: c.conversation_notices,
            attachments: c.attachments,
            history_page: c.history_page,
            history_exact: c.history_exact,
            send_followup: c.send_followup,
            model_selection: c.model_selection,
            pending_queue_read: c.pending_queue_read,
            pending_queue_edit: c.pending_queue_edit,
            pending_queue_remove: c.pending_queue_remove,
            pending_queue_steer: c.pending_queue_steer,
            questions_read: c.questions_read,
            questions_answer_open: c.questions_answer_open,
            questions_answer_continued: c.questions_answer_continued,
            session_fork: c.session_fork,
            delivery_modes: c
                .delivery_modes
                .into_iter()
                .filter_map(|mode| {
                    whalerein_proto::DeliveryMode::try_from(mode)
                        .ok()
                        .map(|m| m.as_str_name().to_owned())
                })
                .collect(),
        }
    }
}
impl From<SessionCapabilities> for whalerein_proto::SourceCapabilities {
    fn from(c: SessionCapabilities) -> Self {
        Self {
            history: c.history,
            resumable_history: c.resumable_history,
            live_text: c.live_text,
            interrupt: c.interrupt,
            approvals: c.approvals,
            live_reasoning: c.live_reasoning,
            tool_activity: c.tool_activity,
            conversation_notices: c.conversation_notices,
            attachments: c.attachments,
            history_page: c.history_page,
            history_exact: c.history_exact,
            send_followup: c.send_followup,
            model_selection: c.model_selection,
            pending_queue_read: c.pending_queue_read,
            pending_queue_edit: c.pending_queue_edit,
            pending_queue_remove: c.pending_queue_remove,
            pending_queue_steer: c.pending_queue_steer,
            questions_read: c.questions_read,
            questions_answer_open: c.questions_answer_open,
            questions_answer_continued: c.questions_answer_continued,
            session_fork: c.session_fork,
            delivery_modes: c
                .delivery_modes
                .iter()
                .filter_map(|mode| {
                    whalerein_proto::DeliveryMode::from_str_name(mode).map(|m| m as i32)
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub running: bool,
    pub agent_available: bool,
    pub blank: bool,
    pub parent_session_id: String,
    pub origin: String,
    pub history_cursor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_capabilities: Option<SessionCapabilities>,
    pub queued_prompts: u32,
    /// The source's own default listing hides this Session; it is still listed
    /// here so a client can offer an archived view.
    pub archived: bool,
}

impl From<whalerein_proto::SessionSummary> for SessionSummary {
    fn from(value: whalerein_proto::SessionSummary) -> Self {
        Self {
            session_id: value.session_id,
            title: value.title,
            cwd: value.cwd,
            created_at_unix_ms: value.created_at_unix_ms,
            updated_at_unix_ms: value.updated_at_unix_ms,
            running: value.running,
            agent_available: value.agent_available,
            blank: value.blank,
            parent_session_id: value.parent_session_id,
            origin: value.origin,
            history_cursor: value.history_cursor,
            source_capabilities: value.source_capabilities.map(Into::into),
            queued_prompts: value.queued_prompts,
            archived: value.archived,
        }
    }
}

impl From<SessionSummary> for whalerein_proto::SessionSummary {
    fn from(value: SessionSummary) -> Self {
        Self {
            session_id: value.session_id,
            title: value.title,
            cwd: value.cwd,
            created_at_unix_ms: value.created_at_unix_ms,
            updated_at_unix_ms: value.updated_at_unix_ms,
            running: value.running,
            agent_available: value.agent_available,
            blank: value.blank,
            parent_session_id: value.parent_session_id,
            origin: value.origin,
            history_cursor: value.history_cursor,
            queued_prompts: value.queued_prompts,
            source_capabilities: value.source_capabilities.map(Into::into),
            archived: value.archived,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
    pub source: String,
    pub complete: bool,
}

impl SessionList {
    pub fn from_proto(value: whalerein_proto::SessionList, source: &str, complete: bool) -> Self {
        Self {
            sessions: value.sessions.into_iter().map(Into::into).collect(),
            source: source.to_owned(),
            complete,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapabilities {
    pub model_catalog: bool,
    pub model_read: bool,
}
impl From<whalerein_proto::HostCapabilities> for HostCapabilities {
    fn from(value: whalerein_proto::HostCapabilities) -> Self {
        Self {
            model_catalog: value.model_catalog,
            model_read: value.model_read,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
}
impl From<whalerein_proto::ModelChoice> for ModelChoice {
    fn from(value: whalerein_proto::ModelChoice) -> Self {
        Self {
            provider: value.provider,
            model: value.model,
            effort: value.effort,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEffort {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub efforts: Vec<ModelEffort>,
    pub default_effort: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProvider {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelEntry>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProviderFailure {
    pub id: String,
    pub name: String,
    pub code: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub default_selection: Option<ModelChoice>,
    pub groups: Vec<ModelProvider>,
    pub failures: Vec<ModelProviderFailure>,
    pub effect_scope: String,
    pub complete: bool,
}
impl From<whalerein_proto::ModelCatalog> for ModelCatalog {
    fn from(value: whalerein_proto::ModelCatalog) -> Self {
        Self {
            default_selection: value.default_selection.map(Into::into),
            effect_scope: value.effect_scope,
            complete: value.complete,
            groups: value
                .groups
                .into_iter()
                .map(|group| ModelProvider {
                    id: group.id,
                    name: group.name,
                    models: group
                        .models
                        .into_iter()
                        .map(|model| ModelEntry {
                            id: model.id,
                            name: model.name,
                            efforts: model
                                .efforts
                                .into_iter()
                                .map(|effort| ModelEffort {
                                    id: effort.id,
                                    name: effort.name,
                                })
                                .collect(),
                            default_effort: model.default_effort,
                        })
                        .collect(),
                })
                .collect(),
            failures: value
                .failures
                .into_iter()
                .map(|failure| ModelProviderFailure {
                    id: failure.id,
                    name: failure.name,
                    code: enum_label::<whalerein_proto::ErrorCode>(failure.code, |code| {
                        code.as_str_name()
                    }),
                })
                .collect(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionModel {
    pub pending: Option<ModelChoice>,
    pub last_used: Option<ModelChoice>,
    pub default_selection: Option<ModelChoice>,
    pub default_is_fallback: bool,
    pub next: Option<ModelChoice>,
    pub pending_known: bool,
}
impl From<whalerein_proto::SessionModel> for SessionModel {
    fn from(value: whalerein_proto::SessionModel) -> Self {
        Self {
            pending: value.pending.map(Into::into),
            last_used: value.last_used.map(Into::into),
            default_selection: value.default_selection.map(Into::into),
            default_is_fallback: value.default_is_fallback,
            next: value.next.map(Into::into),
            pending_known: value.pending_known,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelectionReceipt {
    pub selected: Option<ModelChoice>,
    pub effect_scope: String,
    pub default_persistence: String,
}
impl From<whalerein_proto::ModelSelectionReceipt> for ModelSelectionReceipt {
    fn from(value: whalerein_proto::ModelSelectionReceipt) -> Self {
        Self {
            selected: value.selected.map(Into::into),
            effect_scope: value.effect_scope,
            default_persistence: value.default_persistence,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleReceipt {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub operation: String,
    pub disposition: String,
    pub warning_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CreateSessionInput {
    pub cwd: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_preset: Option<String>,
    pub preferred_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandHandle {
    pub request_id: String,
    pub command_id: String,
    pub status: OnlineStatus,
    pub message_id: Option<String>,
    pub delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelectionReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_receipt: Option<LifecycleReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_receipt: Option<QuestionReceiptInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionReceiptInfo {
    pub batch_id: String,
    pub phase: String,
}

/// Local command counts that survive the CLI process. `non_terminal` is the
/// number of commands still `received` or `host_accepted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDiagnostics {
    pub non_terminal: u64,
    pub unknown: u64,
}

/// Redacted diagnostic snapshot shown by `whalerein doctor`.
///
/// A CLI command exits before the next one starts, so this snapshot only
/// includes facts stored on disk: the paired Access address and command
/// counts. Connection, reconnects, protocol and sync state belong to the
/// process that opened the channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub schema: String,
    pub address: Option<String>,
    pub commands: CommandDiagnostics,
}

/// Manifest returned before/after a diagnostic export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub schema: String,
    pub path: String,
    pub fields: Vec<String>,
    pub redactions: Vec<String>,
    pub bytes: u64,
}

impl ExportManifest {
    pub fn fields() -> Vec<String> {
        vec![
            "address".into(),
            "commands.non_terminal".into(),
            "commands.unknown".into(),
            "last_failure".into(),
        ]
    }

    pub fn redactions() -> Vec<String> {
        vec![
            "message content".into(),
            "tool arguments".into(),
            "private keys".into(),
            "device tokens".into(),
            "pairing tokens".into(),
            "full ciphertext".into(),
            "host and device ids".into(),
        ]
    }
}

/// Pairing prompt shown to the user before msg3 is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPrompt {
    pub host_id: String,
    pub access_url: String,
    pub fingerprint: String,
    pub fingerprint_display: String,
}

/// Whether unconfirmed command identities could be durably recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationRetention {
    Recorded,
    Unverified,
}

/// Confirmed revocation and its command-identity retention outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationOutcome {
    pub device_id: String,
    pub retention: RevocationRetention,
}

/// Redacted evidence of a pairing requiring review or having a known outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingHistoryRecord {
    pub claim_id: String,
    pub host_id: String,
    pub fingerprint: String,
    pub status: String,
}

/// Result of a successful pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub host_id: String,
    pub device_id: String,
    pub fingerprint: String,
    pub access_url: String,
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        hex::decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_status_names_are_stable() {
        assert_eq!(OnlineStatus::HostOffline.as_str(), "host_offline");
        assert!(OnlineStatus::Unknown.is_terminal());
        assert!(!OnlineStatus::HostAccepted.is_terminal());
        assert_eq!(
            OnlineStatus::from_proto(whalerein_proto::OnlineStatus::Succeeded as i32),
            Some(OnlineStatus::Succeeded)
        );
        assert_eq!(OnlineStatus::from_proto(0), None);
        assert_eq!(OnlineStatus::from_proto(999), None);
    }

    #[test]
    fn preserves_session_summary_fields_through_proto_conversion() {
        let summary = SessionSummary {
            session_id: "s-1".into(),
            title: "title".into(),
            cwd: "/tmp".into(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            running: true,
            agent_available: true,
            blank: false,
            parent_session_id: String::new(),
            origin: "cli".into(),
            history_cursor: "source-head".into(),
            source_capabilities: None,
            queued_prompts: 2,
            archived: true,
        };
        let proto: whalerein_proto::SessionSummary = summary.clone().into();
        assert_eq!(SessionSummary::from(proto), summary);
    }

    #[test]
    fn export_manifest_lists_fields_and_redactions() {
        let fields = ExportManifest::fields();
        assert_eq!(
            fields,
            vec![
                "address".to_owned(),
                "commands.non_terminal".to_owned(),
                "commands.unknown".to_owned(),
                "last_failure".to_owned(),
            ]
        );
        let redactions = ExportManifest::redactions();
        assert!(redactions.contains(&"private keys".into()));
        assert!(redactions.contains(&"full ciphertext".into()));
    }

    #[test]
    fn converts_proto_host_info_to_public_host_info() {
        let host: HostInfo = whalerein_proto::HostInfo {
            host_id: "h".into(),
            online: true,
            protocol_version: 1,
            fingerprint: "f".into(),
        }
        .into();
        assert_eq!(host.host_id, "h");
    }

    #[test]
    fn unknown_enums_never_project_as_the_zero_value() {
        let approval: ApprovalInfo = whalerein_proto::Approval {
            state: 99,
            decision: 98,
            ..Default::default()
        }
        .into();
        assert_eq!(approval.state, "UNKNOWN_ENUM_99");
        assert_eq!(approval.decision, "UNKNOWN_ENUM_98");

        let unspecified: ApprovalInfo = whalerein_proto::Approval::default().into();
        assert_eq!(unspecified.state, "APPROVAL_STATE_UNSPECIFIED");
        assert_eq!(unspecified.decision, "APPROVAL_DECISION_UNSPECIFIED");

        let batch: QuestionBatchInfo = whalerein_proto::QuestionBatch {
            phase: 99,
            ..Default::default()
        }
        .into();
        assert_eq!(batch.phase, "UNKNOWN_ENUM_99");

        let queue: PendingQueue = whalerein_proto::PendingQueue {
            items: vec![whalerein_proto::PendingQueueItem {
                placement: 97,
                ..Default::default()
            }],
            ..Default::default()
        }
        .into();
        assert_eq!(queue.items[0].placement, "UNKNOWN_ENUM_97");

        let catalog: ModelCatalog = whalerein_proto::ModelCatalog {
            failures: vec![whalerein_proto::ModelProviderFailure {
                id: "p".into(),
                name: "p".into(),
                code: 96,
            }],
            ..Default::default()
        }
        .into();
        assert_eq!(catalog.failures[0].code, "UNKNOWN_ENUM_96");
    }

    #[test]
    fn capability_projection_keeps_every_flag_and_never_invents_a_delivery_mode() {
        let capabilities: SessionCapabilities = whalerein_proto::SourceCapabilities {
            history: true,
            resumable_history: true,
            live_text: true,
            interrupt: true,
            approvals: true,
            delivery_modes: vec![whalerein_proto::DeliveryMode::StartIfIdle as i32, 99],
            model_selection: true,
            pending_queue_read: true,
            pending_queue_edit: false,
            pending_queue_remove: false,
            pending_queue_steer: true,
            questions_read: true,
            questions_answer_open: false,
            questions_answer_continued: true,
            session_fork: true,
            ..Default::default()
        }
        .into();
        assert!(capabilities.history);
        assert!(capabilities.model_selection);
        assert!(capabilities.pending_queue_read);
        assert!(!capabilities.pending_queue_edit);
        assert!(capabilities.pending_queue_steer);
        assert!(capabilities.questions_read);
        assert!(!capabilities.questions_answer_open);
        assert!(capabilities.questions_answer_continued);
        assert!(capabilities.session_fork);
        // An unknown delivery mode is not advertised as a supported mode.
        assert_eq!(capabilities.delivery_modes, vec!["START_IF_IDLE"]);
        let back: whalerein_proto::SourceCapabilities = capabilities.into();
        assert_eq!(back.delivery_modes.len(), 1);
        assert!(!back.pending_queue_edit);
    }

    #[test]
    fn shared_identifier_and_clock_contract_is_enforced_before_a_call() {
        use whalerein_proto::{validate_id, IdKind};
        assert!(validate_id(IdKind::Request, "req-1").is_ok());
        assert!(validate_id(IdKind::Request, "req|1").is_err());
        assert!(validate_id(IdKind::Session, "session/ü").is_ok());
        assert!(validate_id(IdKind::Command, "").is_ok());
        assert!(validate_id(IdKind::Request, "").is_err());
        assert!(validate_id(IdKind::Request, &"a".repeat(129)).is_err());

        let now = 1_000_000;
        assert!(whalerein_proto::validate_pairing_signature_time(now - 120_000, now).is_ok());
        assert!(whalerein_proto::validate_pairing_signature_time(now - 120_001, now).is_err());
        assert!(!whalerein_proto::is_expired(now + 1, now));
        assert!(whalerein_proto::is_expired(now, now));
    }

    #[test]
    fn converts_proto_session_list_with_source_and_completeness() {
        let list = SessionList::from_proto(
            whalerein_proto::SessionList {
                sessions: vec![whalerein_proto::SessionSummary {
                    session_id: "s".into(),
                    ..Default::default()
                }],
            },
            "dsh",
            true,
        );
        assert_eq!(list.sessions.len(), 1);
        assert_eq!(list.source, "dsh");
        assert!(list.complete);
    }
}

/// Durable routing and identity for a command that may need result recovery.
/// Contains no message content, tool arguments, credentials, or channel keys.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingCommand {
    pub access_url: String,
    pub host_fingerprint: String,
    pub request_id: String,
    pub command_id: String,
    pub host_id: String,
    pub device_id: String,
    pub status: OnlineStatus,
}

/// State-only observation returned by a native-backed session wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitObservation {
    pub session_id: String,
    pub reason: WaitReason,
    pub state: WaitState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Idle,
    InputRequired,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitState {
    pub active: bool,
    pub running: bool,
    pub queue_available: bool,
    pub pending_input: Vec<WaitInput>,
}

/// Evidenced pending-input kinds shared by wait and send follow-up. They are
/// informational and never answer an approval or question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Approval,
    Question,
    Other,
}

/// Existing wait vocabulary; kept as an alias of [`InputKind`].
pub type WaitInput = InputKind;

/// An online observation; queue contents are not cached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingQueue {
    pub session_id: String,
    pub available: bool,
    pub running: bool,
    pub items: Vec<PendingQueueItem>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingQueueItem {
    pub item_id: String,
    pub placement: String,
    pub delivery_id: Option<String>,
    pub blocks: Vec<PendingQueueBlock>,
    pub actions: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PendingQueueBlock {
    Text {
        text: String,
    },
    Attachment {
        media_type: String,
        name: String,
        size: u64,
        available: bool,
    },
    Unavailable,
}
impl From<whalerein_proto::PendingQueue> for PendingQueue {
    fn from(value: whalerein_proto::PendingQueue) -> Self {
        use whalerein_proto::content_block::Content;
        Self {
            session_id: value.session_id,
            available: value.available,
            running: value.running,
            items: value
                .items
                .into_iter()
                .map(|item| PendingQueueItem {
                    item_id: item.item_id,
                    placement: enum_label::<whalerein_proto::QueuePlacement>(
                        item.placement,
                        |placement| placement.as_str_name(),
                    ),
                    delivery_id: if item.delivery_id.is_empty() {
                        None
                    } else {
                        Some(item.delivery_id)
                    },
                    blocks: item
                        .blocks
                        .into_iter()
                        .map(|block| match block.content {
                            Some(Content::Text(text)) => {
                                PendingQueueBlock::Text { text: text.text }
                            }
                            Some(Content::Attachment(attachment)) => {
                                PendingQueueBlock::Attachment {
                                    media_type: attachment.media_type,
                                    name: attachment.name,
                                    size: attachment.size,
                                    available: attachment.available,
                                }
                            }
                            _ => PendingQueueBlock::Unavailable,
                        })
                        .collect(),
                    actions: item
                        .actions
                        .into_iter()
                        .filter_map(|action| whalerein_proto::QueueAction::try_from(action).ok())
                        .map(|action| action.as_str_name().to_owned())
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingQueueAction {
    Edit,
    Remove,
    Promote,
}
impl From<PendingQueueAction> for whalerein_proto::QueueAction {
    fn from(value: PendingQueueAction) -> Self {
        match value {
            PendingQueueAction::Edit => Self::Edit,
            PendingQueueAction::Remove => Self::Remove,
            PendingQueueAction::Promote => Self::Promote,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOptionInfo {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionSubjectInfo {
    pub name: String,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionDisplayInfo {
    pub question: Option<std::collections::HashMap<String, String>>,
    pub detail: Option<std::collections::HashMap<String, String>>,
    pub approve: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionIntentInfo {
    pub kind: String,
    pub approve: String,
    pub reject: Option<String>,
    pub always: Option<String>,
    pub call_id: Option<String>,
    pub subject: Option<QuestionSubjectInfo>,
    pub display: Option<QuestionDisplayInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionItemInfo {
    pub id: String,
    pub question: String,
    pub detail: Option<String>,
    pub header: Option<String>,
    pub options: Vec<QuestionOptionInfo>,
    pub multi_select: bool,
    pub intent: Option<QuestionIntentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub selected: Vec<String>,
    pub custom: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionBatchInfo {
    pub session_id: String,
    pub batch_id: String,
    pub source_epoch: String,
    pub version: u64,
    #[serde(with = "hex_bytes")]
    pub digest: Vec<u8>,
    pub phase: String,
    pub questions: Vec<QuestionItemInfo>,
    pub answerable: bool,
    pub content_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionPageInfo {
    pub batches: Vec<QuestionBatchInfo>,
    pub next_cursor: String,
    /// Stamp of the complete inventory this page was derived from. Present on
    /// the new contract; a missing stamp never establishes a current view.
    pub stamp: Option<QuestionStamp>,
    /// True only on the final page of the same stamped inventory.
    pub complete: bool,
    /// Encoded wire size of this page (including its stamp, cursor and batches
    /// but excluding the outer HostResult envelope). Core charges the complete
    /// staged collection against the existing logical-envelope budget.
    pub encoded_bytes: usize,
}

/// Which facets one session observation should deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationInterests {
    pub conversation: bool,
    pub questions: bool,
    #[serde(default)]
    pub(crate) recent_page: Option<(u32, u32)>,
}

impl ObservationInterests {
    pub fn conversation() -> Self {
        Self {
            conversation: true,
            recent_page: None,
            questions: false,
        }
    }

    pub fn questions() -> Self {
        Self {
            conversation: false,
            recent_page: None,
            questions: true,
        }
    }

    pub fn both() -> Self {
        Self {
            conversation: true,
            recent_page: None,
            questions: true,
        }
    }

    /// Observe a bounded recent conversation and Questions.
    pub fn paged_conversation(max_messages: u32, max_bytes: u32) -> Self {
        Self {
            recent_page: Some((max_messages, max_bytes)),
            ..Self::both()
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.conversation && !self.questions
    }
}

impl From<ObservationInterests> for whalerein_proto::ObservationInterests {
    fn from(value: ObservationInterests) -> Self {
        Self {
            conversation: value.conversation,
            questions: value.questions,
        }
    }
}

/// Source observation epoch plus monotonic normalized-inventory revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionStamp {
    pub observation_epoch: String,
    pub revision: u64,
}

impl From<whalerein_proto::QuestionStamp> for QuestionStamp {
    fn from(value: whalerein_proto::QuestionStamp) -> Self {
        Self {
            observation_epoch: value.observation_epoch,
            revision: value.revision,
        }
    }
}

impl From<QuestionStamp> for whalerein_proto::QuestionStamp {
    fn from(value: QuestionStamp) -> Self {
        Self {
            observation_epoch: value.observation_epoch,
            revision: value.revision,
        }
    }
}

/// Readiness of the question facet. Only `Current` can establish an empty
/// pending collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionFreshness {
    Loading,
    Current,
    Stale,
    Unavailable,
    Unsupported,
}

/// Core-owned question facet state delivered by one observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionView {
    pub freshness: QuestionFreshness,
    pub stamp: Option<QuestionStamp>,
    pub batches: Vec<QuestionBatchInfo>,
    /// Stable failure code, never question content.
    pub error: Option<String>,
}

impl QuestionView {
    pub fn loading() -> Self {
        Self {
            freshness: QuestionFreshness::Loading,
            stamp: None,
            batches: Vec::new(),
            error: None,
        }
    }

    pub fn current(stamp: QuestionStamp, batches: Vec<QuestionBatchInfo>) -> Self {
        Self {
            freshness: QuestionFreshness::Current,
            stamp: Some(stamp),
            batches,
            error: None,
        }
    }

    pub fn stale(
        stamp: Option<QuestionStamp>,
        batches: Vec<QuestionBatchInfo>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            freshness: QuestionFreshness::Stale,
            stamp,
            batches,
            error: Some(error.into()),
        }
    }

    pub fn unavailable(
        stamp: Option<QuestionStamp>,
        batches: Vec<QuestionBatchInfo>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            freshness: QuestionFreshness::Unavailable,
            stamp,
            batches,
            error: Some(error.into()),
        }
    }

    pub fn unsupported(error: impl Into<String>) -> Self {
        Self {
            freshness: QuestionFreshness::Unsupported,
            stamp: None,
            batches: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// One producer-scoped observation event.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub session: crate::session::SessionKey,
    pub observation_id: String,
    pub generation: u64,
    pub payload: SessionEventPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEventPayload {
    Conversation(crate::conversation::ViewUpdate),
    Questions(QuestionView),
    HistorySnapshot(HistorySnapshot),
    HistoryUnavailable { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySnapshot {
    pub page: crate::history::HistoryPage,
    pub continuity: HistoryContinuity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryContinuity {
    /// Core verified continuity from the previous range in this observation.
    Continuous,
    /// Install this window atomically. Includes the first delivered range.
    Replacement,
}

impl From<whalerein_proto::QuestionBatch> for QuestionBatchInfo {
    fn from(value: whalerein_proto::QuestionBatch) -> Self {
        Self {
            session_id: value.session_id,
            batch_id: value.batch_id,
            source_epoch: value.source_epoch,
            version: value.version,
            digest: value.digest,
            phase: enum_label::<whalerein_proto::QuestionPhase>(value.phase, |phase| {
                phase.as_str_name()
            }),
            questions: value
                .questions
                .into_iter()
                .map(|item| QuestionItemInfo {
                    id: item.id,
                    question: item.question,
                    detail: item.detail,
                    header: item.header,
                    options: item
                        .options
                        .into_iter()
                        .map(|option| QuestionOptionInfo {
                            label: option.label,
                            description: option.description,
                        })
                        .collect(),
                    multi_select: item.multi_select,
                    intent: item.intent.map(|intent| QuestionIntentInfo {
                        kind: intent.kind,
                        approve: intent.approve,
                        reject: intent.reject,
                        always: intent.always,
                        call_id: intent.call_id,
                        subject: intent.subject.map(|subject| QuestionSubjectInfo {
                            name: subject.name,
                            icon: subject.icon,
                        }),
                        display: intent.display.map(|display| QuestionDisplayInfo {
                            question: display.question.map(|value| value.translations),
                            detail: display.detail.map(|value| value.translations),
                            approve: display.approve.map(|value| value.translations),
                        }),
                    }),
                })
                .collect(),
            answerable: value.answerable,
            content_unavailable: value.content_unavailable,
        }
    }
}

impl QuestionBatchInfo {
    /// Typed phase for client validation. An unknown wire label stays `None`
    /// so a newer Host cannot be silently treated as answerable.
    pub fn phase_value(&self) -> Option<QuestionPhase> {
        QuestionPhase::parse(&self.phase)
    }
}

/// Shared cancellation flag for one observation or command task.
///
/// The flag is cloneable and visible to every task the caller started; the
/// internal notification wakes an idle observation so cancellation does not
/// wait for the next refresh tick.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(std::sync::Arc<CancellationState>);

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0
            .cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.0.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}
