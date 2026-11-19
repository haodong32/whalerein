//! Bounded receiver for opaque logical MQTT envelopes. AEAD verification stays
//! in security and must succeed before a reconstructed message reaches business.
use super::AccessError;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use whalerein_proto::{
    ChannelHello, ChannelReady, ReceiveAdvertisement, TransportBudget as WireTransportBudget,
    TransportFrame,
};

pub const VERSION: u32 = 2;
/// Default logical envelope retained for callers that only need the baseline.
pub const MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const PREFIX: &[u8] = b"WRMQTT2";

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
    pub fragment_payload_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bulk_concurrency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_idle_timeout_ms: Option<u32>,
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
    pub fragment_payload_bytes: u32,
    pub fragment_count: u32,
    pub envelope_bytes: u64,
    pub bulk_concurrency: u32,
    pub reserved_bytes: u64,
    pub fragment_idle_timeout_ms: u32,
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
            fragment_payload_bytes: values.fragment_payload_bytes,
            fragment_count: values.fragment_count,
            envelope_bytes: values.envelope_bytes,
            bulk_concurrency: values.bulk_concurrency,
            reserved_bytes: values.reserved_bytes,
            fragment_idle_timeout_ms: values.fragment_idle_timeout_ms,
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
            if let Some(value) = overrides.fragment_payload_bytes {
                budget.fragment_payload_bytes = value;
            }
            if let Some(value) = overrides.fragment_count {
                budget.fragment_count = value;
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
            if let Some(value) = overrides.fragment_idle_timeout_ms {
                budget.fragment_idle_timeout_ms = value;
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
            || self.fragment_payload_bytes == 0
            || self.fragment_count == 0
            || self.envelope_bytes == 0
            || self.bulk_concurrency == 0
            || self.reserved_bytes == 0
            || self.fragment_idle_timeout_ms == 0
            || self.transfer_timeout_ms == 0
        {
            return Err("transport limits must be positive".into());
        }
        if u64::from(self.fragment_payload_bytes) + u64::from(self.packet_overhead_bytes)
            > u64::from(self.packet_bytes)
        {
            return Err("fragment payload exceeds packetBytes - packetOverheadBytes".into());
        }
        let required = self
            .envelope_bytes
            .div_ceil(u64::from(self.fragment_payload_bytes));
        if u64::from(self.fragment_count) < required {
            return Err("fragment count does not cover the envelope".into());
        }
        if self.reserved_bytes < self.envelope_bytes {
            return Err("reserved bytes do not cover one envelope".into());
        }
        let in_flight = u64::from(self.bulk_concurrency)
            .checked_mul(u64::from(self.fragment_payload_bytes))
            .ok_or_else(|| "in-flight fragment reservation overflows".to_string())?;
        if self.reserved_bytes < in_flight {
            return Err("reserved bytes do not cover in-flight fragments".into());
        }
        if self.fragment_idle_timeout_ms >= self.transfer_timeout_ms {
            return Err("fragment idle timeout must be below the transfer timeout".into());
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
            fragment_payload_bytes: self.fragment_payload_bytes,
            fragment_count: self.fragment_count,
            envelope_bytes: self.envelope_bytes,
            bulk_concurrency: self.bulk_concurrency,
            reserved_bytes: self.reserved_bytes,
            fragment_idle_timeout_ms: self.fragment_idle_timeout_ms,
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
        })
    }

    /// Validate a peer advertisement against the packaged profile vocabulary
    /// and its internal relations. The named profile supplies the base
    /// geometry (including the packet overhead that is not carried on the
    /// wire), but a peer may advertise a relation-valid override. A peer
    /// cannot invent a profile, a missing budget, a non-positive value or a
    /// contradictory relation.
    pub fn from_advertisement(advertisement: &ReceiveAdvertisement) -> Result<(Self, u64), String> {
        if advertisement.connection_generation == 0 {
            return Err("receive advertisement generation missing".into());
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
            fragment_payload_bytes: budget.fragment_payload_bytes,
            fragment_count: budget.fragment_count,
            envelope_bytes: budget.envelope_bytes,
            bulk_concurrency: budget.bulk_concurrency,
            reserved_bytes: budget.reserved_bytes,
            fragment_idle_timeout_ms: budget.fragment_idle_timeout_ms,
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

/// A bounded, fallible receive admission error used only for diagnostics.
fn budget_exhausted() -> AccessError {
    AccessError::protocol("logical MQTT receive budget exhausted")
}
struct State {
    total: usize,
    count: usize,
    digest: Vec<u8>,
    parts: HashMap<u32, Vec<u8>>,
    until: Instant,
}
pub struct Receiver {
    budget: TransportBudget,
    incoming: HashMap<String, State>,
    completed: HashMap<String, State>,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new(TransportBudget::default())
    }
}
pub struct Receipt {
    pub logical: Option<Vec<u8>>,
    pub ack: Vec<u8>,
}
fn invalid() -> AccessError {
    AccessError::protocol("invalid logical MQTT transport frame")
}

impl Receiver {
    pub fn new(budget: TransportBudget) -> Self {
        Self {
            budget,
            incoming: HashMap::new(),
            completed: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.incoming.clear();
        self.completed.clear();
    }
    pub fn receive(&mut self, topic: &str, bytes: &[u8]) -> Result<Option<Receipt>, AccessError> {
        if !bytes.starts_with(PREFIX) {
            return Ok(None);
        }
        if bytes.len() > self.budget.packet_bytes as usize {
            return Err(invalid());
        }
        let frame = TransportFrame::decode(&bytes[PREFIX.len()..]).map_err(|_| invalid())?;
        let total = frame.total_bytes as usize;
        let count = frame.count as usize;
        let index = frame.index as usize;
        let width = frame.fragment_payload_bytes as usize;
        if frame.version != VERSION
            || frame.ack
            || frame.topic != topic
            || frame.transfer_id.len() != 32
            || !frame
                .transfer_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || frame.digest.len() != 32
            || total == 0
            || total > self.budget.envelope_bytes as usize
            || width == 0
            || width > self.budget.fragment_payload_bytes as usize
            || count != total.div_ceil(width)
            || index >= count
            || frame.payload.len() != width.min(total - index * width)
            || (frame.profile != 0 && frame.profile != self.budget.profile.wire() as i32)
        {
            return Err(invalid());
        }
        let now = Instant::now();
        self.incoming.retain(|_, state| state.until > now);
        self.completed.retain(|_, state| state.until > now);
        let key = frame.transfer_id.clone();
        let mut logical = None;
        if !self.completed.contains_key(&key) {
            if !self.incoming.contains_key(&key) {
                let reserved: usize = self.incoming.values().map(|state| state.total).sum();
                let width = self.budget.fragment_payload_bytes as usize;
                if self.completed.len() >= 16384
                    || (total > width
                        && (self.incoming.len() >= self.budget.bulk_concurrency as usize
                            || reserved.saturating_add(total)
                                > self.budget.reserved_bytes as usize))
                {
                    return Err(budget_exhausted());
                }
                self.incoming.insert(
                    key.clone(),
                    State {
                        total,
                        count,
                        digest: frame.digest.clone(),
                        parts: HashMap::new(),
                        until: now
                            + Duration::from_millis(u64::from(
                                self.budget.fragment_idle_timeout_ms,
                            )),
                    },
                );
            }
            let state = self.incoming.get_mut(&key).expect("inserted transport");
            if state.total != total || state.count != count || state.digest != frame.digest {
                return Err(invalid());
            }
            if let Some(existing) = state.parts.get(&frame.index) {
                if existing != &frame.payload {
                    return Err(invalid());
                }
            }
            state.parts.insert(frame.index, frame.payload);
            if state.parts.len() == count {
                let mut body = Vec::with_capacity(total);
                for index in 0..count {
                    body.extend_from_slice(&state.parts[&(index as u32)]);
                }
                if crate::util::sha256(&body).as_slice() != state.digest {
                    self.incoming.remove(&key);
                    return Err(invalid());
                }
                let mut state = self.incoming.remove(&key).expect("complete transport");
                state.parts.clear();
                state.until =
                    now + Duration::from_millis(u64::from(self.budget.fragment_idle_timeout_ms));
                self.completed.insert(key, state);
                logical = Some(body);
            }
        } else {
            let state = &self.completed[&key];
            if state.total != total || state.count != count || state.digest != frame.digest {
                return Err(invalid());
            }
        }
        let complete = logical.is_some() || self.completed.contains_key(&frame.transfer_id);
        let ack = TransportFrame {
            version: VERSION,
            transfer_id: frame.transfer_id,
            topic: frame.topic,
            index: frame.index,
            digest: frame.digest,
            ack: true,
            complete,
            fragment_payload_bytes: frame.fragment_payload_bytes,
            profile: self.budget.profile.wire() as i32,
            ..Default::default()
        };
        let mut bytes = PREFIX.to_vec();
        ack.encode(&mut bytes).map_err(|_| invalid())?;
        Ok(Some(Receipt {
            logical,
            ack: bytes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const WIDTH: usize = 60 * 1024;
    fn frames(body: &[u8]) -> Vec<Vec<u8>> {
        body.chunks(WIDTH)
            .enumerate()
            .map(|(index, part)| {
                let frame = TransportFrame {
                    version: VERSION,
                    transfer_id: "a".repeat(32),
                    topic: "event".into(),
                    index: index as u32,
                    count: body.len().div_ceil(WIDTH) as u32,
                    total_bytes: body.len() as u32,
                    digest: crate::util::sha256(body).to_vec(),
                    payload: part.to_vec(),
                    fragment_payload_bytes: WIDTH as u32,
                    profile: whalerein_proto::TransportProfile::Default as i32,
                    ..Default::default()
                };
                let mut bytes = PREFIX.to_vec();
                frame.encode(&mut bytes).unwrap();
                bytes
            })
            .collect()
    }
    #[test]
    fn reordered_and_duplicate_parts_deliver_once() {
        let body = vec![0u8; 2 * 1024 * 1024];
        let frames = frames(&body);
        let mut receiver = Receiver::default();
        let mut delivered = 0;
        for bytes in frames.iter().rev().chain(frames.iter()) {
            assert!(bytes.len() <= TransportBudget::default().packet_bytes as usize);
            let receipt = receiver.receive("event", bytes).unwrap().unwrap();
            let ack = TransportFrame::decode(&receipt.ack[PREFIX.len()..]).unwrap();
            assert!(ack.ack);
            if let Some(logical) = receipt.logical {
                assert_eq!(logical.len(), body.len());
                delivered += 1;
            }
        }
        assert_eq!(delivered, 1);
        receiver.clear();
        assert!(receiver.incoming.is_empty());
    }
    #[test]
    fn rejects_metadata_tampering_and_incomplete_messages() {
        let mut receiver = Receiver::default();
        assert!(receiver.receive("event", b"legacy").unwrap().is_none());
        assert!(receiver.receive("event", PREFIX).is_err());
        let body = vec![0; WIDTH + 10];
        let frames = frames(&body);
        assert!(receiver.receive("other", &frames[0]).is_err());
        assert!(receiver
            .receive("event", &frames[0])
            .unwrap()
            .unwrap()
            .logical
            .is_none());
        let mut frame = TransportFrame::decode(&frames[1][PREFIX.len()..]).unwrap();
        frame.payload[0] = 1;
        let mut corrupt = PREFIX.to_vec();
        frame.encode(&mut corrupt).unwrap();
        assert!(receiver.receive("event", &corrupt).is_err());
        for index in 0..4 {
            let mut frame = TransportFrame::decode(&frames[0][PREFIX.len()..]).unwrap();
            frame.transfer_id = format!("{index:032x}");
            let mut bytes = PREFIX.to_vec();
            frame.encode(&mut bytes).unwrap();
            receiver.receive("event", &bytes).unwrap();
        }
        let mut frame = TransportFrame::decode(&frames[0][PREFIX.len()..]).unwrap();
        frame.transfer_id = "f".repeat(32);
        let mut bytes = PREFIX.to_vec();
        frame.encode(&mut bytes).unwrap();
        assert!(receiver.receive("event", &bytes).is_err());
    }

    #[test]
    fn compact_budget_rejects_wider_frames_and_oversized_envelopes() {
        let compact = TransportBudget::resolve(&TransportSettings {
            profile: TransportProfile::Compact,
            overrides: None,
        })
        .unwrap();
        let mut receiver = Receiver::new(compact);
        let body = vec![0u8; compact.fragment_payload_bytes as usize + 1];
        let frames = frames(&body);
        assert!(receiver.receive("event", &frames[0]).is_err());
        assert_eq!(
            TransportBudget::resolve(&TransportSettings::default())
                .unwrap()
                .profile,
            TransportProfile::Default
        );
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
        assert_eq!(default.fragment_payload_bytes, 60 * 1024);
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
            assert!(budget.fragment_payload_bytes < budget.packet_bytes);
            assert!(
                budget.fragment_count as u64
                    >= budget
                        .envelope_bytes
                        .div_ceil(u64::from(budget.fragment_payload_bytes))
            );
            assert!(budget.reserved_bytes >= budget.envelope_bytes);
            assert!(budget.fragment_idle_timeout_ms < budget.transfer_timeout_ms);
        }
    }

    #[test]
    fn overrides_are_validated_before_connection_use() {
        let mut settings = TransportSettings {
            profile: TransportProfile::Compact,
            overrides: None,
        };
        settings.overrides = Some(TransportOverrides {
            fragment_idle_timeout_ms: Some(5_000),
            ..Default::default()
        });
        let budget = TransportBudget::resolve(&settings).unwrap();
        assert_eq!(budget.fragment_idle_timeout_ms, 5_000);

        settings.overrides = Some(TransportOverrides {
            fragment_count: Some(1),
            ..Default::default()
        });
        assert!(TransportBudget::resolve(&settings)
            .unwrap_err()
            .contains("fragment count"));
        settings.overrides = Some(TransportOverrides {
            reserved_bytes: Some(1),
            ..Default::default()
        });
        assert!(TransportBudget::resolve(&settings).is_err());
        settings.overrides = Some(TransportOverrides {
            transfer_timeout_ms: Some(1),
            ..Default::default()
        });
        assert!(TransportBudget::resolve(&settings)
            .unwrap_err()
            .contains("timeout"));
        settings.overrides = Some(TransportOverrides {
            envelope_bytes: Some(64),
            ..Default::default()
        });
        assert!(TransportBudget::resolve(&settings)
            .unwrap_err()
            .contains("minimum"));
        settings.overrides = Some(TransportOverrides {
            fragment_payload_bytes: Some(40 * 1024),
            ..Default::default()
        });
        assert!(TransportBudget::resolve(&settings)
            .unwrap_err()
            .contains("fragment payload"));
    }

    fn overridden_advertisement() -> ReceiveAdvertisement {
        let mut advertisement = TransportBudget::default().advertisement(7).unwrap();
        let budget = advertisement.budget.as_mut().unwrap();
        budget.packet_bytes = 48 * 1024;
        budget.fragment_payload_bytes = 40 * 1024;
        budget.fragment_count = 205;
        budget.envelope_bytes = 8 * 1024 * 1024;
        budget.bulk_concurrency = 3;
        budget.reserved_bytes = 8 * 1024 * 1024;
        budget.fragment_idle_timeout_ms = 10_000;
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
        assert_eq!(peer.fragment_payload_bytes, 40 * 1024);
        assert_eq!(peer.fragment_count, 205);
        assert_eq!(peer.envelope_bytes, 8 * 1024 * 1024);
        assert_eq!(peer.bulk_concurrency, 3);
        assert_eq!(peer.reserved_bytes, 8 * 1024 * 1024);
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
        zero.budget.as_mut().unwrap().fragment_payload_bytes = 0;
        assert!(TransportBudget::from_advertisement(&zero)
            .unwrap_err()
            .contains("positive"));

        let mut packet = advertisement;
        packet.budget.as_mut().unwrap().packet_bytes = 8 * 1024;
        assert!(TransportBudget::from_advertisement(&packet)
            .unwrap_err()
            .contains("fragment payload"));

        let mut count = advertisement;
        count.budget.as_mut().unwrap().fragment_count = 1;
        assert!(TransportBudget::from_advertisement(&count)
            .unwrap_err()
            .contains("fragment count"));

        let mut reserved = advertisement;
        reserved.budget.as_mut().unwrap().reserved_bytes = 1024 * 1024;
        assert!(TransportBudget::from_advertisement(&reserved)
            .unwrap_err()
            .contains("one envelope"));

        let mut in_flight = advertisement;
        let budget = in_flight.budget.as_mut().unwrap();
        budget.envelope_bytes = 1024 * 1024;
        budget.fragment_count = 26;
        budget.bulk_concurrency = 100;
        budget.reserved_bytes = 2 * 1024 * 1024;
        assert!(TransportBudget::from_advertisement(&in_flight)
            .unwrap_err()
            .contains("in-flight"));

        let mut timeouts = advertisement;
        timeouts.budget.as_mut().unwrap().fragment_idle_timeout_ms = 60_000;
        assert!(TransportBudget::from_advertisement(&timeouts)
            .unwrap_err()
            .contains("timeout"));

        // An envelope that the u32 fragment count cannot cover is rejected
        // even though the wire carries a u64 byte count.
        let mut overflow = advertisement;
        let budget = overflow.budget.as_mut().unwrap();
        budget.fragment_payload_bytes = 1;
        budget.envelope_bytes = u64::MAX;
        budget.fragment_count = u32::MAX;
        assert!(TransportBudget::from_advertisement(&overflow)
            .unwrap_err()
            .contains("fragment count"));
    }
}
