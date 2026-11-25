//! Flat, stable errors crossing the UniFFI boundary.
//!
//! Every variant maps to one stable `code` string. Messages are fixed and
//! localized-neutral; Core error text is never forwarded because it may carry
//! Host-supplied content, invitations or endpoints. Raw failures stay in the
//! redacted diagnostics preview instead.

use whalerein_core::CoreError;

/// Stable error codes exposed to Swift.
pub const CODE_INVALID_INPUT: &str = "invalid_input";
pub const CODE_NOT_PAIRED: &str = "not_paired";
pub const CODE_NOT_FOUND: &str = "not_found";
pub const CODE_REVOKED: &str = "revoked";
pub const CODE_HOST_OFFLINE: &str = "host_offline";
pub const CODE_UNAVAILABLE: &str = "unavailable";
pub const CODE_CONFLICT: &str = "conflict";
pub const CODE_TIMEOUT: &str = "timeout";
pub const CODE_CANCELLED: &str = "cancelled";
/// A history continuation or exact-message reference is stale, expired or
/// belongs to another scope or content projection. The caller must restart the
/// traversal instead of retrying the same token.
pub const CODE_STALE_HISTORY: &str = "stale_history";
pub const CODE_SECURITY: &str = "security";
pub const CODE_INTERNAL: &str = "internal";
/// The Host speaks a protocol line this client cannot use. The only recovery is
/// an upgrade, so this must never be presented as a retryable outage.
pub const CODE_PROTOCOL_VERSION_INCOMPATIBLE: &str = "protocol_version_incompatible";
/// The Host does not implement a capability the app needs. Upgrading the Host is
/// the only recovery; retrying the same request cannot succeed.
pub const CODE_UNSUPPORTED_CAPABILITY: &str = "unsupported_capability";

/// One flat error type for every mobile entry point.
///
/// `message` is the fixed, user-presentable explanation. `code()` returns the
/// stable machine-readable value used by Swift for branching and logs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum MobileError {
    #[error("{message}")]
    InvalidInput { message: String },
    #[error("{message}")]
    NotPaired { message: String },
    #[error("{message}")]
    NotFound { message: String },
    #[error("{message}")]
    Revoked { message: String },
    #[error("{message}")]
    HostOffline { message: String },
    #[error("{message}")]
    Unavailable { message: String },
    #[error("{message}")]
    Conflict { message: String },
    #[error("{message}")]
    Timeout { message: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    StaleHistory { message: String },
    #[error("{message}")]
    Security { message: String },
    #[error("{message}")]
    Internal { message: String },
    /// Non-retryable upgrade prompt: the Host's protocol line is incompatible.
    #[error("{message}")]
    ProtocolVersionIncompatible { message: String },
    /// Non-retryable upgrade prompt: the Host lacks a required capability.
    #[error("{message}")]
    UnsupportedCapability { message: String },
}

macro_rules! constructors {
    ($($name:ident => ($variant:ident, $message:literal)),+ $(,)?) => {
        impl MobileError {
            $(
                pub(crate) fn $name() -> Self {
                    Self::$variant {
                        message: $message.to_owned(),
                    }
                }
            )+
        }
    };
}

constructors! {
    invalid_input => (InvalidInput, "The request is invalid."),
    not_paired => (NotPaired, "This device is not paired with a host."),
    not_found => (NotFound, "The requested item was not found."),
    revoked => (Revoked, "This device's host access was revoked."),
    host_offline => (HostOffline, "The host is offline."),
    unavailable => (Unavailable, "The requested operation is unavailable."),
    conflict => (Conflict, "The operation conflicts with the current state."),
    timeout => (Timeout, "The operation timed out."),
    cancelled => (Cancelled, "The operation was cancelled."),
    stale_history => (
        StaleHistory,
        "This history view is out of date. Load the latest messages again."
    ),
    security => (Security, "A security check failed."),
    internal => (Internal, "An internal error occurred."),
    protocol_version_incompatible => (
        ProtocolVersionIncompatible,
        "This host uses an incompatible protocol version. Update WhaleRein on the host."
    ),
    unsupported_capability => (
        UnsupportedCapability,
        "This host does not support a required capability. Update WhaleRein on the host."
    ),
}

impl MobileError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput { .. } => CODE_INVALID_INPUT,
            Self::NotPaired { .. } => CODE_NOT_PAIRED,
            Self::NotFound { .. } => CODE_NOT_FOUND,
            Self::Revoked { .. } => CODE_REVOKED,
            Self::HostOffline { .. } => CODE_HOST_OFFLINE,
            Self::Unavailable { .. } => CODE_UNAVAILABLE,
            Self::Conflict { .. } => CODE_CONFLICT,
            Self::Timeout { .. } => CODE_TIMEOUT,
            Self::Cancelled { .. } => CODE_CANCELLED,
            Self::StaleHistory { .. } => CODE_STALE_HISTORY,
            Self::Security { .. } => CODE_SECURITY,
            Self::Internal { .. } => CODE_INTERNAL,
            Self::ProtocolVersionIncompatible { .. } => CODE_PROTOCOL_VERSION_INCOMPATIBLE,
            Self::UnsupportedCapability { .. } => CODE_UNSUPPORTED_CAPABILITY,
        }
    }

    /// Neutral explanation safe to present and log.
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidInput { message }
            | Self::NotPaired { message }
            | Self::NotFound { message }
            | Self::Revoked { message }
            | Self::HostOffline { message }
            | Self::Unavailable { message }
            | Self::Conflict { message }
            | Self::Timeout { message }
            | Self::Cancelled { message }
            | Self::StaleHistory { message }
            | Self::Security { message }
            | Self::Internal { message }
            | Self::ProtocolVersionIncompatible { message }
            | Self::UnsupportedCapability { message } => message,
        }
    }
}

/// Maps a Core failure onto the flat contract without forwarding its text.
pub(crate) fn from_core(error: &CoreError) -> MobileError {
    from_core_code(error.code())
}

pub(crate) fn from_core_code(code: &str) -> MobileError {
    match code {
        "invalid_argument" | "invalid_configuration" => MobileError::invalid_input(),
        "not_paired" => MobileError::not_paired(),
        "not_found" => MobileError::not_found(),
        "revoked" | "revoked_retention_unverified" => MobileError::revoked(),
        "host_offline" => MobileError::host_offline(),
        "conflict"
        | "approval_expired"
        | "approval_version_mismatch"
        | "approval_already_resolved"
        | "approval_unavailable" => MobileError::conflict(),
        "timeout" | "unknown" => MobileError::timeout(),
        "cancelled" | "pairing_fenced" => MobileError::cancelled(),
        "security_error"
        | "host_fingerprint_mismatch"
        | "unauthorized"
        | "forbidden"
        | "payload_digest_mismatch" => MobileError::security(),
        "snapshot_required" => MobileError::stale_history(),
        "unavailable"
        | "rejected"
        | "expired"
        | "revocations_pending"
        | "channel_mismatch"
        | "channel_replaced"
        | "rate_limited"
        | "pairing_committed_unavailable"
        | "pairing_committed_unconfirmed" => MobileError::unavailable(),
        "protocol_version_incompatible" => MobileError::protocol_version_incompatible(),
        "unsupported_capability" => MobileError::unsupported_capability(),
        "payload_too_large" => MobileError::invalid_input(),
        _ => MobileError::internal(),
    }
}

impl From<CoreError> for MobileError {
    fn from(value: CoreError) -> Self {
        from_core(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<MobileError> {
        vec![
            MobileError::invalid_input(),
            MobileError::not_paired(),
            MobileError::not_found(),
            MobileError::revoked(),
            MobileError::host_offline(),
            MobileError::unavailable(),
            MobileError::conflict(),
            MobileError::timeout(),
            MobileError::cancelled(),
            MobileError::stale_history(),
            MobileError::security(),
            MobileError::internal(),
            MobileError::protocol_version_incompatible(),
            MobileError::unsupported_capability(),
        ]
    }

    #[test]
    fn codes_are_stable_and_unique() {
        let codes: Vec<&str> = all().iter().map(MobileError::code).collect();
        assert_eq!(
            codes,
            vec![
                "invalid_input",
                "not_paired",
                "not_found",
                "revoked",
                "host_offline",
                "unavailable",
                "conflict",
                "timeout",
                "cancelled",
                "stale_history",
                "security",
                "internal",
                "protocol_version_incompatible",
                "unsupported_capability",
            ]
        );
        let mut unique = codes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
    }

    #[test]
    fn core_errors_map_to_the_documented_categories() {
        let cases = [
            (CoreError::InvalidInput("x".into()), "invalid_input"),
            (CoreError::NotPaired, "not_paired"),
            (CoreError::Revoked, "revoked"),
            (CoreError::RevokedRetentionUnverified, "revoked"),
            (CoreError::HostOffline, "host_offline"),
            (CoreError::Timeout, "timeout"),
            (CoreError::UnknownResult, "timeout"),
            (CoreError::Conflict("x".into()), "conflict"),
            (CoreError::PairingCancelled, "cancelled"),
            (
                CoreError::Api {
                    code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
                    message: "x".into(),
                },
                "stale_history",
            ),
            (CoreError::Security("x".into()), "security"),
            (
                CoreError::FingerprintMismatch {
                    expected: "a".into(),
                    actual: "b".into(),
                },
                "security",
            ),
            (CoreError::Unauthorized, "security"),
            (
                CoreError::ProtocolIncompatible,
                "protocol_version_incompatible",
            ),
            (CoreError::Unsupported, "unsupported_capability"),
            (CoreError::Unavailable("x".into()), "unavailable"),
            (CoreError::NotFound("x".into()), "not_found"),
            (CoreError::Internal("x".into()), "internal"),
            (CoreError::Cache("x".into()), "internal"),
        ];
        for (error, code) in cases {
            assert_eq!(from_core(&error).code(), code, "{error:?}");
        }
    }

    #[test]
    fn messages_never_contain_raw_core_text() {
        let token = "pairing-token-7d1f";
        let uri = "whalerein://pair?token=secret&host=h";
        let content = "please delete the production database";
        let errors = [
            CoreError::InvalidInput(format!("{uri} {token} {content}")),
            CoreError::Security(format!("{uri} {token}")),
            CoreError::Unavailable(content.to_owned()),
            CoreError::Host {
                code: None,
                message: format!("{uri} {token} {content}"),
            },
            CoreError::Source {
                code: None,
                message: content.to_owned(),
            },
            CoreError::Api {
                code: None,
                message: token.to_owned(),
            },
        ];
        for error in errors {
            let mapped = from_core(&error);
            let message = mapped.message();
            for forbidden in [token, uri, content, "secret", "h=", "delete"] {
                assert!(
                    !message.contains(forbidden),
                    "{} leaked into {message}",
                    forbidden
                );
            }
            assert!(!mapped.to_string().contains(token));
        }
    }

    #[test]
    fn user_facing_messages_are_nonempty_and_neutral() {
        for error in all() {
            assert!(!error.message().is_empty());
            assert!(!error.message().contains("http"));
            assert!(!error.message().contains("token"));
        }
    }
}
