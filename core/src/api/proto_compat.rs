//! Forward-compatible Core wire decoding and actionable protocol errors.
//!
//! The generated proto3 JSON decoders ignore unknown fields (see
//! `proto/v1/bindings/rust/build.rs`), so a peer that adds a field stays
//! readable. A value this build does not know is different: an unknown enum or
//! a missing oneof must be reported as a version problem with an upgrade
//! instruction, never as an `internal` failure.

use serde::de::DeserializeOwned;

use whalerein_proto::{check_protocol_version, WireError};

use crate::access::{AccessError, AccessErrorKind};
use crate::api::{CoreError, CoreResult};

/// Maximum diagnostic length kept from a decode failure.
const DECODE_MESSAGE_LIMIT: usize = 200;

/// Decode one proto3 JSON body with the additive-field policy.
///
/// Unknown field names are ignored by the generated decoder. A decode failure
/// is classified: an unknown enum value means the peer is newer and yields
/// `protocol_version_incompatible`; other malformed bodies stay a protocol
/// error with an actionable message.
pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AccessError> {
    serde_json::from_slice(bytes).map_err(|error| decode_failure(&error))
}

/// Classify a proto3 JSON decode failure without echoing payload bytes.
pub fn decode_failure(error: &serde_json::Error) -> AccessError {
    let text = error.to_string().replace(['\n', '\r'], " ");
    let mut text = text;
    crate::util::truncate_utf8(&mut text, DECODE_MESSAGE_LIMIT);
    let (code, message) = if looks_like_unknown_value(&text) {
        (
            Some(whalerein_proto::ErrorCode::ProtocolVersionIncompatible),
            format!(
                "peer sent a value this client does not understand ({text}); upgrade this client or the peer to a common protocol version"
            ),
        )
    } else {
        (
            None,
            format!(
                "peer response does not match the v1 wire contract ({text}); if the peer was upgraded, upgrade this client too"
            ),
        )
    };
    AccessError {
        kind: AccessErrorKind::Protocol,
        status: None,
        code,
        message,
    }
}

fn looks_like_unknown_value(text: &str) -> bool {
    text.contains("unknown variant")
        || text.contains("invalid value")
        || text.contains("unknown enum")
}

/// Actionable error for a peer application protocol version.
pub fn protocol_incompatible(peer: u32) -> CoreError {
    let compatibility = check_protocol_version(peer);
    CoreError::Api {
        code: Some(whalerein_proto::ErrorCode::ProtocolVersionIncompatible),
        message: compatibility.message(),
    }
}

/// Require the current protocol version or explain which side must upgrade.
pub fn require_protocol_version(peer: u32) -> CoreResult<()> {
    if check_protocol_version(peer).is_compatible() {
        Ok(())
    } else {
        Err(protocol_incompatible(peer))
    }
}

/// Convert a shared wire violation into the Core error projection.
pub fn wire_error(error: WireError) -> CoreError {
    CoreError::Api {
        code: Some(error.code()),
        message: error.message(),
    }
}

/// Require a known oneof variant and map a missing variant to `invalid_argument`.
pub fn require_payload<T>(payload: Option<T>, type_name: &'static str) -> CoreResult<T> {
    whalerein_proto::required_oneof(payload, type_name).map_err(wire_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_proto::{
        ErrorCode, HostResult, OnlineStatus, ProtocolCompatibility, PROTOCOL_VERSION,
    };

    #[test]
    fn old_reader_ignores_a_new_field_in_both_directions() {
        // A newer Access sends an additional field; the old Core reader keeps
        // decoding the fields it knows instead of failing the whole request.
        let json = br#"{
            "hosts": [{"hostId": "h", "online": true, "protocolVersion": 1, "fingerprint": "f", "futureZone": "x"}],
            "futureTotal": 3
        }"#;
        let list: whalerein_proto::HostList = decode_json(json).expect("additive field");
        assert_eq!(list.hosts.len(), 1);
        assert_eq!(list.hosts[0].host_id, "h");

        // A newer reader accepts an older payload that lacks newer fields.
        let older: HostResult = decode_json(br#"{"requestId":"r","status":"succeeded"}"#)
            .expect("absent fields mean defaults");
        assert_eq!(older.status, OnlineStatus::Succeeded as i32);
        assert_eq!(older.protocol_version, 0);
    }

    #[test]
    fn an_unknown_enum_is_a_version_error_with_an_upgrade_instruction() {
        let error = decode_json::<HostResult>(br#"{"requestId":"r","status":"a_newer_status"}"#)
            .expect_err("unknown enum name");
        assert_eq!(error.code, Some(ErrorCode::ProtocolVersionIncompatible));
        assert!(
            error.message.contains("upgrade this client"),
            "{}",
            error.message
        );
        let core = CoreError::from(error);
        assert_eq!(core.code(), "protocol_version_incompatible");
        assert_eq!(core.exit_code(), 9);
        assert_eq!(core.action(), "upgrade");
    }

    #[test]
    fn an_unknown_enum_number_is_also_a_version_error() {
        let error = decode_json::<HostResult>(br#"{"requestId":"r","status":99}"#)
            .expect_err("unknown enum number");
        assert_eq!(error.code, Some(ErrorCode::ProtocolVersionIncompatible));
    }

    #[test]
    fn a_malformed_body_is_reported_with_actionable_text_and_no_payload() {
        let error = decode_json::<HostResult>(br#"{"requestId": 5}"#).expect_err("type mismatch");
        assert_eq!(error.kind, AccessErrorKind::Protocol);
        assert_eq!(error.code, None);
        assert!(
            error.message.contains("upgrade this client"),
            "{}",
            error.message
        );
        assert!(!error.message.contains("req-secret"));
        let core = CoreError::from(error);
        assert_eq!(core.exit_code(), 1);
        assert_eq!(core.action(), "retry");
    }

    #[test]
    fn missing_oneof_is_never_an_empty_success() {
        let error = require_payload::<u8>(None, "RequestPayload").expect_err("missing oneof");
        assert_eq!(error.code(), "invalid_argument");
        assert_eq!(error.exit_code(), 2);
        assert_eq!(require_payload(Some(7u8), "RequestPayload").unwrap(), 7);
        let shared = crate::api::proto_compat::wire_error(
            whalerein_proto::WireError::UnsupportedCapability {
                capability: "approvals",
            },
        );
        assert_eq!(shared.code(), "unsupported_capability");
        assert_eq!(shared.exit_code(), 9);
    }

    #[test]
    fn version_window_names_the_side_that_must_upgrade() {
        assert!(require_protocol_version(PROTOCOL_VERSION).is_ok());
        assert_eq!(
            ProtocolCompatibility::Compatible.upgrade_side(),
            whalerein_proto::UpgradeSide::None
        );

        let newer = protocol_incompatible(2);
        assert_eq!(newer.code(), "protocol_version_incompatible");
        assert_eq!(newer.exit_code(), 9);
        assert_eq!(newer.action(), "upgrade");
        assert!(newer.to_string().contains("upgrade this client"));

        let older = protocol_incompatible(0);
        assert!(older.to_string().contains("upgrade the older peer"));
        assert!(!newer.to_string().contains("upgrade the older peer"));
    }

    #[test]
    fn int64_json_strings_decode_exactly_near_two_to_the_53() {
        let result: HostResult = decode_json(
            br#"{"requestId":"r","completedAtUnixMs":"9007199254740993","protocolVersion":1}"#,
        )
        .expect("decode");
        assert_eq!(result.completed_at_unix_ms, 9_007_199_254_740_993);
    }
}
