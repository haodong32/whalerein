//! Stable Core-facing domain types.
//!
//! These types are not HTTP DTOs. They are the projection that CLI and future
//! bindings consume, so proto changes stay inside Core.

use serde::{Deserialize, Serialize};

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
            state: format!(
                "{:?}",
                whalerein_proto::ApprovalState::try_from(value.state).unwrap_or_default()
            ),
            decision: format!(
                "{:?}",
                whalerein_proto::ApprovalDecision::try_from(value.decision).unwrap_or_default()
            ),
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
    pub delivery_modes: Vec<String>,
}
impl From<whalerein_proto::SourceCapabilities> for SessionCapabilities {
    fn from(c: whalerein_proto::SourceCapabilities) -> Self {
        Self {
            history: c.history,
            resumable_history: c.resumable_history,
            live_text: c.live_text,
            interrupt: c.interrupt,
            approvals: c.approvals,
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
pub struct CommandHandle {
    pub request_id: String,
    pub command_id: String,
    pub status: OnlineStatus,
    pub message_id: Option<String>,
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

/// Result of a successful pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOutcome {
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
