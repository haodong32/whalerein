//! Generated Protobuf and proto3 JSON types for the WhaleRein v1 contract.
//!
//! The crate also owns the executable compatibility policy: see [`compat`].

include!(concat!(env!("OUT_DIR"), "/whalerein.v1.rs"));
// pbjson-build 0.9 borrows field lists in its generated formatting calls.
#[allow(clippy::useless_borrows_in_formatting)]
mod serde_impls {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/whalerein.v1.serde.rs"));
}

mod compat;
pub use compat::*;

/// Protocol version carried on every request and result envelope.
pub const PROTOCOL_VERSION: u32 = MIN_SUPPORTED_PROTOCOL_VERSION;

/// Version of the pairing and channel prologue strings.
pub const PROLOGUE_VERSION: &str = "whalerein/v1";

/// Every `ErrorCode` value in wire order, including the unspecified zero value.
///
/// This list is checked against `proto/v1/error_codes.json` and against the
/// complete `ErrorCode -> exit code` table in `proto/v1/WIRE.md`.
pub const ALL_ERROR_CODES: [ErrorCode; 24] = [
    ErrorCode::Unspecified,
    ErrorCode::InvalidArgument,
    ErrorCode::Unauthorized,
    ErrorCode::Forbidden,
    ErrorCode::NotFound,
    ErrorCode::Conflict,
    ErrorCode::Expired,
    ErrorCode::UnsupportedCapability,
    ErrorCode::ProtocolVersionIncompatible,
    ErrorCode::PayloadDigestMismatch,
    ErrorCode::ApprovalExpired,
    ErrorCode::ApprovalVersionMismatch,
    ErrorCode::ApprovalAlreadyResolved,
    ErrorCode::ApprovalUnavailable,
    ErrorCode::SnapshotRequired,
    ErrorCode::RevocationsPending,
    ErrorCode::RateLimited,
    ErrorCode::Internal,
    ErrorCode::Unavailable,
    ErrorCode::PayloadTooLarge,
    ErrorCode::PairingFenced,
    ErrorCode::PairingCommittedUnavailable,
    ErrorCode::PairingCommittedUnconfirmed,
    ErrorCode::ChannelReplaced,
];

/// Stable wire name of an `ErrorCode`.
pub fn error_code_name(code: ErrorCode) -> &'static str {
    code.as_str_name()
}
