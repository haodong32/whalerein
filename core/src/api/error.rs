//! Stable Core error type and wire/exit-code mapping.

use whalerein_proto::ErrorCode;

use crate::access::{AccessError, AccessErrorKind};
use crate::cache::CacheError;
use crate::configuration::ConfigurationError;
use crate::pairing::PairingError;
use crate::security::SecurityError;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("fresh Cloud authentication is required")]
    AuthenticationRequired,
    #[error("Cloud authorization has expired")]
    AuthorizationExpired,
    #[error("device is revoked")]
    Revoked,
    #[error(
        "device is revoked; unable to verify whether unconfirmed command or pairing identities were retained"
    )]
    RevokedRetentionUnverified,
    #[error("host is offline")]
    HostOffline,
    #[error("host rejected the request")]
    HostRejected,
    #[error("request timed out")]
    Timeout,
    #[error("command result is unknown")]
    UnknownResult,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unsupported capability")]
    Unsupported,
    #[error("protocol version incompatible")]
    ProtocolIncompatible,
    #[error("pairing was cancelled")]
    PairingCancelled,
    #[error("pairing intent was invalidated before commit")]
    PairingFenced,
    #[error("pairing committed but its original grant is unavailable")]
    PairingCommittedUnavailable,
    #[error("pairing committed but Host confirmation is unknown")]
    PairingCommittedUnconfirmed,
    #[error("host fingerprint does not match")]
    FingerprintMismatch { expected: String, actual: String },
    #[error("client is not paired")]
    NotPaired,
    #[error("result belongs to an old secure channel")]
    ChannelMismatch,
    #[error("command result must be recovered over a new channel")]
    StaleCommand {
        request_id: String,
        command_id: String,
    },
    #[error("Host error: {message}")]
    Host {
        code: Option<ErrorCode>,
        message: String,
    },
    #[error("Source error: {message}")]
    Source {
        code: Option<ErrorCode>,
        message: String,
    },
    #[error("Access error: {message}")]
    Api {
        code: Option<ErrorCode>,
        message: String,
    },
    #[error("service unavailable: {0}")]
    Unavailable(String),
    #[error("cache error: {0}")]
    Cache(String),
    #[error("security error: {0}")]
    Security(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("client environment: {0}")]
    Configuration(Box<ConfigurationError>),
}

impl From<ConfigurationError> for CoreError {
    fn from(error: ConfigurationError) -> Self {
        Self::Configuration(Box::new(error))
    }
}

impl CoreError {
    /// Stable machine-readable error code for the CLI JSON envelope.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_argument",
            Self::NotFound(_) => "not_found",
            Self::Unauthorized => "unauthorized",
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthorizationExpired => "authorization_expired",
            Self::Revoked | Self::RevokedRetentionUnverified => "revoked",
            Self::HostOffline => "host_offline",
            Self::HostRejected => "rejected",
            Self::Timeout => "timeout",
            Self::UnknownResult => "unknown",
            Self::Conflict(_) => "conflict",
            Self::Unsupported => "unsupported_capability",
            Self::ProtocolIncompatible => "protocol_version_incompatible",
            Self::PairingCancelled => "cancelled",
            Self::PairingFenced => "pairing_fenced",
            Self::PairingCommittedUnavailable => "pairing_committed_unavailable",
            Self::PairingCommittedUnconfirmed => "pairing_committed_unconfirmed",
            Self::FingerprintMismatch { .. } => "host_fingerprint_mismatch",
            Self::NotPaired => "not_paired",
            Self::ChannelMismatch | Self::StaleCommand { .. } => "channel_mismatch",
            Self::Host {
                code: Some(code), ..
            }
            | Self::Source {
                code: Some(code), ..
            }
            | Self::Api {
                code: Some(code), ..
            } => error_code_name(*code),
            Self::Host { code: None, .. }
            | Self::Source { code: None, .. }
            | Self::Api { code: None, .. } => "internal",
            Self::Unavailable(_) => "unavailable",
            Self::Cache(_) => "cache_error",
            Self::Security(_) => "security_error",
            Self::Internal(_) => "internal",
            Self::Config(_) => "invalid_configuration",
            Self::Configuration(error) => error.code(),
        }
    }

    /// The boundary that produced this error, independent of its wire code.
    pub fn source_layer(&self) -> &'static str {
        match self {
            Self::Api { .. }
            | Self::PairingFenced
            | Self::PairingCommittedUnavailable
            | Self::PairingCommittedUnconfirmed => "access",
            Self::Host { .. } | Self::HostRejected => "host",
            Self::Source { .. } => "source",
            Self::Security(_) | Self::FingerprintMismatch { .. } => "security",
            Self::Cache(_) => "storage",
            Self::Unavailable(_) => "transport",
            Self::AuthenticationRequired | Self::AuthorizationExpired => "cloud",
            Self::Revoked | Self::RevokedRetentionUnverified => "access",
            _ => "core",
        }
    }

    /// Stable process exit code from WIRE.md.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Host {
                code: Some(code), ..
            }
            | Self::Source {
                code: Some(code), ..
            }
            | Self::Api {
                code: Some(code), ..
            } => error_code_exit_code(*code),
            Self::InvalidInput(_) | Self::Config(_) | Self::Configuration(_) => 2,
            Self::NotFound(_) => 3,
            Self::Unauthorized
            | Self::AuthenticationRequired
            | Self::AuthorizationExpired
            | Self::Revoked
            | Self::RevokedRetentionUnverified
            | Self::NotPaired => 4,
            Self::HostOffline | Self::Unavailable(_) => 5,
            Self::HostRejected => 6,
            Self::Timeout | Self::UnknownResult => 7,
            Self::Conflict(_)
            | Self::PairingFenced
            | Self::PairingCommittedUnavailable
            | Self::PairingCommittedUnconfirmed
            | Self::FingerprintMismatch { .. }
            | Self::ChannelMismatch
            | Self::StaleCommand { .. } => 8,
            Self::Unsupported | Self::ProtocolIncompatible => 9,
            Self::PairingCancelled => 2,
            Self::Cache(_) | Self::Security(_) | Self::Internal(_) => 1,
            Self::Host { code: None, .. }
            | Self::Source { code: None, .. }
            | Self::Api { code: None, .. } => 1,
        }
    }

    /// Stable client action for this error.
    ///
    /// Clients use this instead of guessing from the exit code. `upgrade` is
    /// terminal for the current attempt: a mobile or CLI client must show an
    /// upgrade prompt and must never map it to a retryable `unavailable`.
    pub fn action(&self) -> &'static str {
        match self {
            Self::Host {
                code: Some(code), ..
            }
            | Self::Source {
                code: Some(code), ..
            }
            | Self::Api {
                code: Some(code), ..
            } => error_code_action(*code),
            Self::InvalidInput(_) | Self::Config(_) => "fix_input",
            Self::Configuration(error) if error.code() == "revision_conflict" => "refresh",
            Self::Configuration(_) => "fix_input",
            Self::NotFound(_) => "refresh",
            Self::Unauthorized
            | Self::AuthenticationRequired
            | Self::AuthorizationExpired
            | Self::Revoked
            | Self::RevokedRetentionUnverified
            | Self::NotPaired => "reauth",
            Self::HostOffline | Self::Unavailable(_) => "retry",
            Self::HostRejected => "refresh",
            Self::Timeout | Self::UnknownResult => "retry",
            Self::Conflict(_) | Self::FingerprintMismatch { .. } => "refresh",
            Self::Unsupported | Self::ProtocolIncompatible => "upgrade",
            Self::PairingCancelled | Self::PairingFenced | Self::PairingCommittedUnavailable => {
                "restart_pairing"
            }
            Self::PairingCommittedUnconfirmed => "review_pairing",
            Self::ChannelMismatch | Self::StaleCommand { .. } => "retry",
            Self::Host { code: None, .. }
            | Self::Source { code: None, .. }
            | Self::Api { code: None, .. } => "retry",
            Self::Cache(_) | Self::Security(_) | Self::Internal(_) => "retry",
        }
    }

    /// Authorization rejection that requires Access confirmation before cleanup.
    ///
    /// Access answers that token with unauthorized or forbidden. A missing
    /// request stays a not-found for that request. Transport and server
    /// failures stay outside this set so local keys are kept when the control
    /// plane cannot be reached.
    pub fn device_authorization_lost(&self) -> bool {
        match self {
            Self::Unauthorized => true,
            Self::Host {
                code: Some(code), ..
            }
            | Self::Api {
                code: Some(code), ..
            } => matches!(code, ErrorCode::Unauthorized | ErrorCode::Forbidden),
            _ => false,
        }
    }

    pub(crate) fn from_pairing_hello_access(error: AccessError) -> Self {
        match error.code {
            Some(ErrorCode::PairingFenced)
                if error.kind == AccessErrorKind::Http && error.status == Some(409) =>
            {
                Self::PairingFenced
            }
            Some(
                ErrorCode::PairingFenced
                | ErrorCode::PairingCommittedUnavailable
                | ErrorCode::PairingCommittedUnconfirmed,
            ) => Self::Security("invalid pairing HELLO response".into()),
            _ => Self::from(error),
        }
    }

    pub(crate) fn from_pairing_access(error: AccessError) -> Self {
        if matches!(
            error.code,
            Some(
                ErrorCode::PairingFenced
                    | ErrorCode::PairingCommittedUnavailable
                    | ErrorCode::PairingCommittedUnconfirmed
            )
        ) {
            if error.kind == AccessErrorKind::Http && error.status == Some(409) {
                match error.code {
                    Some(ErrorCode::PairingFenced) => Self::PairingFenced,
                    Some(ErrorCode::PairingCommittedUnavailable) => {
                        Self::PairingCommittedUnavailable
                    }
                    Some(ErrorCode::PairingCommittedUnconfirmed) => {
                        Self::PairingCommittedUnconfirmed
                    }
                    _ => unreachable!(),
                }
            } else {
                Self::Security("invalid pairing terminal response".into())
            }
        } else {
            Self::from(error)
        }
    }

    pub(crate) fn pairing_closed(&self) -> bool {
        matches!(
            self,
            Self::PairingFenced | Self::PairingCommittedUnavailable
        )
    }

    pub(crate) fn host_channel_replaced(&self) -> bool {
        matches!(
            self,
            Self::Host {
                code: Some(ErrorCode::ChannelReplaced),
                ..
            }
        )
    }
}

impl From<AccessError> for CoreError {
    fn from(error: AccessError) -> Self {
        if let Some(code) = error.code {
            return Self::Api {
                code: Some(code),
                message: error.message,
            };
        }
        match error.kind {
            AccessErrorKind::NotSent | AccessErrorKind::Transport => {
                Self::Unavailable(error.message)
            }
            AccessErrorKind::Timeout => Self::Timeout,
            AccessErrorKind::Http => {
                if error.status == Some(404) {
                    Self::NotFound(error.message)
                } else if error.status == Some(401) || error.status == Some(403) {
                    Self::Unauthorized
                } else {
                    Self::Api {
                        code: None,
                        message: error.message,
                    }
                }
            }
            AccessErrorKind::Protocol => Self::Internal(error.message),
        }
    }
}

impl From<crate::security::SecretError> for CoreError {
    fn from(error: crate::security::SecretError) -> Self {
        Self::Security(error.to_string())
    }
}

impl From<CacheError> for CoreError {
    fn from(error: CacheError) -> Self {
        if matches!(error, CacheError::HistoryBudget) {
            return Self::Api {
                code: Some(ErrorCode::PayloadTooLarge),
                message: error.to_string(),
            };
        }
        Self::Cache(error.to_string())
    }
}

impl From<SecurityError> for CoreError {
    fn from(error: SecurityError) -> Self {
        match error {
            SecurityError::ChannelMismatch => Self::ChannelMismatch,
            other => Self::Security(other.to_string()),
        }
    }
}

impl From<PairingError> for CoreError {
    fn from(error: PairingError) -> Self {
        match error {
            PairingError::Cancelled => Self::PairingCancelled,
            PairingError::FingerprintMismatch => {
                Self::Conflict("host fingerprint does not match".into())
            }
            PairingError::ProtocolIncompatible => Self::ProtocolIncompatible,
            PairingError::ResponseMismatch | PairingError::DeviceKeyMismatch => Self::Unauthorized,
            PairingError::InvalidUri | PairingError::UnsupportedVersion => {
                Self::InvalidInput("invalid pairing URI".into())
            }
            PairingError::InvalidState | PairingError::Noise => {
                Self::Internal("pairing failed".into())
            }
        }
    }
}

pub fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unspecified => "error_code_unspecified",
        ErrorCode::InvalidArgument => "invalid_argument",
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::Forbidden => "forbidden",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Conflict => "conflict",
        ErrorCode::Expired => "expired",
        ErrorCode::UnsupportedCapability => "unsupported_capability",
        ErrorCode::ProtocolVersionIncompatible => "protocol_version_incompatible",
        ErrorCode::PayloadDigestMismatch => "payload_digest_mismatch",
        ErrorCode::ApprovalExpired => "approval_expired",
        ErrorCode::ApprovalVersionMismatch => "approval_version_mismatch",
        ErrorCode::ApprovalAlreadyResolved => "approval_already_resolved",
        ErrorCode::ApprovalUnavailable => "approval_unavailable",
        ErrorCode::SnapshotRequired => "snapshot_required",
        ErrorCode::RevocationsPending => "revocations_pending",
        ErrorCode::RateLimited => "rate_limited",
        ErrorCode::Internal => "internal",
        ErrorCode::Unavailable => "unavailable",
        ErrorCode::PayloadTooLarge => "payload_too_large",
        ErrorCode::PairingFenced => "pairing_fenced",
        ErrorCode::PairingCommittedUnavailable => "pairing_committed_unavailable",
        ErrorCode::PairingCommittedUnconfirmed => "pairing_committed_unconfirmed",
        ErrorCode::ChannelReplaced => "channel_replaced",
    }
}

/// Stable client action for an `ErrorResponse` code.
///
/// The vocabulary and the per-code value live in `proto/v1/error_codes.json`
/// and in the complete table in `proto/v1/WIRE.md`.
pub fn error_code_action(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unspecified => "none",
        ErrorCode::InvalidArgument | ErrorCode::PayloadTooLarge => "fix_input",
        ErrorCode::Unauthorized | ErrorCode::Forbidden => "reauth",
        ErrorCode::NotFound | ErrorCode::Conflict => "refresh",
        ErrorCode::Expired
        | ErrorCode::Unavailable
        | ErrorCode::Internal
        | ErrorCode::RateLimited
        | ErrorCode::RevocationsPending
        | ErrorCode::ApprovalUnavailable => "retry",
        ErrorCode::UnsupportedCapability | ErrorCode::ProtocolVersionIncompatible => "upgrade",
        ErrorCode::PayloadDigestMismatch
        | ErrorCode::ApprovalExpired
        | ErrorCode::ApprovalVersionMismatch
        | ErrorCode::ApprovalAlreadyResolved => "refresh",
        ErrorCode::SnapshotRequired => "resync",
        ErrorCode::PairingFenced | ErrorCode::PairingCommittedUnavailable => "restart_pairing",
        ErrorCode::PairingCommittedUnconfirmed => "review_pairing",
        ErrorCode::ChannelReplaced => "retry",
    }
}

/// Exit code mapping for an `ErrorResponse` code, exactly as WIRE.md requires.
pub fn error_code_exit_code(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::InvalidArgument => 2,
        ErrorCode::NotFound => 3,
        ErrorCode::Unauthorized | ErrorCode::Forbidden => 4,
        ErrorCode::Unavailable | ErrorCode::PayloadTooLarge => 5,
        ErrorCode::Conflict
        | ErrorCode::PairingFenced
        | ErrorCode::PairingCommittedUnavailable
        | ErrorCode::PairingCommittedUnconfirmed
        | ErrorCode::ChannelReplaced
        | ErrorCode::PayloadDigestMismatch
        | ErrorCode::ApprovalVersionMismatch
        | ErrorCode::ApprovalAlreadyResolved
        | ErrorCode::ApprovalExpired
        | ErrorCode::ApprovalUnavailable
        | ErrorCode::SnapshotRequired
        | ErrorCode::RevocationsPending
        | ErrorCode::RateLimited => 8,
        ErrorCode::Expired => 7,
        ErrorCode::UnsupportedCapability | ErrorCode::ProtocolVersionIncompatible => 9,
        ErrorCode::Internal | ErrorCode::Unspecified => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_wire_contract() {
        let cases = [
            (ErrorCode::InvalidArgument, 2),
            (ErrorCode::NotFound, 3),
            (ErrorCode::Unauthorized, 4),
            (ErrorCode::Forbidden, 4),
            (ErrorCode::Unavailable, 5),
            (ErrorCode::Conflict, 8),
            (ErrorCode::ChannelReplaced, 8),
            (ErrorCode::PayloadDigestMismatch, 8),
            (ErrorCode::ApprovalAlreadyResolved, 8),
            (ErrorCode::Expired, 7),
            (ErrorCode::UnsupportedCapability, 9),
            (ErrorCode::ProtocolVersionIncompatible, 9),
            (ErrorCode::Internal, 1),
        ];
        for (code, expected) in cases {
            assert_eq!(error_code_exit_code(code), expected, "{code:?}");
        }
    }

    #[test]
    fn rejected_device_authorization_is_distinct_from_transport_failure() {
        assert!(CoreError::Unauthorized.device_authorization_lost());
        assert!(CoreError::Api {
            code: Some(ErrorCode::Forbidden),
            message: "device revoked".into(),
        }
        .device_authorization_lost());
        assert!(!CoreError::Unavailable("down".into()).device_authorization_lost());
        assert!(!CoreError::NotFound("request not found".into()).device_authorization_lost());
        assert!(!CoreError::Api {
            code: Some(ErrorCode::NotFound),
            message: "request not found".into(),
        }
        .device_authorization_lost());
        assert!(!CoreError::Api {
            code: Some(ErrorCode::Unavailable),
            message: "unavailable".into(),
        }
        .device_authorization_lost());
    }

    #[test]
    fn identical_wire_errors_keep_their_source_boundary() {
        let host = CoreError::Host {
            code: Some(ErrorCode::Unauthorized),
            message: "rejected".into(),
        };
        let access = CoreError::Api {
            code: Some(ErrorCode::Unauthorized),
            message: "rejected".into(),
        };
        assert_eq!(host.code(), access.code());
        assert_eq!(host.exit_code(), access.exit_code());
        assert_eq!(host.source_layer(), "host");
        assert_eq!(access.source_layer(), "access");
        assert!(host.device_authorization_lost());
        let unknown = CoreError::Host {
            code: None,
            message: "failed".into(),
        };
        assert_eq!(unknown.code(), "internal");
        assert_eq!(unknown.exit_code(), 1);
        assert_eq!(CoreError::Cache("failed".into()).source_layer(), "storage");
        assert_eq!(
            CoreError::Security("failed".into()).source_layer(),
            "security"
        );
    }

    #[test]
    fn local_errors_map_to_stable_codes() {
        assert_eq!(CoreError::InvalidInput("x".into()).exit_code(), 2);
        assert_eq!(CoreError::NotFound("x".into()).exit_code(), 3);
        assert_eq!(CoreError::Unauthorized.exit_code(), 4);
        assert_eq!(CoreError::Revoked.exit_code(), 4);
        assert_eq!(CoreError::RevokedRetentionUnverified.exit_code(), 4);
        assert_eq!(CoreError::RevokedRetentionUnverified.code(), "revoked");
        assert_eq!(
            CoreError::RevokedRetentionUnverified.source_layer(),
            "access"
        );
        assert_eq!(CoreError::HostOffline.exit_code(), 5);
        assert_eq!(CoreError::HostRejected.exit_code(), 6);
        assert_eq!(CoreError::Timeout.exit_code(), 7);
        assert_eq!(CoreError::UnknownResult.exit_code(), 7);
        assert_eq!(CoreError::Conflict("x".into()).exit_code(), 8);
        assert_eq!(CoreError::Unsupported.exit_code(), 9);
        assert_eq!(CoreError::ProtocolIncompatible.exit_code(), 9);
        assert_eq!(CoreError::PairingCancelled.exit_code(), 2);
        assert_eq!(
            CoreError::FingerprintMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .exit_code(),
            8
        );
        assert_eq!(CoreError::Internal("x".into()).exit_code(), 1);
        assert_eq!(
            CoreError::Api {
                code: Some(ErrorCode::Unauthorized),
                message: "x".into()
            }
            .exit_code(),
            4
        );
        assert_eq!(
            CoreError::Api {
                code: None,
                message: "x".into()
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn error_codes_are_stable_names() {
        assert_eq!(CoreError::HostOffline.code(), "host_offline");
        assert_eq!(CoreError::Unauthorized.code(), "unauthorized");
        assert_eq!(
            CoreError::Api {
                code: Some(ErrorCode::PayloadDigestMismatch),
                message: "x".into()
            }
            .code(),
            "payload_digest_mismatch"
        );
        assert_eq!(CoreError::NotPaired.code(), "not_paired");
        assert_eq!(CoreError::ChannelMismatch.code(), "channel_mismatch");
    }

    #[test]
    fn access_errors_map_to_core_errors() {
        let api = AccessError {
            kind: AccessErrorKind::Http,
            status: Some(409),
            code: Some(ErrorCode::PayloadDigestMismatch),
            message: "mismatch".into(),
        };
        assert!(matches!(
            CoreError::from(api),
            CoreError::Api {
                code: Some(ErrorCode::PayloadDigestMismatch),
                ..
            }
        ));
        let not_found = AccessError {
            kind: AccessErrorKind::Http,
            status: Some(404),
            code: None,
            message: "missing".into(),
        };
        assert!(matches!(CoreError::from(not_found), CoreError::NotFound(_)));
        let transport = AccessError::transport("down");
        assert!(matches!(
            CoreError::from(transport),
            CoreError::Unavailable(_)
        ));
        let timeout = AccessError::timeout();
        assert!(matches!(CoreError::from(timeout), CoreError::Timeout));
        let protocol = AccessError::protocol("bad");
        assert!(matches!(CoreError::from(protocol), CoreError::Internal(_)));
    }
}

#[cfg(test)]
mod exhaustive_tests {
    use super::*;
    use crate::cache::CacheError;
    use crate::configuration::ConfigurationError;
    use crate::pairing::PairingError;
    use crate::security::SecurityError;

    #[test]
    fn every_error_code_has_a_stable_name_and_exit_code() {
        let cases = [
            (ErrorCode::Unspecified, "error_code_unspecified", 1),
            (ErrorCode::InvalidArgument, "invalid_argument", 2),
            (ErrorCode::Unauthorized, "unauthorized", 4),
            (ErrorCode::Forbidden, "forbidden", 4),
            (ErrorCode::NotFound, "not_found", 3),
            (ErrorCode::Conflict, "conflict", 8),
            (ErrorCode::Expired, "expired", 7),
            (
                ErrorCode::UnsupportedCapability,
                "unsupported_capability",
                9,
            ),
            (
                ErrorCode::ProtocolVersionIncompatible,
                "protocol_version_incompatible",
                9,
            ),
            (
                ErrorCode::PayloadDigestMismatch,
                "payload_digest_mismatch",
                8,
            ),
            (ErrorCode::ApprovalExpired, "approval_expired", 8),
            (
                ErrorCode::ApprovalVersionMismatch,
                "approval_version_mismatch",
                8,
            ),
            (
                ErrorCode::ApprovalAlreadyResolved,
                "approval_already_resolved",
                8,
            ),
            (ErrorCode::ApprovalUnavailable, "approval_unavailable", 8),
            (ErrorCode::SnapshotRequired, "snapshot_required", 8),
            (ErrorCode::RevocationsPending, "revocations_pending", 8),
            (ErrorCode::RateLimited, "rate_limited", 8),
            (ErrorCode::Internal, "internal", 1),
            (ErrorCode::Unavailable, "unavailable", 5),
            (ErrorCode::PayloadTooLarge, "payload_too_large", 5),
            (ErrorCode::ChannelReplaced, "channel_replaced", 8),
        ];
        for (code, name, exit) in cases {
            assert_eq!(error_code_name(code), name);
            assert_eq!(error_code_exit_code(code), exit);
            let core = CoreError::Api {
                code: Some(code),
                message: "m".into(),
            };
            assert_eq!(core.code(), name);
            assert_eq!(core.exit_code(), exit);
        }
    }

    #[test]
    fn maps_each_core_error_variant_to_stable_code_and_exit_code() {
        let errors = [
            CoreError::InvalidInput("x".into()),
            CoreError::NotFound("x".into()),
            CoreError::Unauthorized,
            CoreError::Revoked,
            CoreError::RevokedRetentionUnverified,
            CoreError::HostOffline,
            CoreError::HostRejected,
            CoreError::Timeout,
            CoreError::UnknownResult,
            CoreError::Conflict("x".into()),
            CoreError::Unsupported,
            CoreError::ProtocolIncompatible,
            CoreError::PairingCancelled,
            CoreError::FingerprintMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            CoreError::NotPaired,
            CoreError::ChannelMismatch,
            CoreError::StaleCommand {
                request_id: "r".into(),
                command_id: "c".into(),
            },
            CoreError::Api {
                code: None,
                message: "x".into(),
            },
            CoreError::Unavailable("x".into()),
            CoreError::Cache("x".into()),
            CoreError::Security("x".into()),
            CoreError::Internal("x".into()),
            CoreError::Config("x".into()),
            CoreError::from(ConfigurationError::Storage),
            CoreError::from(ConfigurationError::InvalidTls {
                message: "x".into(),
            }),
        ];
        for error in errors {
            assert!(!error.code().is_empty());
            assert!(error.exit_code() <= 9);
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn access_status_without_body_maps_to_http_codes() {
        for (status, expected_exit) in [(401u16, 4), (403, 4), (404, 3), (500, 1), (503, 1)] {
            let error = AccessError {
                kind: AccessErrorKind::Http,
                status: Some(status),
                code: None,
                message: "x".into(),
            };
            assert_eq!(CoreError::from(error).exit_code(), expected_exit);
        }
    }

    #[test]
    fn security_cache_and_pairing_errors_convert() {
        assert!(matches!(
            CoreError::from(SecurityError::ChannelMismatch),
            CoreError::ChannelMismatch
        ));
        for error in [
            SecurityError::Crypto,
            SecurityError::InvalidKey,
            SecurityError::InvalidState,
            SecurityError::HandshakeIncomplete,
            SecurityError::HandshakeComplete,
            SecurityError::AuthenticationFailed,
            SecurityError::InvalidNonce,
            SecurityError::InvalidMessage,
        ] {
            assert!(matches!(CoreError::from(error), CoreError::Security(_)));
        }
        assert!(matches!(
            CoreError::from(CacheError::Database),
            CoreError::Cache(_)
        ));
        for (error, code, exit) in [
            (PairingError::Cancelled, "cancelled", 2),
            (PairingError::FingerprintMismatch, "conflict", 8),
            (
                PairingError::ProtocolIncompatible,
                "protocol_version_incompatible",
                9,
            ),
            (PairingError::ResponseMismatch, "unauthorized", 4),
            (PairingError::DeviceKeyMismatch, "unauthorized", 4),
            (PairingError::InvalidUri, "invalid_argument", 2),
            (PairingError::UnsupportedVersion, "invalid_argument", 2),
            (PairingError::InvalidState, "internal", 1),
            (PairingError::Noise, "internal", 1),
        ] {
            let core = CoreError::from(error);
            assert_eq!(core.code(), code);
            assert_eq!(core.exit_code(), exit);
        }
    }

    #[test]
    fn golden_error_table_is_the_single_source_for_names_exits_and_actions() {
        let table: serde_json::Value =
            serde_json::from_str(include_str!("../../../proto/v1/error_codes.json"))
                .expect("golden error table");
        assert_eq!(table["schema"].as_str(), Some("whalerein/error-codes/v1"));
        let codes = table["codes"].as_array().expect("codes");
        assert_eq!(codes.len(), whalerein_proto::ALL_ERROR_CODES.len());
        let actions = [
            "none",
            "fix_input",
            "reauth",
            "refresh",
            "retry",
            "upgrade",
            "resync",
            "restart_pairing",
            "review_pairing",
        ];
        for (position, code) in whalerein_proto::ALL_ERROR_CODES.iter().enumerate() {
            let entry = &codes[position];
            let name = error_code_name(*code);
            assert_eq!(entry["name"].as_str(), Some(name));
            assert_eq!(entry["number"].as_i64(), Some(*code as i64));
            assert_eq!(
                entry["exitCode"].as_u64(),
                Some(error_code_exit_code(*code) as u64),
                "{name} exit"
            );
            let action = error_code_action(*code);
            assert_eq!(entry["action"].as_str(), Some(action), "{name} action");
            assert!(actions.contains(&action), "{name} action {action}");
            assert_eq!(
                CoreError::Api {
                    code: Some(*code),
                    message: "m".into()
                }
                .action(),
                action,
                "{name} projection"
            );
        }
    }

    #[test]
    fn every_core_error_has_a_known_client_action() {
        let actions = [
            "none",
            "fix_input",
            "reauth",
            "refresh",
            "retry",
            "upgrade",
            "resync",
            "restart_pairing",
            "review_pairing",
        ];
        let errors = [
            CoreError::InvalidInput("x".into()),
            CoreError::NotFound("x".into()),
            CoreError::Unauthorized,
            CoreError::Revoked,
            CoreError::RevokedRetentionUnverified,
            CoreError::HostOffline,
            CoreError::HostRejected,
            CoreError::Timeout,
            CoreError::UnknownResult,
            CoreError::Conflict("x".into()),
            CoreError::Unsupported,
            CoreError::ProtocolIncompatible,
            CoreError::PairingCancelled,
            CoreError::PairingFenced,
            CoreError::PairingCommittedUnavailable,
            CoreError::PairingCommittedUnconfirmed,
            CoreError::FingerprintMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            CoreError::NotPaired,
            CoreError::ChannelMismatch,
            CoreError::StaleCommand {
                request_id: "r".into(),
                command_id: "c".into(),
            },
            CoreError::Host {
                code: Some(ErrorCode::ProtocolVersionIncompatible),
                message: "x".into(),
            },
            CoreError::Source {
                code: None,
                message: "x".into(),
            },
            CoreError::Api {
                code: None,
                message: "x".into(),
            },
            CoreError::Unavailable("x".into()),
            CoreError::Cache("x".into()),
            CoreError::Security("x".into()),
            CoreError::Internal("x".into()),
            CoreError::Config("x".into()),
            CoreError::from(ConfigurationError::Storage),
            CoreError::from(ConfigurationError::InvalidTls {
                message: "x".into(),
            }),
        ];
        for error in errors {
            assert!(
                actions.contains(&error.action()),
                "{error:?} has unknown action {}",
                error.action()
            );
        }
    }

    #[test]
    fn a_version_mismatch_is_an_upgrade_contract_not_a_retryable_outage() {
        // The iOS client previously mapped this state to `unavailable` and
        // retried forever. `upgrade` is terminal for the current attempt.
        let error = CoreError::Host {
            code: Some(ErrorCode::ProtocolVersionIncompatible),
            message: "peer is newer".into(),
        };
        assert_eq!(error.code(), "protocol_version_incompatible");
        assert_eq!(error.exit_code(), 9);
        assert_eq!(error.action(), "upgrade");
        assert_ne!(error.code(), "unavailable");
        assert_eq!(
            error_code_action(ErrorCode::UnsupportedCapability),
            "upgrade"
        );
        assert_eq!(error_code_action(ErrorCode::Unavailable), "retry");
        assert_eq!(
            error_code_action(ErrorCode::PairingCommittedUnconfirmed),
            "review_pairing"
        );
        assert_eq!(CoreError::Unsupported.action(), "upgrade");
        assert_eq!(CoreError::ProtocolIncompatible.action(), "upgrade");
    }

    #[test]
    fn replacement_signal_is_accepted_only_from_host_result() {
        assert!(CoreError::Host {
            code: Some(ErrorCode::ChannelReplaced),
            message: String::new()
        }
        .host_channel_replaced());
        assert!(!CoreError::Api {
            code: Some(ErrorCode::ChannelReplaced),
            message: String::new()
        }
        .host_channel_replaced());
        assert!(!CoreError::Host {
            code: Some(ErrorCode::Unavailable),
            message: String::new()
        }
        .host_channel_replaced());
    }
}
