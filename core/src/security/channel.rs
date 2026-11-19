//! Secure application channel: HKDF key schedule and ChaCha20-Poly1305 frames.
//!
//! The AAD strings are part of the wire contract. They bind a frame to the
//! request (or event) metadata that Access relays.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use hkdf::Hkdf;
use prost::Message;
use sha2::Sha256;
use whalerein_proto::{EncryptedFrame, RequestKind};

use super::SecurityError;

pub const CHANNEL_KEYS_INFO: &str = "whalerein/v1/channel-keys";
pub const NONCE_LEN: usize = 12;

/// Direction of an application message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Device to host: `c2h`.
    DeviceToHost,
    /// Host to device: `h2c`.
    HostToDevice,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ChannelKeys {
    pub c2h: [u8; 32],
    pub h2c: [u8; 32],
}

impl std::fmt::Debug for ChannelKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelKeys(..)")
    }
}

/// Derive the application keys from the Noise handshake hash and prologue.
///
/// The secure channel id is the client-chosen random id in the `HostRequest`
/// envelope. It is not derived from the handshake hash; the prologue binds it
/// to the handshake, so it is authenticated by Noise.
pub fn derive_channel_keys(
    handshake_hash: &[u8],
    prologue: &str,
) -> Result<ChannelKeys, SecurityError> {
    let hash: [u8; 32] = handshake_hash
        .try_into()
        .map_err(|_| SecurityError::InvalidKey)?;
    let hkdf = Hkdf::<Sha256>::new(Some(prologue.as_bytes()), &hash);
    let mut okm = [0u8; 64];
    hkdf.expand(CHANNEL_KEYS_INFO.as_bytes(), &mut okm)
        .map_err(|_| SecurityError::Crypto)?;

    let mut c2h = [0u8; 32];
    let mut h2c = [0u8; 32];
    c2h.copy_from_slice(&okm[..32]);
    h2c.copy_from_slice(&okm[32..]);
    Ok(ChannelKeys { c2h, h2c })
}

/// An established secure channel. A reconnect always replaces this value.
#[derive(Clone)]
pub struct SecureChannel {
    host_id: String,
    device_id: String,
    channel_id: String,
    keys: ChannelKeys,
}

impl std::fmt::Debug for SecureChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecureChannel")
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

impl SecureChannel {
    pub fn from_handshake(
        handshake_hash: &[u8],
        prologue: &str,
        channel_id: impl Into<String>,
        host_id: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Result<Self, SecurityError> {
        let channel_id = channel_id.into();
        if channel_id.is_empty() {
            return Err(SecurityError::InvalidKey);
        }
        Ok(Self {
            host_id: host_id.into(),
            device_id: device_id.into(),
            channel_id,
            keys: derive_channel_keys(handshake_hash, prologue)?,
        })
    }

    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn seal_request(
        &self,
        aad: &RequestAad<'_>,
        plaintext: &[u8],
    ) -> Result<EncryptedFrame, SecurityError> {
        self.seal(Direction::DeviceToHost, &request_aad(aad), plaintext)
    }

    pub fn open_request(
        &self,
        aad: &RequestAad<'_>,
        frame: &EncryptedFrame,
    ) -> Result<Vec<u8>, SecurityError> {
        self.open(Direction::HostToDevice, frame, &request_aad(aad))
    }

    /// Seal a host-to-device event. Used by the host side and by test fixtures.
    #[cfg(test)]
    pub fn seal_event(
        &self,
        aad: &EventAad<'_>,
        plaintext: &[u8],
    ) -> Result<EncryptedFrame, SecurityError> {
        self.seal(Direction::HostToDevice, &event_aad(aad), plaintext)
    }

    pub fn open_event(
        &self,
        aad: &EventAad<'_>,
        frame: &EncryptedFrame,
    ) -> Result<Vec<u8>, SecurityError> {
        self.open(Direction::HostToDevice, frame, &event_aad(aad))
    }

    /// Host-side view of the same channel, with directions swapped. Test
    /// fixtures and the interop tool use this; the client never does.
    #[cfg(any(test, feature = "test-support"))]
    pub fn peer_view(&self) -> Self {
        Self {
            host_id: self.host_id.clone(),
            device_id: self.device_id.clone(),
            channel_id: self.channel_id.clone(),
            keys: ChannelKeys {
                c2h: self.keys.h2c,
                h2c: self.keys.c2h,
            },
        }
    }

    fn seal(
        &self,
        direction: Direction,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<EncryptedFrame, SecurityError> {
        let key = match direction {
            Direction::DeviceToHost => &self.keys.c2h,
            Direction::HostToDevice => &self.keys.h2c,
        };
        let cipher =
            ChaCha20Poly1305::new_from_slice(key).map_err(|_| SecurityError::InvalidKey)?;
        let nonce_bytes = crate::util::random_bytes(NONCE_LEN);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| SecurityError::Crypto)?;
        Ok(EncryptedFrame {
            channel_id: self.channel_id.clone(),
            nonce: nonce_bytes,
            ciphertext,
        })
    }

    fn open(
        &self,
        direction: Direction,
        frame: &EncryptedFrame,
        aad: &[u8],
    ) -> Result<Vec<u8>, SecurityError> {
        if frame.channel_id != self.channel_id {
            return Err(SecurityError::ChannelMismatch);
        }
        if frame.nonce.len() != NONCE_LEN {
            return Err(SecurityError::InvalidNonce);
        }
        let key = match direction {
            Direction::DeviceToHost => &self.keys.c2h,
            Direction::HostToDevice => &self.keys.h2c,
        };
        let cipher =
            ChaCha20Poly1305::new_from_slice(key).map_err(|_| SecurityError::InvalidKey)?;
        let nonce = Nonce::from_slice(&frame.nonce);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &frame.ciphertext,
                    aad,
                },
            )
            .map_err(|_| SecurityError::AuthenticationFailed)
    }
}

/// Request metadata bound into an encrypted request frame.
#[derive(Debug, Clone)]
pub struct RequestAad<'a> {
    pub request_id: &'a str,
    pub host_id: &'a str,
    pub device_id: &'a str,
    pub kind: RequestKind,
    pub command_id: &'a str,
    pub expires_at_unix_ms: i64,
}

/// Event metadata bound into an encrypted event frame.
#[derive(Debug, Clone)]
pub struct EventAad<'a> {
    pub host_id: &'a str,
    pub device_id: &'a str,
    pub channel_id: &'a str,
    pub created_at_unix_ms: i64,
}

/// Proto3 enum value name used in the request AAD string.
pub fn request_kind_name(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Unspecified => "REQUEST_KIND_UNSPECIFIED",
        RequestKind::Handshake => "REQUEST_KIND_HANDSHAKE",
        RequestKind::Query => "REQUEST_KIND_QUERY",
        RequestKind::Command => "REQUEST_KIND_COMMAND",
        RequestKind::PairingClaim => "REQUEST_KIND_PAIRING_CLAIM",
        RequestKind::PairingAuthorize => "REQUEST_KIND_PAIRING_AUTHORIZE",
        RequestKind::Revocation => "REQUEST_KIND_REVOCATION",
        RequestKind::HostClients => "REQUEST_KIND_HOST_CLIENTS",
    }
}

pub fn request_aad(aad: &RequestAad<'_>) -> Vec<u8> {
    format!(
        "whalerein/v1/request|{}|{}|{}|{}|{}|{}",
        aad.request_id,
        aad.host_id,
        aad.device_id,
        request_kind_name(aad.kind),
        aad.command_id,
        aad.expires_at_unix_ms
    )
    .into_bytes()
}

pub fn event_aad(aad: &EventAad<'_>) -> Vec<u8> {
    format!(
        "whalerein/v1/event|{}|{}|{}|{}",
        aad.host_id, aad.device_id, aad.channel_id, aad.created_at_unix_ms
    )
    .into_bytes()
}

/// Escape one canonical approval field so the `|` separator cannot be forged
/// by content. The mapping is injective (`\` -> `\\`, `|` -> `\|`), matching
/// `host/runtime/src/security/digest.js`; values without those bytes keep
/// their historical digest.
fn escape_approval_field(value: &str) -> String {
    value.replace('\\', "\\\\").replace('|', "\\|")
}

/// Approval digest exactly as defined by the wire contract. The shared
/// vectors in `proto/v1/approval_digest_vectors.json` bind this
/// implementation to the Node connector.
pub fn approval_digest(
    approval_id: &str,
    session_id: &str,
    tool_name: &str,
    call_id: &str,
    reason: &str,
    version: i64,
    expires_at_unix_ms: i64,
) -> [u8; 32] {
    let canonical = format!(
        "whalerein/v1/approval|{}|{}|{}|{}|{}|{version}|{expires_at_unix_ms}",
        escape_approval_field(approval_id),
        escape_approval_field(session_id),
        escape_approval_field(tool_name),
        escape_approval_field(call_id),
        escape_approval_field(reason),
    );
    crate::util::sha256(canonical.as_bytes())
}

/// Seal arbitrary bytes with a raw application key.
///
/// Used by interop tooling and by Core's local draft store; both must bind the
/// ciphertext to caller-supplied associated data.
pub fn seal_with_key(
    key: &[u8; 32],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), SecurityError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| SecurityError::InvalidKey)?;
    let nonce_bytes = crate::util::random_bytes(NONCE_LEN);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| SecurityError::Crypto)?;
    Ok((nonce_bytes, ciphertext))
}

/// Open arbitrary bytes with a raw application key.
///
/// Used by interop tooling and by Core's local draft store.
pub fn open_with_key(
    key: &[u8; 32],
    aad: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, SecurityError> {
    if nonce.len() != NONCE_LEN {
        return Err(SecurityError::InvalidNonce);
    }
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| SecurityError::InvalidKey)?;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| SecurityError::AuthenticationFailed)
}

pub fn encode_frame(frame: &EncryptedFrame) -> Vec<u8> {
    frame.encode_to_vec()
}

pub fn decode_frame(bytes: &[u8]) -> Result<EncryptedFrame, SecurityError> {
    EncryptedFrame::decode(bytes).map_err(|_| SecurityError::InvalidMessage)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROLOGUE: &str = "whalerein/v1/channel|host-1|device-1|nego-1";

    const CHANNEL_ID: &str = "e372075fe44f25a0f0e06bb6604889e9";

    fn channel() -> SecureChannel {
        SecureChannel::from_handshake(&[7u8; 32], PROLOGUE, CHANNEL_ID, "host-1", "device-1")
            .unwrap()
    }

    fn sample_request_aad() -> RequestAad<'static> {
        RequestAad {
            request_id: "req-1",
            host_id: "host-1",
            device_id: "device-1",
            kind: RequestKind::Query,
            command_id: "",
            expires_at_unix_ms: 1_700_000_000_000,
        }
    }

    fn sample_event_aad() -> EventAad<'static> {
        EventAad {
            host_id: "host-1",
            device_id: "device-1",
            channel_id: "chan-1",
            created_at_unix_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn key_schedule_matches_node_vector() {
        let keys = derive_channel_keys(&[7u8; 32], PROLOGUE).unwrap();
        assert_eq!(
            hex::encode(keys.c2h),
            "901c7e63ab3f3a65a863335ab32d1f4ab3b50c1378b0b6078656f40331c42a38"
        );
        assert_eq!(
            hex::encode(keys.h2c),
            "9c86a9a0532d20a11b810d1bb2bc88a2f42f0dd65ea6d1439f65c68d01e272de"
        );
        assert_eq!(
            keys.c2h,
            derive_channel_keys(&[7u8; 32], PROLOGUE).unwrap().c2h
        );
    }

    #[test]
    fn key_schedule_rejects_bad_hash() {
        for length in [0, 1, 3, 31, 33, 64] {
            assert!(matches!(
                derive_channel_keys(&vec![1; length], PROLOGUE),
                Err(SecurityError::InvalidKey)
            ));
        }
        assert!(derive_channel_keys(&[1; 32], PROLOGUE).is_ok());
    }

    #[test]
    fn request_and_event_aad_are_exact() {
        assert_eq!(
            String::from_utf8(request_aad(&sample_request_aad())).unwrap(),
            "whalerein/v1/request|req-1|host-1|device-1|REQUEST_KIND_QUERY||1700000000000"
        );
        assert_eq!(
            String::from_utf8(event_aad(&sample_event_aad())).unwrap(),
            "whalerein/v1/event|host-1|device-1|chan-1|1700000000000"
        );
        assert_eq!(
            request_kind_name(RequestKind::Command),
            "REQUEST_KIND_COMMAND"
        );
        assert_eq!(
            request_kind_name(RequestKind::Handshake),
            "REQUEST_KIND_HANDSHAKE"
        );
    }

    #[test]
    fn decrypts_request_frame_and_rejects_wrong_aad() {
        let channel = channel();
        let aad = sample_request_aad();
        let frame = channel.seal_request(&aad, b"payload").unwrap();
        assert_eq!(frame.channel_id, channel.channel_id());
        assert_eq!(frame.nonce.len(), 12);
        assert_eq!(
            channel.peer_view().open_request(&aad, &frame).unwrap(),
            b"payload"
        );

        let mut other = sample_request_aad();
        other.request_id = "req-2";
        assert!(matches!(
            channel.peer_view().open_request(&other, &frame),
            Err(SecurityError::AuthenticationFailed)
        ));
        // A host response is sealed with h2c and opened by the client.
        let response = channel.peer_view().seal_request(&aad, b"response").unwrap();
        assert_eq!(channel.open_request(&aad, &response).unwrap(), b"response");
    }

    #[test]
    fn decrypts_valid_event_frame_and_rejects_tampered_ciphertext() {
        let channel = channel();
        let aad = sample_event_aad();
        let frame = channel.seal_event(&aad, b"event").unwrap();
        assert_eq!(channel.open_event(&aad, &frame).unwrap(), b"event");
        let mut tampered = frame.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(channel.open_event(&aad, &tampered).is_err());
    }

    #[test]
    fn old_channel_frame_is_rejected_without_side_effects() {
        let old = channel();
        let new = SecureChannel::from_handshake(
            &[8u8; 32],
            PROLOGUE,
            "other-channel",
            "host-1",
            "device-1",
        )
        .unwrap();
        let aad = sample_request_aad();
        let frame = old.seal_request(&aad, b"payload").unwrap();
        assert_ne!(old.channel_id(), new.channel_id());
        assert!(matches!(
            new.open_request(&aad, &frame),
            Err(SecurityError::ChannelMismatch)
        ));
    }

    #[test]
    fn invalid_nonce_length_is_rejected() {
        let channel = channel();
        let aad = sample_request_aad();
        let mut frame = channel.seal_request(&aad, b"payload").unwrap();
        frame.nonce.pop();
        assert!(matches!(
            channel.open_request(&aad, &frame),
            Err(SecurityError::InvalidNonce)
        ));
    }

    #[test]
    fn frames_use_fresh_nonces() {
        let channel = channel();
        let aad = sample_request_aad();
        let first = channel.seal_request(&aad, b"payload").unwrap();
        let second = channel.seal_request(&aad, b"payload").unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn approval_digest_matches_vector() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../proto/v1/approval_digest_vectors.json"
        ))
        .expect("shared approval digest vectors");
        let cases = vectors["cases"].as_array().expect("cases");
        let mut digest_of = std::collections::HashMap::new();
        for case in cases {
            let fields = &case["fields"];
            let digest = approval_digest(
                fields["approval_id"].as_str().unwrap(),
                fields["session_id"].as_str().unwrap(),
                fields["tool_name"].as_str().unwrap(),
                fields["call_id"].as_str().unwrap(),
                fields["reason"].as_str().unwrap(),
                fields["version"].as_i64().unwrap(),
                fields["expires_at_unix_ms"].as_i64().unwrap(),
            );
            let name = case["name"].as_str().unwrap();
            assert_eq!(
                hex::encode(digest),
                case["digest_hex"].as_str().unwrap(),
                "shared vector {name}"
            );
            digest_of.insert(name.to_owned(), digest);
        }
        for pair in vectors["must_differ"].as_array().expect("must_differ") {
            let left = pair[0].as_str().unwrap();
            let right = pair[1].as_str().unwrap();
            assert_ne!(digest_of[left], digest_of[right], "{left} vs {right}");
        }
    }

    #[test]
    fn seals_and_opens_with_raw_key_and_rejects_wrong_aad() {
        let key = [3u8; 32];
        let (nonce, ciphertext) = seal_with_key(&key, b"aad", b"data").unwrap();
        assert_eq!(
            open_with_key(&key, b"aad", &nonce, &ciphertext).unwrap(),
            b"data"
        );
        assert!(open_with_key(&key, b"other", &nonce, &ciphertext).is_err());
        assert!(matches!(
            open_with_key(&key, b"aad", &nonce[..11], &ciphertext),
            Err(SecurityError::InvalidNonce)
        ));
    }

    #[test]
    fn decodes_encoded_frame_and_rejects_malformed_bytes() {
        let frame = channel()
            .seal_request(&sample_request_aad(), b"payload")
            .unwrap();
        assert_eq!(decode_frame(&encode_frame(&frame)).unwrap(), frame);
        assert!(matches!(
            decode_frame(b"not a frame"),
            Err(SecurityError::InvalidMessage)
        ));
    }

    #[test]
    fn debug_redacts_keys() {
        let channel = channel();
        let text = format!("{channel:?}");
        assert!(text.contains(channel.channel_id()));
        assert!(!text.contains(&hex::encode(channel.keys.c2h)));
        assert!(!text.contains(&hex::encode(channel.keys.h2c)));
    }
}
