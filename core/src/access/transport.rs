//! Complete-message relay transport.
//!
//! One coordinated relay epoch (`relay_version = 4`) carries a complete
//! application message as exactly one MQTT PUBLISH. Each hop selects the
//! representation for its own outgoing packet:
//!
//! - **inline** — the outer envelope carries the complete opaque body;
//! - **reference** — the outer envelope carries an opaque [`PayloadReference`]
//!   to the exact ciphertext on its owning Access node.
//!
//! A reference confers no authority. The reader authenticates with the Device
//! bearer, fetches the complete ciphertext over the HTTPS directory, verifies
//! its exact length and SHA-256, and only then applies the same AEAD and
//! business validation as for an inline packet. A partial or failed fetch is
//! never a completed message.
//!
//! There is no fragment, reassembly or receipt path: an envelope that does not
//! fit is either represented by a reference or fails explicitly before
//! publication.

use super::{AccessClient, AccessError};
use prost::Message;
use serde::{Deserialize, Serialize};
use whalerein_proto::{
    ChannelHello, ChannelReady, ErrorCode, EventEnvelope, HostRequest, HostResult,
    PayloadReference, ReceiveAdvertisement, RequestKind, TransportBudget as WireTransportBudget,
};

/// The only accepted MQTT relay epoch.
pub const RELAY_EPOCH_V4: u32 = 4;

/// Default logical envelope retained for callers that only need the baseline.
pub const MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Stable packaged profile selector. The executable values come from the
/// generated `proto/v1/bindings/rust/src/transport_profiles.rs` artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransportProfile {
    #[default]
    Default,
    Compact,
    Roomy,
}

impl TransportProfile {
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Compact => "compact",
            Self::Roomy => "roomy",
        }
    }

    pub fn wire(self) -> whalerein_proto::TransportProfile {
        match self {
            Self::Default => whalerein_proto::TransportProfile::Default,
            Self::Compact => whalerein_proto::TransportProfile::Compact,
            Self::Roomy => whalerein_proto::TransportProfile::Roomy,
        }
    }
}

/// Optional local overrides applied to one packaged profile. Every relation is
/// validated again, so an override cannot bypass the encoded limits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_overhead_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bulk_concurrency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_timeout_ms: Option<u32>,
}

/// Additive `[transport]` section of the client TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportSettings {
    #[serde(default)]
    pub profile: TransportProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<TransportOverrides>,
}

/// One validated effective transport budget. Byte fields are encoded bytes;
/// no field is a canonical content length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TransportBudget {
    pub profile: TransportProfile,
    pub packet_bytes: u32,
    #[serde(skip)]
    pub packet_overhead_bytes: u32,
    pub envelope_bytes: u64,
    pub bulk_concurrency: u32,
    pub reserved_bytes: u64,
    pub transfer_timeout_ms: u32,
}

impl Default for TransportBudget {
    fn default() -> Self {
        Self::from_packaged(
            TransportProfile::Default,
            *whalerein_proto::default_transport_profile_values(),
        )
        .expect("packaged default transport profile")
    }
}

impl TransportBudget {
    fn from_packaged(
        profile: TransportProfile,
        values: whalerein_proto::TransportProfileValues,
    ) -> Result<Self, String> {
        if let Err(error) = whalerein_proto::validate_transport_profile(&values) {
            return Err(error.to_string());
        }
        let budget = Self {
            profile,
            packet_bytes: values.packet_bytes,
            packet_overhead_bytes: values.packet_overhead_bytes,
            envelope_bytes: values.envelope_bytes,
            bulk_concurrency: values.bulk_concurrency,
            reserved_bytes: values.reserved_bytes,
            transfer_timeout_ms: values.transfer_timeout_ms,
        };
        budget.validate_control_envelopes()?;
        Ok(budget)
    }

    /// Resolve a selected packaged profile and its local overrides, then run
    /// every shared relation plus the control-envelope fit check.
    pub fn resolve(settings: &TransportSettings) -> Result<Self, String> {
        let name = settings.profile.name();
        let values = whalerein_proto::transport_profile_values(name)
            .ok_or_else(|| format!("unknown transport profile {name}"))?;
        let mut budget = Self::from_packaged(settings.profile, *values)?;
        if let Some(overrides) = &settings.overrides {
            if let Some(value) = overrides.packet_bytes {
                budget.packet_bytes = value;
            }
            if let Some(value) = overrides.packet_overhead_bytes {
                budget.packet_overhead_bytes = value;
            }
            if let Some(value) = overrides.envelope_bytes {
                budget.envelope_bytes = value;
            }
            if let Some(value) = overrides.bulk_concurrency {
                budget.bulk_concurrency = value;
            }
            if let Some(value) = overrides.reserved_bytes {
                budget.reserved_bytes = value;
            }
            if let Some(value) = overrides.transfer_timeout_ms {
                budget.transfer_timeout_ms = value;
            }
        }
        budget.validate()?;
        Ok(budget)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_relations()?;
        self.validate_control_envelopes()
    }

    /// Validate only the relations an advertisement must satisfy. A peer may
    /// advertise any relation-valid override of its named profile; the
    /// minimum against local limits is applied by the sender instead of
    /// clamping the advertisement. Control-envelope fit stays a local
    /// readiness check because the receiver already carries the handshake.
    fn validate_relations(&self) -> Result<(), String> {
        if self.packet_bytes == 0
            || self.envelope_bytes == 0
            || self.bulk_concurrency == 0
            || self.reserved_bytes == 0
            || self.transfer_timeout_ms == 0
        {
            return Err("transport limits must be positive".into());
        }
        if u64::from(self.packet_overhead_bytes) >= u64::from(self.packet_bytes) {
            return Err("packet overhead must be below the packet budget".into());
        }
        if u64::from(self.packet_bytes) > whalerein_proto::BROKER_PACKET_LIMIT_BYTES {
            return Err("packet budget exceeds the broker limit".into());
        }
        if self.envelope_bytes < u64::from(self.packet_bytes) {
            return Err("envelope bytes do not cover one packet".into());
        }
        if self.reserved_bytes < self.envelope_bytes {
            return Err("reserved bytes do not cover one envelope".into());
        }
        Ok(())
    }

    fn validate_control_envelopes(&self) -> Result<(), String> {
        // A profile that cannot carry the control handshake cannot establish a
        // channel, so reject it before any connection use.
        let hello = ChannelHello {
            device_id: "device".into(),
            host_id: "host".into(),
            protocol_version: whalerein_proto::PROTOCOL_VERSION,
            capabilities: Some(crate::pairing::default_capabilities()),
            client_nonce: "nonce".into(),
            receive: Some(self.advertisement(1)?),
        };
        let ready = ChannelReady {
            host_id: "host".into(),
            device_id: "device".into(),
            protocol_version: whalerein_proto::PROTOCOL_VERSION,
            capabilities: Some(crate::pairing::default_capabilities()),
            host_nonce: "nonce".into(),
            receive: Some(self.advertisement(1)?),
        };
        for (name, encoded) in [
            ("channel hello", hello.encoded_len()),
            ("channel ready", ready.encoded_len()),
        ] {
            if encoded as u64 > self.envelope_bytes {
                return Err(format!(
                    "envelope budget cannot carry the minimum {name} control message"
                ));
            }
        }
        Ok(())
    }

    pub fn wire_budget(&self) -> WireTransportBudget {
        WireTransportBudget {
            packet_bytes: self.packet_bytes,
            envelope_bytes: self.envelope_bytes,
            bulk_concurrency: self.bulk_concurrency,
            reserved_bytes: self.reserved_bytes,
            transfer_timeout_ms: self.transfer_timeout_ms,
        }
    }

    pub fn advertisement(&self, generation: u64) -> Result<ReceiveAdvertisement, String> {
        if generation == 0 {
            return Err("receive advertisement generation must be positive".into());
        }
        Ok(ReceiveAdvertisement {
            profile: self.profile.wire() as i32,
            budget: Some(self.wire_budget()),
            connection_generation: generation,
            relay_version: RELAY_EPOCH_V4,
        })
    }

    /// Largest complete request envelope this device sends inline. The local
    /// packet limit is reduced by the planning reserve for the actual topic,
    /// MQTT 5 properties, packet identifier and fixed header that Access uses
    /// when it forwards the request to the Host. The exact encoded packet
    /// remains authoritative on the sending hop; Core never over-estimates it.
    pub fn inline_request_bytes(&self) -> u64 {
        // The empty-payload PUBLISH framing is the fixed header plus the
        // zero-length topic/properties prefixes; the planning reserve covers
        // the actual topic, packet identifier and property bytes.
        let framing = encoded_publish_bytes("", 0, 0).saturating_sub(1);
        u64::from(self.packet_bytes)
            .saturating_sub(u64::from(self.packet_overhead_bytes))
            .saturating_sub(framing)
    }

    /// Validate a peer advertisement against the packaged profile vocabulary
    /// and its internal relations. A peer cannot invent a profile, a missing
    /// budget, a non-positive value, a contradictory relation or another relay
    /// epoch.
    pub fn from_advertisement(advertisement: &ReceiveAdvertisement) -> Result<(Self, u64), String> {
        if advertisement.connection_generation == 0 {
            return Err("receive advertisement generation missing".into());
        }
        if advertisement.relay_version != RELAY_EPOCH_V4 {
            return Err("receive advertisement relay epoch is not supported".into());
        }
        let profile = match whalerein_proto::TransportProfile::try_from(advertisement.profile) {
            Ok(whalerein_proto::TransportProfile::Default) => TransportProfile::Default,
            Ok(whalerein_proto::TransportProfile::Compact) => TransportProfile::Compact,
            Ok(whalerein_proto::TransportProfile::Roomy) => TransportProfile::Roomy,
            _ => return Err("unknown transport profile".into()),
        };
        let base = whalerein_proto::transport_profile_values(profile.name())
            .ok_or_else(|| "unknown transport profile".to_string())?;
        let budget = advertisement
            .budget
            .as_ref()
            .ok_or_else(|| "receive budget missing".to_string())?;
        // Take the advertised receive values as-is. The base profile only
        // supplies packet overhead, which is a local framing constant and is
        // not exchanged. Any relation-valid override is accepted unclamped.
        let effective = Self {
            profile,
            packet_bytes: budget.packet_bytes,
            packet_overhead_bytes: base.packet_overhead_bytes,
            envelope_bytes: budget.envelope_bytes,
            bulk_concurrency: budget.bulk_concurrency,
            reserved_bytes: budget.reserved_bytes,
            transfer_timeout_ms: budget.transfer_timeout_ms,
        };
        effective.validate_relations()?;
        Ok((effective, advertisement.connection_generation))
    }
}

/// Resolve a serialized `[transport]` section. Invalid relations fail before
/// any connection is used.
pub fn resolve_transport_settings(settings: &TransportSettings) -> Result<TransportBudget, String> {
    TransportBudget::resolve(settings)
}

/// A peer that does not advertise epoch 4 is rejected explicitly. There is no
/// unframed v2 acceptance, no fragment fallback and no mixed-epoch route.
pub fn protocol_incompatible(detail: &'static str) -> AccessError {
    AccessError {
        kind: super::AccessErrorKind::Protocol,
        status: None,
        code: Some(ErrorCode::ProtocolVersionIncompatible),
        message: detail.into(),
        reason: String::new(),
    }
}

fn invalid(detail: &'static str) -> AccessError {
    AccessError {
        kind: super::AccessErrorKind::Protocol,
        status: None,
        code: Some(ErrorCode::InvalidArgument),
        message: detail.into(),
        reason: String::new(),
    }
}

fn payload_too_large(detail: &'static str) -> AccessError {
    AccessError {
        kind: super::AccessErrorKind::Protocol,
        status: None,
        code: Some(ErrorCode::PayloadTooLarge),
        message: detail.into(),
        reason: String::new(),
    }
}

/// Reject a relay envelope whose epoch is not the coordinated epoch 4.
pub fn require_relay_epoch(version: u32) -> Result<(), AccessError> {
    if version == RELAY_EPOCH_V4 {
        Ok(())
    } else {
        Err(protocol_incompatible(
            "unsupported MQTT relay epoch; upgrade the peer to relay epoch 4",
        ))
    }
}

/// Validate the per-hop representation rule for a Device-submitted request.
///
/// `QUERY` and `COMMAND` carry exactly one of `body` and `body_reference`; both
/// or neither is `invalid_argument`. Bootstrap kinds (`HANDSHAKE`,
/// `PAIRING_CLAIM` and internal Access notices) are small inline bodies and
/// never carry a reference.
pub fn validate_request_representation(request: &HostRequest) -> Result<(), AccessError> {
    require_relay_epoch(request.relay_version)?;
    let kind = RequestKind::try_from(request.kind).unwrap_or(RequestKind::Unspecified);
    let has_body = !request.body.is_empty();
    match kind {
        RequestKind::Query | RequestKind::Command => {
            let has_reference = request.body_reference.is_some();
            if has_body == has_reference {
                return Err(invalid(
                    "exactly one of body and body_reference is required",
                ));
            }
            if let Some(reference) = &request.body_reference {
                if reference.sha256.len() != 32
                    || reference.ciphertext_bytes == 0
                    || reference.object_id.len() != 32
                    || !reference
                        .object_id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(invalid("invalid payload reference"));
                }
                if request.payload_digest != reference.sha256 {
                    return Err(invalid(
                        "payload_digest must equal the referenced ciphertext digest",
                    ));
                }
            } else if kind == RequestKind::Command && request.payload_digest.len() != 32 {
                // A control command binds the exact body digest; a read-only
                // query may leave it empty.
                return Err(invalid("payload_digest is required for an inline command"));
            }
        }
        RequestKind::PairingAuthorize | RequestKind::Revocation | RequestKind::HostClients => {
            if request.body_reference.is_some() {
                return Err(invalid("an internal Access notice must stay inline"));
            }
        }
        _ => {
            if request.body_reference.is_some() {
                return Err(invalid("this request kind must stay inline"));
            }
        }
    }
    Ok(())
}

/// Validate the per-hop representation rule for a Host result. A result body
/// may be absent when the complete body could not be admitted; the recorded
/// outcome is unchanged. Both representations at once is ambiguous.
pub fn validate_result_representation(result: &HostResult) -> Result<(), AccessError> {
    require_relay_epoch(result.relay_version)?;
    if !result.result_body.is_empty() && result.result_reference.is_some() {
        return Err(invalid(
            "exactly one of result_body and result_reference may be set",
        ));
    }
    Ok(())
}

/// Validate the per-hop representation rule for a live event. The nonce is
/// framing metadata and is always present; exactly one of the ciphertext and
/// its reference is set.
pub fn validate_event_representation(envelope: &EventEnvelope) -> Result<(), AccessError> {
    require_relay_epoch(envelope.relay_version)?;
    if envelope.nonce.is_empty() {
        return Err(invalid("event nonce is required"));
    }
    let has_ciphertext = !envelope.ciphertext.is_empty();
    let has_reference = envelope.payload_reference.is_some();
    if has_ciphertext == has_reference {
        return Err(invalid(
            "exactly one of ciphertext and payload_reference is required",
        ));
    }
    Ok(())
}

/// Validate a live-class event envelope. Live previews are QoS 0 derived
/// state and are always an inline ciphertext: they never use the HTTPS
/// reference carrier. The class payload rule is enforced again after
/// decryption by observation.
pub fn validate_live_event_representation(envelope: &EventEnvelope) -> Result<(), AccessError> {
    validate_event_representation(envelope)?;
    if envelope.payload_reference.is_some() {
        return Err(invalid(
            "a live preview must carry inline ciphertext, not a payload reference",
        ));
    }
    Ok(())
}

/// Validate the per-hop representation rule for the status copy stored by
/// Access. Exactly one complete body representation is present when content
/// exists; a metadata-only terminal carries neither.
pub fn validate_request_state_representation(
    state: &whalerein_proto::RequestState,
) -> Result<(), AccessError> {
    if !state.result_body.is_empty() && state.result_reference.is_some() {
        return Err(invalid(
            "exactly one of result_body and result_reference may be set",
        ));
    }
    Ok(())
}

/// Resolve a stored request status to its complete verified result body.
/// `None` means the recorded outcome has no complete body. `max_ciphertext_bytes`
/// is the bound the request admitted to Access: a reference that declares more
/// is refused before any fetch.
pub async fn resolve_request_state_body(
    client: &AccessClient,
    state: &whalerein_proto::RequestState,
    max_ciphertext_bytes: u64,
) -> Result<Option<Vec<u8>>, AccessError> {
    validate_request_state_representation(state)?;
    match &state.result_reference {
        Some(reference) => Ok(Some(
            resolve_reference(client, reference, max_ciphertext_bytes).await?,
        )),
        None if state.result_body.is_empty() => Ok(None),
        None => Ok(Some(state.result_body.clone())),
    }
}

/// Fetch one referenced ciphertext from the Access HTTPS directory and verify
/// the exact reserved length and SHA-256 before returning it. A truncated,
/// longer, corrupt or substituted body is rejected: a failed fetch never
/// becomes a completed message. A reference that declares more than
/// `max_ciphertext_bytes` is refused before the request so a peer cannot make
/// the client buffer an unbounded object.
pub async fn resolve_reference(
    client: &AccessClient,
    reference: &PayloadReference,
    max_ciphertext_bytes: u64,
) -> Result<Vec<u8>, AccessError> {
    if reference.object_id.len() != 32
        || !reference
            .object_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("invalid payload reference object id"));
    }
    if reference.sha256.len() != 32 {
        return Err(invalid("invalid payload reference digest"));
    }
    if reference.ciphertext_bytes > max_ciphertext_bytes {
        return Err(payload_too_large(
            "referenced ciphertext exceeds the admitted transport bound",
        ));
    }
    let bytes = client.fetch_relay_object(reference).await?;
    if bytes.len() as u64 != reference.ciphertext_bytes {
        return Err(invalid(
            "referenced ciphertext length does not match the reserved length",
        ));
    }
    if crate::util::sha256(&bytes).as_slice() != reference.sha256.as_slice() {
        return Err(invalid(
            "referenced ciphertext digest does not match the reserved digest",
        ));
    }
    Ok(bytes)
}

/// Resolve a Host result body to its complete verified bytes. `None` means the
/// result recorded an outcome without a complete body (metadata-only terminal).
#[allow(dead_code)]
pub async fn resolve_result_body(
    client: &AccessClient,
    result: &HostResult,
    max_ciphertext_bytes: u64,
) -> Result<Option<Vec<u8>>, AccessError> {
    validate_result_representation(result)?;
    match &result.result_reference {
        Some(reference) => Ok(Some(
            resolve_reference(client, reference, max_ciphertext_bytes).await?,
        )),
        None if result.result_body.is_empty() => Ok(None),
        None => Ok(Some(result.result_body.clone())),
    }
}

/// Resolve one live event to its complete ciphertext. The returned envelope
/// carries the verified inline representation so AEAD and business validation
/// are identical for both carriers.
pub async fn resolve_event_envelope(
    client: &AccessClient,
    envelope: &EventEnvelope,
    max_ciphertext_bytes: u64,
) -> Result<EventEnvelope, AccessError> {
    validate_event_representation(envelope)?;
    let Some(reference) = &envelope.payload_reference else {
        return Ok(envelope.clone());
    };
    let ciphertext = resolve_reference(client, reference, max_ciphertext_bytes).await?;
    let mut resolved = envelope.clone();
    resolved.ciphertext = ciphertext;
    resolved.payload_reference = None;
    Ok(resolved)
}

/// Encoded length of one MQTT 5 PUBLISH packet, counting the fixed header,
/// topic name, packet identifier, properties and payload.
pub fn encoded_publish_bytes(topic: &str, properties_bytes: u64, payload_bytes: u64) -> u64 {
    let remaining = 2u64
        + topic.len() as u64
        + 2
        + varint_len(properties_bytes)
        + properties_bytes
        + payload_bytes;
    1 + varint_len(remaining) + remaining
}

/// Number of bytes a protobuf/MQTT variable-length integer occupies.
pub fn varint_len(value: u64) -> u64 {
    let mut value = value;
    let mut bytes = 1;
    while value >= 0x80 {
        value >>= 7;
        bytes += 1;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_proto::{request_payload, EventPayload, SessionUpdate};
    use whalerein_proto::{HostResult, OnlineStatus, PayloadPurpose};

    fn reference() -> PayloadReference {
        PayloadReference {
            object_id: "a".repeat(32),
            owner_node_id: "node-1".into(),
            ciphertext_bytes: 3,
            sha256: crate::util::sha256(b"abc").to_vec(),
            expires_at_unix_ms: 1,
            purpose: PayloadPurpose::Result as i32,
        }
    }

    fn request(kind: RequestKind) -> HostRequest {
        HostRequest {
            request_id: "r".into(),
            host_id: "h".into(),
            device_id: "d".into(),
            kind: kind as i32,
            relay_version: RELAY_EPOCH_V4,
            ..Default::default()
        }
    }

    #[test]
    fn request_representation_requires_exactly_one_carrier() {
        let mut both = request(RequestKind::Command);
        both.body = vec![1];
        both.payload_digest = crate::util::sha256(&both.body).to_vec();
        both.body_reference = Some(reference());
        assert_eq!(
            validate_request_representation(&both).unwrap_err().code,
            Some(ErrorCode::InvalidArgument)
        );

        let neither = request(RequestKind::Command);
        assert!(validate_request_representation(&neither).is_err());

        let mut inline = request(RequestKind::Command);
        inline.body = b"abc".to_vec();
        inline.payload_digest = crate::util::sha256(&inline.body).to_vec();
        assert!(validate_request_representation(&inline).is_ok());

        let mut referenced = request(RequestKind::Command);
        referenced.body_reference = Some(reference());
        referenced.payload_digest = reference().sha256;
        assert!(validate_request_representation(&referenced).is_ok());

        // A mismatched digest is rejected before any fetch.
        let mut mismatched = referenced.clone();
        mismatched.payload_digest = vec![0; 32];
        assert!(validate_request_representation(&mismatched).is_err());
    }

    #[test]
    fn bootstrap_requests_must_stay_inline() {
        let mut handshake = request(RequestKind::Handshake);
        handshake.body = b"noise".to_vec();
        handshake.body_reference = Some(reference());
        assert!(validate_request_representation(&handshake).is_err());

        let mut access_ready = request(RequestKind::AccessReady);
        access_ready.body = b"advertisement".to_vec();
        assert!(validate_request_representation(&access_ready).is_ok());
    }

    #[test]
    fn only_epoch_four_is_accepted() {
        // Epoch 3 is reserved: it is never accepted and has no dual path.
        for version in [0u32, 2, RELAY_EPOCH_V4 - 1] {
            let error = require_relay_epoch(version).unwrap_err();
            assert_eq!(error.code, Some(ErrorCode::ProtocolVersionIncompatible));
        }
        assert!(require_relay_epoch(RELAY_EPOCH_V4).is_ok());

        let mut request = request(RequestKind::Command);
        request.body = b"abc".to_vec();
        request.payload_digest = crate::util::sha256(&request.body).to_vec();
        request.relay_version = 2;
        assert_eq!(
            validate_request_representation(&request).unwrap_err().code,
            Some(ErrorCode::ProtocolVersionIncompatible)
        );
    }

    #[test]
    fn event_representation_requires_one_carrier_and_a_nonce() {
        let mut inline = EventEnvelope {
            host_id: "h".into(),
            device_id: "d".into(),
            channel_id: "c".into(),
            nonce: vec![1; 12],
            ciphertext: vec![2; 4],
            relay_version: RELAY_EPOCH_V4,
            ..Default::default()
        };
        assert!(validate_event_representation(&inline).is_ok());
        inline.payload_reference = Some(reference());
        assert!(validate_event_representation(&inline).is_err());
        let both = inline.clone();
        let mut inline_only = both;
        inline_only.ciphertext = vec![3; 4];
        inline_only.payload_reference = None;
        assert!(validate_event_representation(&inline_only).is_ok());
        let mut neither = EventEnvelope {
            nonce: vec![1; 12],
            relay_version: RELAY_EPOCH_V4,
            ..Default::default()
        };
        assert!(validate_event_representation(&neither).is_err());
        neither.payload_reference = Some(reference());
        assert!(validate_event_representation(&neither).is_ok());
        neither.ciphertext = vec![9];
        assert!(validate_event_representation(&neither).is_err());
        let mut missing_nonce = inline_only;
        missing_nonce.nonce.clear();
        assert!(validate_event_representation(&missing_nonce).is_err());
        missing_nonce.nonce = vec![1; 12];
        missing_nonce.relay_version = 2;
        assert_eq!(
            validate_event_representation(&missing_nonce)
                .unwrap_err()
                .code,
            Some(ErrorCode::ProtocolVersionIncompatible)
        );
        let mut live = EventEnvelope {
            nonce: vec![1; 12],
            ciphertext: vec![2; 4],
            relay_version: RELAY_EPOCH_V4,
            ..Default::default()
        };
        assert!(validate_live_event_representation(&live).is_ok());
        // Live previews never use the HTTPS reference carrier.
        live.ciphertext.clear();
        live.payload_reference = Some(reference());
        assert_eq!(
            validate_live_event_representation(&live).unwrap_err().code,
            Some(ErrorCode::InvalidArgument)
        );
        let _ = SessionUpdate::default();
        let _ = EventPayload::default();
        let _ = request_payload::Payload::Ping(Default::default());
    }

    #[test]
    fn host_result_representation_allows_a_metadata_only_terminal() {
        let mut result = HostResult {
            request_id: "r".into(),
            status: OnlineStatus::Succeeded as i32,
            relay_version: RELAY_EPOCH_V4,
            ..Default::default()
        };
        assert!(validate_result_representation(&result).is_ok());
        result.result_body = vec![1];
        result.result_reference = Some(reference());
        assert!(validate_result_representation(&result).is_err());
        let mut referenced = HostResult {
            request_id: "r".into(),
            relay_version: RELAY_EPOCH_V4,
            result_reference: Some(reference()),
            ..Default::default()
        };
        assert!(validate_result_representation(&referenced).is_ok());
        referenced.relay_version = 2;
        assert_eq!(
            validate_result_representation(&referenced)
                .unwrap_err()
                .code,
            Some(ErrorCode::ProtocolVersionIncompatible)
        );
    }

    #[test]
    fn publish_arithmetic_counts_topic_identifier_and_properties() {
        // remaining = 2 + topic + 2 + varint(properties) + properties + payload
        assert_eq!(varint_len(0), 1);
        assert_eq!(varint_len(127), 1);
        assert_eq!(varint_len(128), 2);
        assert_eq!(varint_len(16_383), 2);
        assert_eq!(varint_len(16_384), 3);
        assert_eq!(
            encoded_publish_bytes("t", 0, 10),
            1 + 1 + (2 + 1 + 2 + 1 + 10)
        );
        assert_eq!(
            encoded_publish_bytes("topic", 5, 0),
            1 + 1 + (2 + 5 + 2 + 1 + 5)
        );
        // A larger remaining length costs another varint byte.
        let small = encoded_publish_bytes("t", 0, 100);
        let large = encoded_publish_bytes("t", 0, 200);
        // A larger remaining length adds one varint byte: 206 - 106 + 1.
        assert_eq!(large - small, 101);
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn packaged_profiles_keep_the_shared_geometry() {
        let default = TransportBudget::resolve(&TransportSettings::default()).unwrap();
        assert_eq!(default.profile, TransportProfile::Default);
        assert_eq!(default.packet_bytes, 64 * 1024);
        assert_eq!(default.packet_overhead_bytes, 4096);
        assert_eq!(default.envelope_bytes, 16 * 1024 * 1024);
        for profile in [
            TransportProfile::Default,
            TransportProfile::Compact,
            TransportProfile::Roomy,
        ] {
            let budget = TransportBudget::resolve(&TransportSettings {
                profile,
                overrides: None,
            })
            .unwrap();
            assert_eq!(budget.profile, profile);
            assert!(budget.packet_overhead_bytes < budget.packet_bytes);
            assert!(budget.envelope_bytes >= u64::from(budget.packet_bytes));
            assert!(budget.reserved_bytes >= budget.envelope_bytes);
            assert!(budget.inline_request_bytes() < u64::from(budget.packet_bytes));
        }
    }

    #[test]
    fn overrides_are_validated_before_connection_use() {
        let settings = TransportSettings {
            profile: TransportProfile::Compact,
            overrides: Some(TransportOverrides {
                transfer_timeout_ms: Some(5_000),
                ..Default::default()
            }),
        };
        assert_eq!(
            TransportBudget::resolve(&settings)
                .unwrap()
                .transfer_timeout_ms,
            5_000
        );

        let settings = TransportSettings {
            profile: TransportProfile::Compact,
            overrides: Some(TransportOverrides {
                reserved_bytes: Some(1),
                ..Default::default()
            }),
        };
        assert!(TransportBudget::resolve(&settings).is_err());
        let settings = TransportSettings {
            profile: TransportProfile::Compact,
            overrides: Some(TransportOverrides {
                transfer_timeout_ms: Some(0),
                ..Default::default()
            }),
        };
        assert!(TransportBudget::resolve(&settings).is_err());
        let settings = TransportSettings {
            profile: TransportProfile::Compact,
            overrides: Some(TransportOverrides {
                envelope_bytes: Some(64),
                ..Default::default()
            }),
        };
        assert!(TransportBudget::resolve(&settings)
            .unwrap_err()
            .contains("cover one packet"));
        let settings = TransportSettings {
            profile: TransportProfile::Compact,
            overrides: Some(TransportOverrides {
                packet_bytes: Some(2048),
                ..Default::default()
            }),
        };
        assert!(TransportBudget::resolve(&settings)
            .unwrap_err()
            .contains("overhead"));
    }

    fn overridden_advertisement() -> ReceiveAdvertisement {
        let mut advertisement = TransportBudget::default().advertisement(7).unwrap();
        let budget = advertisement.budget.as_mut().unwrap();
        budget.packet_bytes = 48 * 1024;
        budget.envelope_bytes = 8 * 1024 * 1024;
        budget.bulk_concurrency = 3;
        budget.reserved_bytes = 8 * 1024 * 1024;
        budget.transfer_timeout_ms = 60_000;
        advertisement
    }

    #[test]
    fn relation_valid_overrides_are_accepted_without_clamping() {
        let advertisement = overridden_advertisement();
        let (peer, generation) = TransportBudget::from_advertisement(&advertisement).unwrap();
        assert_eq!(generation, 7);
        assert_eq!(peer.profile, TransportProfile::Default);
        assert_eq!(peer.packet_bytes, 48 * 1024);
        assert_eq!(peer.envelope_bytes, 8 * 1024 * 1024);
        assert_eq!(peer.bulk_concurrency, 3);
        assert_eq!(peer.packet_overhead_bytes, 4096);
        // The packaged profile is only the base label, not a value clamp.
        assert_ne!(peer, TransportBudget::default());
    }

    #[test]
    fn advertisements_reject_missing_unknown_and_contradictory_values() {
        let advertisement = overridden_advertisement();
        assert!(TransportBudget::default().advertisement(0).is_err());

        let mut zero_generation = advertisement;
        zero_generation.connection_generation = 0;
        assert!(TransportBudget::from_advertisement(&zero_generation).is_err());

        let mut missing = advertisement;
        missing.budget = None;
        assert!(TransportBudget::from_advertisement(&missing).is_err());

        let mut unknown = advertisement;
        unknown.profile = 99;
        assert!(TransportBudget::from_advertisement(&unknown).is_err());
        let mut unspecified = advertisement;
        unspecified.profile = 0;
        assert!(TransportBudget::from_advertisement(&unspecified).is_err());

        let mut zero = advertisement;
        zero.budget.as_mut().unwrap().packet_bytes = 0;
        assert!(TransportBudget::from_advertisement(&zero)
            .unwrap_err()
            .contains("positive"));

        let mut packet = advertisement;
        packet.budget.as_mut().unwrap().packet_bytes = 4 * 1024;
        assert!(TransportBudget::from_advertisement(&packet)
            .unwrap_err()
            .contains("overhead"));

        let mut reserved = advertisement;
        reserved.budget.as_mut().unwrap().reserved_bytes = 1024 * 1024;
        assert!(TransportBudget::from_advertisement(&reserved)
            .unwrap_err()
            .contains("one envelope"));

        // A different relay epoch is never a valid advertisement.
        let mut epoch = advertisement;
        epoch.relay_version = 2;
        assert!(TransportBudget::from_advertisement(&epoch).is_err());
    }
}
