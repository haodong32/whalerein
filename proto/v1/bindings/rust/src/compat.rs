//! Cross-language compatibility rules for the v1 wire contract.
//!
//! This module is the single executable statement of the compatibility policy
//! that `proto/v1/WIRE.md` documents:
//!
//! * A reader ignores unknown proto3 JSON field names and unknown binary tags.
//!   Adding a field is additive.
//! * An unknown enum value is **not** additive: a reader that does not know a
//!   value must fail explicitly instead of falling back to the zero value.
//! * A oneof payload must carry exactly one known variant. An absent variant is
//!   an error, never an empty success.
//! * `protocol_version` is checked against a supported window, and the error
//!   names the side that must upgrade.
//! * Identifiers are bounded and cannot contain the AAD separator.
//! * Time values are Unix milliseconds; clock skew is explicit and bounded.

use crate::ErrorCode;
use crate::{
    CapabilitySet, ContentBlock, LiveFrame, RequestPayload, ResponsePayload, SessionUpdate,
};

/// Lowest application protocol version this build can talk to.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 1;
/// Highest application protocol version this build can talk to.
pub const MAX_SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// Unit of every `*_unix_ms` field: milliseconds since the Unix epoch, UTC.
pub const UNIX_TIMESTAMP_UNIT: &str = "unix-epoch-milliseconds";
/// Pairing signatures are accepted up to this far in the past.
pub const PAIRING_SIGNATURE_PAST_MS: i64 = 120_000;
/// Pairing signatures are accepted up to this far in the future.
pub const PAIRING_SIGNATURE_FUTURE_MS: i64 = 60_000;
/// Diagnostic clock-skew tolerance. It never extends a request lifetime.
pub const CLOCK_SKEW_TOLERANCE_MS: i64 = 60_000;

/// One actionable wire violation, independent of a transport layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// A oneof carried no variant this reader knows.
    MissingOneof {
        type_name: &'static str,
        member: &'static str,
    },
    /// A peer sent an enum value this reader does not know.
    UnknownEnum { type_name: &'static str, value: i32 },
    /// A peer needs a capability this side does not advertise.
    UnsupportedCapability { capability: &'static str },
    /// A protocol version is outside the supported window.
    ProtocolVersion(ProtocolCompatibility),
    /// An identifier violates the wire contract.
    Id(IdError),
    /// A timestamp is outside the accepted window.
    Timestamp(TimestampError),
}

impl WireError {
    /// Stable wire `ErrorCode` for this violation.
    ///
    /// An unknown enum or an unsupported version means the peer is newer, so
    /// both map to `protocol_version_incompatible` and exit code 9. Malformed
    /// frames and identifiers are client input errors.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::MissingOneof { .. } | Self::Id(_) => ErrorCode::InvalidArgument,
            Self::Timestamp(_) => ErrorCode::Unauthorized,
            Self::UnknownEnum { .. } | Self::ProtocolVersion(_) => {
                ErrorCode::ProtocolVersionIncompatible
            }
            Self::UnsupportedCapability { .. } => ErrorCode::UnsupportedCapability,
        }
    }

    /// Actionable, content-free text. It names the offending type or value and
    /// never includes payload bytes.
    pub fn message(&self) -> String {
        match self {
            Self::MissingOneof {
                type_name,
                member,
            } => format!(
                "{type_name} carries no known {member} variant; reject the frame and check the peer protocol version"
            ),
            Self::UnknownEnum { type_name, value } => format!(
                "unknown {type_name} value {value}; this build only knows the v{MIN_SUPPORTED_PROTOCOL_VERSION}..=v{MAX_SUPPORTED_PROTOCOL_VERSION} enum values, so upgrade the reader or lower the peer version"
            ),
            Self::UnsupportedCapability { capability } => {
                format!("peer did not advertise the required capability {capability}")
            }
            Self::ProtocolVersion(compatibility) => compatibility.message(),
            Self::Id(error) => error.message(),
            Self::Timestamp(error) => error.message(),
        }
    }

    /// Client action required to recover. Every wire violation is terminal for
    /// the current attempt, so none is retryable without a change.
    pub fn is_retryable(&self) -> bool {
        false
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for WireError {}

impl From<IdError> for WireError {
    fn from(value: IdError) -> Self {
        Self::Id(value)
    }
}

impl From<TimestampError> for WireError {
    fn from(value: TimestampError) -> Self {
        Self::Timestamp(value)
    }
}

impl From<ProtocolCompatibility> for WireError {
    fn from(value: ProtocolCompatibility) -> Self {
        Self::ProtocolVersion(value)
    }
}

/// Which side must change to reach a supported version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeSide {
    /// Both sides already agree.
    None,
    /// The remote peer must be upgraded.
    Peer,
    /// This local component must be upgraded.
    Local,
}

/// Result of comparing a peer version with the supported window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolCompatibility {
    Compatible,
    /// The peer sent zero, which no released version uses.
    MissingVersion,
    /// The peer is older than [`MIN_SUPPORTED_PROTOCOL_VERSION`].
    PeerOlder {
        peer: u32,
    },
    /// The peer is newer than [`MAX_SUPPORTED_PROTOCOL_VERSION`].
    PeerNewer {
        peer: u32,
    },
}

impl ProtocolCompatibility {
    pub fn is_compatible(self) -> bool {
        matches!(self, Self::Compatible)
    }

    pub fn upgrade_side(self) -> UpgradeSide {
        match self {
            Self::Compatible => UpgradeSide::None,
            Self::MissingVersion | Self::PeerOlder { .. } => UpgradeSide::Peer,
            Self::PeerNewer { .. } => UpgradeSide::Local,
        }
    }

    /// Actionable text naming the side that must upgrade.
    pub fn message(self) -> String {
        let window = format!("{MIN_SUPPORTED_PROTOCOL_VERSION}..={MAX_SUPPORTED_PROTOCOL_VERSION}");
        match self {
            Self::Compatible => format!("protocol version is supported ({window})"),
            Self::MissingVersion => format!(
                "peer omitted protocol_version (0); upgrade the older peer to protocol version {MIN_SUPPORTED_PROTOCOL_VERSION} (supported window {window})"
            ),
            Self::PeerOlder { peer } => format!(
                "peer protocol version {peer} is older than the supported window {window}; upgrade the remote Host, Access, or client"
            ),
            Self::PeerNewer { peer } => format!(
                "peer protocol version {peer} is newer than this build supports ({window}); upgrade this client"
            ),
        }
    }
}

/// Compare a peer version against the supported window.
pub fn check_protocol_version(peer: u32) -> ProtocolCompatibility {
    if peer == 0 {
        ProtocolCompatibility::MissingVersion
    } else if peer < MIN_SUPPORTED_PROTOCOL_VERSION {
        ProtocolCompatibility::PeerOlder { peer }
    } else if peer > MAX_SUPPORTED_PROTOCOL_VERSION {
        ProtocolCompatibility::PeerNewer { peer }
    } else {
        ProtocolCompatibility::Compatible
    }
}

/// Require the current application protocol version and explain a mismatch.
pub fn require_protocol_version(peer: u32) -> Result<(), WireError> {
    let compatibility = check_protocol_version(peer);
    if compatibility.is_compatible() {
        Ok(())
    } else {
        Err(WireError::ProtocolVersion(compatibility))
    }
}

/// Decode a known enum value without falling back to the zero value.
pub fn require_known_enum<E>(type_name: &'static str, value: i32) -> Result<E, WireError>
where
    E: TryFrom<i32>,
{
    E::try_from(value).map_err(|_| WireError::UnknownEnum { type_name, value })
}

/// Require exactly one known oneof variant.
pub fn required_oneof<T>(value: Option<T>, type_name: &'static str) -> Result<T, WireError> {
    value.ok_or(WireError::MissingOneof {
        type_name,
        member: "payload",
    })
}

pub fn require_request_payload(
    payload: RequestPayload,
) -> Result<crate::request_payload::Payload, WireError> {
    required_oneof(payload.payload, "RequestPayload")
}

pub fn require_response_payload(
    payload: ResponsePayload,
) -> Result<crate::response_payload::Payload, WireError> {
    required_oneof(payload.payload, "ResponsePayload")
}

pub fn require_session_update(
    update: SessionUpdate,
) -> Result<crate::session_update::Payload, WireError> {
    required_oneof(update.payload, "SessionUpdate")
}

pub fn require_live_frame(frame: LiveFrame) -> Result<crate::live_frame::Payload, WireError> {
    required_oneof(frame.payload, "LiveFrame")
}

pub fn require_content_block(
    block: ContentBlock,
) -> Result<crate::content_block::Content, WireError> {
    required_oneof(block.content, "ContentBlock")
}

/// Whether a capability set explicitly advertises one capability.
pub fn has_capability(set: &CapabilitySet, capability: crate::Capability) -> bool {
    set.capabilities.contains(&(capability as i32))
}

/// Require an advertised capability before a call is attempted.
pub fn require_capability(
    set: &CapabilitySet,
    capability: crate::Capability,
    name: &'static str,
) -> Result<(), WireError> {
    if has_capability(set, capability) {
        Ok(())
    } else {
        Err(WireError::UnsupportedCapability { capability: name })
    }
}

/// Ordering of a peer timestamp against the local clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampOrder {
    /// In the past, up to and including the local clock.
    Past { by_ms: i64 },
    /// In the future, beyond the local clock.
    Future { by_ms: i64 },
    /// Exactly the local clock.
    Now,
}

/// Classify a Unix-millisecond timestamp against the local clock.
///
/// The result is diagnostic: request expiry never gains grace from it.
pub fn classify_timestamp(at_unix_ms: i64, now_unix_ms: i64) -> TimestampOrder {
    match at_unix_ms.cmp(&now_unix_ms) {
        std::cmp::Ordering::Less => TimestampOrder::Past {
            by_ms: now_unix_ms - at_unix_ms,
        },
        std::cmp::Ordering::Greater => TimestampOrder::Future {
            by_ms: at_unix_ms - now_unix_ms,
        },
        std::cmp::Ordering::Equal => TimestampOrder::Now,
    }
}

/// A request is expired at or before the local clock. No clock-skew grace is
/// applied, so a delayed frame can never extend a request lifetime.
pub fn is_expired(at_unix_ms: i64, now_unix_ms: i64) -> bool {
    at_unix_ms <= now_unix_ms
}

/// Why a timestamp fell outside the accepted window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimestampError {
    /// Signed too far in the past.
    TooOld {
        signed_at_unix_ms: i64,
        now_unix_ms: i64,
    },
    /// Signed too far in the future.
    TooFarAhead {
        signed_at_unix_ms: i64,
        now_unix_ms: i64,
    },
}

impl TimestampError {
    pub fn message(&self) -> String {
        match self {
            Self::TooOld {
                signed_at_unix_ms,
                now_unix_ms,
            } => format!(
                "signed_at_unix_ms {signed_at_unix_ms} is more than {} ms before the local clock {now_unix_ms}; check the signer clock",
                PAIRING_SIGNATURE_PAST_MS
            ),
            Self::TooFarAhead {
                signed_at_unix_ms,
                now_unix_ms,
            } => format!(
                "signed_at_unix_ms {signed_at_unix_ms} is more than {} ms ahead of the local clock {now_unix_ms}; check the signer clock",
                PAIRING_SIGNATURE_FUTURE_MS
            ),
        }
    }
}

/// Validate the bounded pairing signature clock window: `now - 120s` through
/// `now + 60s`, inclusive.
pub fn validate_pairing_signature_time(
    signed_at_unix_ms: i64,
    now_unix_ms: i64,
) -> Result<(), TimestampError> {
    if now_unix_ms - signed_at_unix_ms > PAIRING_SIGNATURE_PAST_MS {
        return Err(TimestampError::TooOld {
            signed_at_unix_ms,
            now_unix_ms,
        });
    }
    if signed_at_unix_ms - now_unix_ms > PAIRING_SIGNATURE_FUTURE_MS {
        return Err(TimestampError::TooFarAhead {
            signed_at_unix_ms,
            now_unix_ms,
        });
    }
    Ok(())
}

/// Diagnostic label for a peer clock that is outside [`CLOCK_SKEW_TOLERANCE_MS`].
pub fn clock_skew_label(at_unix_ms: i64, now_unix_ms: i64) -> Option<&'static str> {
    match classify_timestamp(at_unix_ms, now_unix_ms) {
        TimestampOrder::Past { by_ms } | TimestampOrder::Future { by_ms }
            if by_ms > CLOCK_SKEW_TOLERANCE_MS =>
        {
            Some("clock_skew")
        }
        _ => None,
    }
}

/// Identifier categories with independent bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    Request,
    Command,
    Channel,
    Trace,
    Claim,
    Session,
    Host,
    Device,
    Watch,
    Wait,
    Approval,
    Delivery,
    Message,
    Block,
    Batch,
    Item,
    Cursor,
}

impl IdKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Request => "request_id",
            Self::Command => "command_id",
            Self::Channel => "channel_id",
            Self::Trace => "trace_id",
            Self::Claim => "claim_id",
            Self::Session => "session_id",
            Self::Host => "host_id",
            Self::Device => "device_id",
            Self::Watch => "watch_id",
            Self::Wait => "wait_id",
            Self::Approval => "approval_id",
            Self::Delivery => "delivery_id",
            Self::Message => "message_id",
            Self::Block => "block_id",
            Self::Batch => "batch_id",
            Self::Item => "item_id",
            Self::Cursor => "cursor",
        }
    }

    /// Maximum encoded length in bytes. The values bound logs, database rows
    /// and AAD inputs without constraining native session identifiers.
    pub fn max_bytes(self) -> usize {
        match self {
            Self::Session => 512,
            Self::Cursor => 4096,
            Self::Host | Self::Device => 200,
            Self::Trace | Self::Watch | Self::Wait => 64,
            Self::Request
            | Self::Command
            | Self::Channel
            | Self::Claim
            | Self::Approval
            | Self::Delivery
            | Self::Message
            | Self::Block
            | Self::Batch
            | Self::Item => 128,
        }
    }

    /// Empty is valid only where the contract defines an absent value.
    pub fn allow_empty(self) -> bool {
        matches!(self, Self::Command | Self::Cursor)
    }

    /// Whether this identifier is concatenated into an AAD string.
    pub fn aad_bound(self) -> bool {
        matches!(
            self,
            Self::Request | Self::Command | Self::Channel | Self::Host | Self::Device
        )
    }

    /// Opaque routing identifiers are printable ASCII. Native session ids and
    /// opaque cursors may be UTF-8 but still cannot contain control bytes.
    pub fn ascii_only(self) -> bool {
        !matches!(self, Self::Session | Self::Cursor)
    }
}

/// Why an identifier was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdProblem {
    Empty,
    TooLong,
    ControlCharacter,
    AadSeparator,
    NonAscii,
}

/// A rejected identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdError {
    pub kind: IdKind,
    pub problem: IdProblem,
}

impl IdError {
    pub fn message(self) -> String {
        let problem = match self.problem {
            IdProblem::Empty => "must not be empty",
            IdProblem::TooLong => "is longer than the wire limit",
            IdProblem::ControlCharacter => "must not contain control characters",
            IdProblem::AadSeparator => "must not contain '|' because it is an AAD separator",
            IdProblem::NonAscii => "must be printable ASCII",
        };
        format!("{} {problem}", self.kind.name())
    }
}

/// Validate one identifier against the shared wire contract.
pub fn validate_id(kind: IdKind, value: &str) -> Result<(), IdError> {
    if value.is_empty() {
        return if kind.allow_empty() {
            Ok(())
        } else {
            Err(IdError {
                kind,
                problem: IdProblem::Empty,
            })
        };
    }
    if value.len() > kind.max_bytes() {
        return Err(IdError {
            kind,
            problem: IdProblem::TooLong,
        });
    }
    for character in value.chars() {
        if character == '|' {
            return Err(IdError {
                kind,
                problem: IdProblem::AadSeparator,
            });
        }
        if character.is_control() {
            return Err(IdError {
                kind,
                problem: IdProblem::ControlCharacter,
            });
        }
        if kind.ascii_only() && (!character.is_ascii_graphic()) {
            return Err(IdError {
                kind,
                problem: IdProblem::NonAscii,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{request_payload, response_payload};

    #[test]
    fn version_window_classifies_each_side_and_names_the_upgrade() {
        assert!(check_protocol_version(1).is_compatible());
        assert_eq!(check_protocol_version(1).upgrade_side(), UpgradeSide::None);

        let missing = check_protocol_version(0);
        assert_eq!(missing.upgrade_side(), UpgradeSide::Peer);
        assert!(!missing.is_compatible());
        assert!(missing.message().contains("upgrade the older peer"));

        let older = ProtocolCompatibility::PeerOlder { peer: 0 };
        assert_eq!(older.upgrade_side(), UpgradeSide::Peer);
        assert!(older.message().contains("is older"));

        let newer = check_protocol_version(2);
        assert!(!newer.is_compatible());
        assert_eq!(newer.upgrade_side(), UpgradeSide::Local);
        assert!(newer.message().contains("upgrade this client"));
        assert!(newer.message().contains("1..=1"));

        assert!(require_protocol_version(1).is_ok());
        let error = require_protocol_version(9).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProtocolVersionIncompatible);
        assert!(error.message().contains("9"));
        assert!(!error.is_retryable());
    }

    #[test]
    fn unknown_enum_is_explicit_and_never_the_zero_value() {
        let error = require_known_enum::<crate::OnlineStatus>("OnlineStatus", 999).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProtocolVersionIncompatible);
        assert!(error.message().contains("OnlineStatus"));
        assert!(error.message().contains("999"));
        assert!(require_known_enum::<crate::OnlineStatus>("OnlineStatus", 0).is_ok());
    }

    #[test]
    fn missing_oneof_is_an_error_not_an_empty_success() {
        let request = RequestPayload { payload: None };
        let error = require_request_payload(request).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("RequestPayload"));
        assert!(error.message().contains("protocol version"));

        let response = ResponsePayload {
            payload: None,
            error_code: 0,
            error_detail: String::new(),
        };
        assert!(require_response_payload(response).is_err());

        let known = require_request_payload(RequestPayload {
            payload: Some(request_payload::Payload::ListSessions(
                crate::ListSessions {},
            )),
        })
        .unwrap();
        assert!(matches!(known, request_payload::Payload::ListSessions(_)));
        assert!(matches!(
            require_response_payload(ResponsePayload {
                payload: Some(response_payload::Payload::Pong(crate::Pong {
                    host_time_unix_ms: 1
                })),
                error_code: 0,
                error_detail: String::new(),
            })
            .unwrap(),
            response_payload::Payload::Pong(_)
        ));
        for update in [
            SessionUpdate {
                watch_id: "w".into(),
                session: None,
                payload: None,
            },
            SessionUpdate {
                watch_id: "w".into(),
                session: None,
                payload: Some(crate::session_update::Payload::History(
                    crate::HistoryBatch::default(),
                )),
            },
        ] {
            if let Err(error) = require_session_update(update) {
                assert_eq!(error.code(), ErrorCode::InvalidArgument);
            }
        }
        assert!(require_live_frame(LiveFrame {
            payload: None,
            ..Default::default()
        })
        .is_err());
        assert!(require_content_block(ContentBlock {
            block_id: "b".into(),
            content: None
        })
        .is_err());
        assert_eq!(
            required_oneof::<u8>(None, "X").unwrap_err().code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(required_oneof(Some(7u8), "X").unwrap(), 7);
    }

    #[test]
    fn capability_set_read_path_is_explicit() {
        let set = CapabilitySet {
            capabilities: vec![
                crate::Capability::Sessions as i32,
                crate::Capability::Events as i32,
            ],
            protocol_version: 1,
        };
        assert!(has_capability(&set, crate::Capability::Sessions));
        assert!(!has_capability(&set, crate::Capability::Approvals));
        assert!(require_capability(&set, crate::Capability::Sessions, "sessions").is_ok());
        let error =
            require_capability(&set, crate::Capability::Approvals, "approvals").unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnsupportedCapability);
        assert!(error.message().contains("approvals"));
    }

    #[test]
    fn time_units_are_milliseconds_and_expiry_has_no_grace() {
        assert_eq!(UNIX_TIMESTAMP_UNIT, "unix-epoch-milliseconds");
        assert!(!is_expired(1_001, 1_000));
        assert!(is_expired(1_000, 1_000));
        assert!(is_expired(999, 1_000));
        assert_eq!(
            classify_timestamp(900, 1_000),
            TimestampOrder::Past { by_ms: 100 }
        );
        assert_eq!(
            classify_timestamp(1_100, 1_000),
            TimestampOrder::Future { by_ms: 100 }
        );
        assert_eq!(classify_timestamp(1_000, 1_000), TimestampOrder::Now);
        assert_eq!(clock_skew_label(1_000, 1_000), None);
        assert_eq!(clock_skew_label(1_000 + 60_001, 1_000), Some("clock_skew"));
        assert_eq!(clock_skew_label(1_000, 1_000 + 60_001), Some("clock_skew"));
    }

    #[test]
    fn pairing_signature_window_is_bounded_on_both_sides() {
        let now = 1_000_000;
        assert!(validate_pairing_signature_time(now, now).is_ok());
        assert!(validate_pairing_signature_time(now - PAIRING_SIGNATURE_PAST_MS, now).is_ok());
        assert!(validate_pairing_signature_time(now + PAIRING_SIGNATURE_FUTURE_MS, now).is_ok());
        let old =
            validate_pairing_signature_time(now - PAIRING_SIGNATURE_PAST_MS - 1, now).unwrap_err();
        assert!(old.message().contains("120000"));
        let ahead = validate_pairing_signature_time(now + PAIRING_SIGNATURE_FUTURE_MS + 1, now)
            .unwrap_err();
        assert!(ahead.message().contains("60000"));
        assert_eq!(
            WireError::from(old).code(),
            ErrorCode::Unauthorized,
            "a signature outside the clock window is rejected as unauthorized"
        );
    }

    #[test]
    fn identifiers_are_bounded_and_cannot_break_the_aad() {
        for kind in [
            IdKind::Request,
            IdKind::Command,
            IdKind::Channel,
            IdKind::Trace,
            IdKind::Claim,
            IdKind::Session,
            IdKind::Host,
            IdKind::Device,
            IdKind::Watch,
            IdKind::Wait,
            IdKind::Approval,
            IdKind::Delivery,
            IdKind::Message,
            IdKind::Block,
            IdKind::Batch,
            IdKind::Item,
            IdKind::Cursor,
        ] {
            assert!(!kind.name().is_empty());
            assert!(kind.max_bytes() > 0);
            assert_eq!(validate_id(kind, "").is_ok(), kind.allow_empty());
            assert_eq!(
                validate_id(kind, &"a".repeat(kind.max_bytes() + 1)),
                Err(IdError {
                    kind,
                    problem: IdProblem::TooLong
                })
            );
            assert_eq!(
                validate_id(kind, "a|b"),
                Err(IdError {
                    kind,
                    problem: IdProblem::AadSeparator
                })
            );
            assert_eq!(
                validate_id(kind, "a\nb"),
                Err(IdError {
                    kind,
                    problem: IdProblem::ControlCharacter
                })
            );
        }
        assert_eq!(validate_id(IdKind::Request, "abc-DEF_123"), Ok(()));
        assert_eq!(
            validate_id(IdKind::Request, "héllo"),
            Err(IdError {
                kind: IdKind::Request,
                problem: IdProblem::NonAscii
            })
        );
        assert_eq!(validate_id(IdKind::Session, "sess/ü"), Ok(()));
        assert!(IdKind::Request.aad_bound());
        assert!(!IdKind::Session.aad_bound());
        assert!(IdKind::Session.max_bytes() > IdKind::Request.max_bytes());
        assert!(IdError {
            kind: IdKind::Request,
            problem: IdProblem::AadSeparator
        }
        .message()
        .contains("request_id"));
    }
}
