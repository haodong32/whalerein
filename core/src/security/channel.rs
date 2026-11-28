//! Secure application channel: Noise Split keys and ChaCha20-Poly1305 frames.
//!
//! Application traffic keys are the secret Noise XX Split outputs, never a
//! function of the observable handshake hash. The v2 wire domain strings below
//! are part of the contract and bind a frame to the request (or event)
//! metadata that Access relays. The retired `whalerein/v1` suite derived keys
//! from the transcript hash and used random nonces; it must not interoperate.
//!
//! Each channel direction uses one monotonic 64-bit record counter encoded in
//! the 12-byte `EncryptedFrame.nonce` as four zero bytes followed by the
//! big-endian counter. The receiver keeps an authenticated 128-record window so
//! concurrent in-flight requests may complete out of order while duplicates and
//! counters older than the window are rejected.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use prost::Message;
use whalerein_proto::{EncryptedFrame, RequestKind};

use super::SecurityError;

/// Wire domain for request AAD. Part of the v2 suite epoch.
pub const REQUEST_AAD_DOMAIN: &str = "whalerein/v2/request";
/// Wire domain for event AAD. Part of the v2 suite epoch.
pub const EVENT_AAD_DOMAIN: &str = "whalerein/v2/event";
/// Length of `EncryptedFrame.nonce`: four zero bytes plus a big-endian u64.
pub const NONCE_LEN: usize = 12;
/// Leading zero bytes that carry the fixed suite epoch inside the nonce.
pub const NONCE_COUNTER_OFFSET: usize = 4;
/// Records accepted behind the highest authenticated counter.
pub const REPLAY_WINDOW: u64 = 128;

/// Direction of an application message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Device to host: `c2h`, the first Split key.
    DeviceToHost,
    /// Host to device: `h2c`, the second Split key.
    HostToDevice,
}

/// Which endpoint this channel view represents.
///
/// The device seals requests with `c2h` and opens responses/events with `h2c`.
/// The host opens requests with `c2h` and seals responses/events with `h2c`.
/// Split keys are symmetric outputs, so only the operation mapping differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// `Host` exists only for the test/tool peer view below.
#[allow(dead_code)]
enum Role {
    Device,
    Host,
}

/// Directional application keys taken from Noise XX Split.
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

impl ChannelKeys {
    /// Map Noise Split outputs to wire directions: `k1` is `c2h`, `k2` is `h2c`.
    pub fn from_split((c2h, h2c): ([u8; 32], [u8; 32])) -> Self {
        Self { c2h, h2c }
    }
}

/// Authenticated receive window for one direction.
#[derive(Default)]
struct ReceiveWindow {
    highest: Option<u64>,
    /// Bit 0 is `highest`; bit N is `highest - N`.
    seen: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowVerdict {
    Accept,
    Replay,
    Stale,
}

impl ReceiveWindow {
    fn classify(&self, counter: u64) -> WindowVerdict {
        let Some(highest) = self.highest else {
            return WindowVerdict::Accept;
        };
        if counter > highest {
            return WindowVerdict::Accept;
        }
        let delta = highest - counter;
        if delta >= REPLAY_WINDOW {
            return WindowVerdict::Stale;
        }
        if self.seen & (1u128 << delta) != 0 {
            WindowVerdict::Replay
        } else {
            WindowVerdict::Accept
        }
    }

    /// Commit an authenticated record. Only call after successful decryption.
    fn record(&mut self, counter: u64) {
        match self.highest {
            None => {
                self.highest = Some(counter);
                self.seen = 1;
            }
            Some(highest) if counter > highest => {
                let shift = counter - highest;
                self.seen = if shift >= 128 {
                    1
                } else {
                    (self.seen << shift) | 1
                };
                self.highest = Some(counter);
            }
            Some(highest) => {
                let delta = highest - counter;
                if delta < 128 {
                    self.seen |= 1u128 << delta;
                }
            }
        }
    }
}

/// Send counters and receive windows for both key directions.
///
/// Clones share this state so two tasks cannot reuse a send counter and a
/// delayed frame can never replay on a copied handle.
#[derive(Default)]
struct ChannelState {
    send_c2h: AtomicU64,
    send_h2c: AtomicU64,
    recv_c2h: Mutex<ReceiveWindow>,
    recv_h2c: Mutex<ReceiveWindow>,
}

impl ChannelState {
    fn send(&self, direction: Direction) -> &AtomicU64 {
        match direction {
            Direction::DeviceToHost => &self.send_c2h,
            Direction::HostToDevice => &self.send_h2c,
        }
    }

    fn receive(&self, direction: Direction) -> &Mutex<ReceiveWindow> {
        match direction {
            Direction::DeviceToHost => &self.recv_c2h,
            Direction::HostToDevice => &self.recv_h2c,
        }
    }
}

/// Unit-test-only channel-state handoff.
///
/// The in-process `FakeHost` fixture and the `Core` it serves are two
/// endpoints with independent send counters in production. A unit test that
/// manufactures Host frames through the Device channel therefore needs both
/// views to share one directional counter. The fixture publishes its state
/// under the (random, per-handshake) channel id and the Device adopts it;
/// production builds never compile this module.
#[cfg(test)]
mod test_state_registry {
    use super::ChannelState;
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};

    static SHARED: LazyLock<Mutex<HashMap<String, Arc<ChannelState>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(super) fn publish(channel_id: &str, state: &Arc<ChannelState>) {
        let mut shared = SHARED.lock().unwrap_or_else(|error| error.into_inner());
        if shared.len() >= 512 {
            shared.clear();
        }
        shared.insert(channel_id.to_owned(), Arc::clone(state));
    }

    pub(super) fn adopt(channel_id: &str) -> Option<Arc<ChannelState>> {
        SHARED
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(channel_id)
    }
}

/// An established secure channel. A reconnect always replaces this value.
pub struct SecureChannel {
    role: Role,
    host_id: String,
    device_id: String,
    channel_id: String,
    keys: ChannelKeys,
    state: Arc<ChannelState>,
}

impl Clone for SecureChannel {
    fn clone(&self) -> Self {
        Self {
            role: self.role,
            host_id: self.host_id.clone(),
            device_id: self.device_id.clone(),
            channel_id: self.channel_id.clone(),
            keys: self.keys.clone(),
            state: Arc::clone(&self.state),
        }
    }
}

impl std::fmt::Debug for SecureChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecureChannel")
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

impl SecureChannel {
    /// Build the device view of a channel from Noise Split outputs.
    ///
    /// The key material comes only from the secret handshake Split. The
    /// transcript hash is deliberately not an input; it stays available for
    /// the App-session proof and channel binding.
    pub fn from_split(
        split: ([u8; 32], [u8; 32]),
        channel_id: impl Into<String>,
        host_id: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Result<Self, SecurityError> {
        let channel_id = channel_id.into();
        if channel_id.is_empty() {
            return Err(SecurityError::InvalidKey);
        }
        #[cfg(test)]
        let state = test_state_registry::adopt(&channel_id).unwrap_or_default();
        #[cfg(not(test))]
        let state = Arc::new(ChannelState::default());
        Ok(Self {
            role: Role::Device,
            host_id: host_id.into(),
            device_id: device_id.into(),
            channel_id,
            keys: ChannelKeys::from_split(split),
            state,
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

    /// Non-reversible fingerprint of the directional key material.
    ///
    /// Interop diagnostics compare this value; it never reveals a key and is
    /// the only key-derived value any diagnostic surface may emit.
    #[cfg(any(test, feature = "protocol-testing"))]
    pub fn key_fingerprint(&self) -> String {
        let mut material = Vec::with_capacity(64);
        material.extend_from_slice(&self.keys.c2h);
        material.extend_from_slice(&self.keys.h2c);
        crate::util::sha256_hex(&material)
    }

    pub fn seal_request(
        &self,
        aad: &RequestAad<'_>,
        plaintext: &[u8],
    ) -> Result<EncryptedFrame, SecurityError> {
        self.seal(self.send_direction(), &request_aad(aad), plaintext)
    }

    pub fn open_request(
        &self,
        aad: &RequestAad<'_>,
        frame: &EncryptedFrame,
    ) -> Result<Vec<u8>, SecurityError> {
        self.open(self.receive_direction(), frame, &request_aad(aad))
    }

    /// Seal a host-to-device event. Test fixtures use this to produce the
    /// frame the host would send; the host itself seals with `h2c`.
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

    /// Raw seal for the interop tool: same counter rules, caller-supplied AAD.
    #[cfg(any(test, feature = "protocol-testing"))]
    pub fn seal_raw(&self, aad: &[u8], plaintext: &[u8]) -> Result<EncryptedFrame, SecurityError> {
        self.seal(self.send_direction(), aad, plaintext)
    }

    /// Raw open for the interop tool: same replay window, caller-supplied AAD.
    #[cfg(any(test, feature = "protocol-testing"))]
    pub fn open_raw(&self, aad: &[u8], frame: &EncryptedFrame) -> Result<Vec<u8>, SecurityError> {
        self.open(self.receive_direction(), frame, aad)
    }

    /// Raw open from wire parts, for peers that do not link the proto crate.
    #[cfg(any(test, feature = "protocol-testing"))]
    pub fn open_raw_parts(
        &self,
        aad: &[u8],
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, SecurityError> {
        let frame = EncryptedFrame {
            channel_id: self.channel_id.clone(),
            nonce: nonce.to_vec(),
            ciphertext: ciphertext.to_vec(),
        };
        self.open(self.receive_direction(), &frame, aad)
    }

    /// Fixture hook: let the matching Device channel adopt this view's state.
    #[cfg(test)]
    pub fn publish_state_for_test(&self) {
        test_state_registry::publish(&self.channel_id, &self.state);
    }

    /// Host-side view of the same channel. Test fixtures and the interop tool
    /// use this to act as the other endpoint; it shares counters and replay
    /// state so frames produced through either view stay unique.
    #[cfg(any(test, feature = "test-support", feature = "protocol-testing"))]
    pub fn peer_view(&self) -> Self {
        Self {
            role: Role::Host,
            host_id: self.host_id.clone(),
            device_id: self.device_id.clone(),
            channel_id: self.channel_id.clone(),
            keys: self.keys.clone(),
            state: Arc::clone(&self.state),
        }
    }

    fn send_direction(&self) -> Direction {
        match self.role {
            Role::Device => Direction::DeviceToHost,
            Role::Host => Direction::HostToDevice,
        }
    }

    fn receive_direction(&self) -> Direction {
        match self.role {
            Role::Device => Direction::HostToDevice,
            Role::Host => Direction::DeviceToHost,
        }
    }

    fn next_counter(&self, direction: Direction) -> Result<u64, SecurityError> {
        let counter = self.state.send(direction);
        let mut current = counter.load(Ordering::SeqCst);
        loop {
            if current == u64::MAX {
                return Err(SecurityError::NonceExhausted);
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(current),
                Err(actual) => current = actual,
            }
        }
    }

    #[cfg(test)]
    fn force_send_counter(&self, direction: Direction, counter: u64) {
        self.state.send(direction).store(counter, Ordering::SeqCst);
    }

    fn seal(
        &self,
        direction: Direction,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<EncryptedFrame, SecurityError> {
        let counter = self.next_counter(direction)?;
        let key = match direction {
            Direction::DeviceToHost => &self.keys.c2h,
            Direction::HostToDevice => &self.keys.h2c,
        };
        let cipher =
            ChaCha20Poly1305::new_from_slice(key).map_err(|_| SecurityError::InvalidKey)?;
        let nonce_bytes = encode_nonce(counter);
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
            nonce: nonce_bytes.to_vec(),
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
        let counter = decode_nonce(&frame.nonce)?;
        let key = match direction {
            Direction::DeviceToHost => &self.keys.c2h,
            Direction::HostToDevice => &self.keys.h2c,
        };
        let cipher =
            ChaCha20Poly1305::new_from_slice(key).map_err(|_| SecurityError::InvalidKey)?;
        let mut window = self
            .state
            .receive(direction)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match window.classify(counter) {
            WindowVerdict::Replay => return Err(SecurityError::ReplayDetected),
            WindowVerdict::Stale => return Err(SecurityError::StaleCounter),
            WindowVerdict::Accept => {}
        }
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&frame.nonce),
                Payload {
                    msg: &frame.ciphertext,
                    aad,
                },
            )
            .map_err(|_| SecurityError::AuthenticationFailed)?;
        // Authentication succeeded: only now does the counter enter the
        // window. A tampered or wrong-AAD record cannot advance it.
        window.record(counter);
        Ok(plaintext)
    }
}

/// Encode one application record counter as four zero bytes plus big-endian u64.
pub fn encode_nonce(counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[NONCE_COUNTER_OFFSET..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

/// Decode a record nonce, rejecting anything outside the v2 counter encoding.
///
/// The leading zero bytes make the epoch explicit: a retired random 12-byte
/// nonce is rejected as invalid rather than being interpreted as a counter.
pub fn decode_nonce(nonce: &[u8]) -> Result<u64, SecurityError> {
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| SecurityError::InvalidNonce)?;
    if nonce[..NONCE_COUNTER_OFFSET] != [0u8; NONCE_COUNTER_OFFSET] {
        return Err(SecurityError::InvalidNonce);
    }
    let counter: [u8; 8] = nonce[NONCE_COUNTER_OFFSET..]
        .try_into()
        .map_err(|_| SecurityError::InvalidNonce)?;
    Ok(u64::from_be_bytes(counter))
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
        RequestKind::AccessReady => "REQUEST_KIND_ACCESS_READY",
        RequestKind::Authority => "REQUEST_KIND_AUTHORITY",
        RequestKind::SessionProof => "REQUEST_KIND_SESSION_PROOF",
        RequestKind::SelfStatus => "REQUEST_KIND_SELF_STATUS",
    }
}

pub fn request_aad(aad: &RequestAad<'_>) -> Vec<u8> {
    format!(
        "{REQUEST_AAD_DOMAIN}|{}|{}|{}|{}|{}|{}",
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
        "{EVENT_AAD_DOMAIN}|{}|{}|{}|{}",
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
///
/// `review_availability` is the canonical lowercase token
/// (`complete` / `incomplete` / `unavailable`). The bounded review facts are
/// part of the digest so a changed target or impact requires fresh review.
#[allow(clippy::too_many_arguments)]
pub fn approval_digest(
    approval_id: &str,
    session_id: &str,
    tool_name: &str,
    call_id: &str,
    reason: &str,
    review_action: &str,
    review_target: &str,
    review_impact: &str,
    review_availability: &str,
    version: i64,
    expires_at_unix_ms: i64,
) -> [u8; 32] {
    let canonical = format!(
        "whalerein/v2/approval|{}|{}|{}|{}|{}|{}|{}|{}|{}|{version}|{expires_at_unix_ms}",
        escape_approval_field(approval_id),
        escape_approval_field(session_id),
        escape_approval_field(tool_name),
        escape_approval_field(call_id),
        escape_approval_field(reason),
        escape_approval_field(review_action),
        escape_approval_field(review_target),
        escape_approval_field(review_impact),
        escape_approval_field(review_availability),
    );
    crate::util::sha256(canonical.as_bytes())
}

/// Seal arbitrary bytes with a raw application key.
///
/// Used by interop tooling and by Core's local draft store; both must bind the
/// ciphertext to caller-supplied associated data. These are not channel
/// records and therefore keep an independently random nonce.
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
    use hkdf::Hkdf;
    use sha2::Sha256;

    const PROLOGUE: &str = "whalerein/v2/channel|host-1|device-1|nego-1";
    const CHANNEL_ID: &str = "e372075fe44f25a0f0e06bb6604889e9";

    fn channel() -> SecureChannel {
        SecureChannel::from_split(([7u8; 32], [8u8; 32]), CHANNEL_ID, "host-1", "device-1").unwrap()
    }

    /// The retired v1 schedule: HKDF over the observable transcript hash. It is
    /// implemented here only to prove it cannot open v2 records.
    fn retired_keys(handshake_hash: &[u8], prologue: &str) -> ChannelKeys {
        let hkdf = Hkdf::<Sha256>::new(Some(prologue.as_bytes()), handshake_hash);
        let mut okm = [0u8; 64];
        hkdf.expand(b"whalerein/v1/channel-keys", &mut okm).unwrap();
        ChannelKeys {
            c2h: okm[..32].try_into().unwrap(),
            h2c: okm[32..].try_into().unwrap(),
        }
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
    fn split_outputs_map_to_wire_directions() {
        let keys = ChannelKeys::from_split(([1u8; 32], [2u8; 32]));
        assert_eq!(keys.c2h, [1u8; 32]);
        assert_eq!(keys.h2c, [2u8; 32]);
    }

    #[test]
    fn key_fingerprint_is_stable_across_views_and_never_reveals_keys() {
        let device = channel();
        let host = device.peer_view();
        assert_eq!(device.key_fingerprint(), host.key_fingerprint());
        assert_eq!(device.key_fingerprint().len(), 64);
        assert!(!device
            .key_fingerprint()
            .contains(&hex::encode(device.keys.c2h)));
        let other =
            SecureChannel::from_split(([8u8; 32], [9u8; 32]), CHANNEL_ID, "host-1", "device-1")
                .unwrap();
        assert_ne!(device.key_fingerprint(), other.key_fingerprint());
    }

    #[test]
    fn nonce_is_four_zero_bytes_then_big_endian_counter() {
        assert_eq!(hex::encode(encode_nonce(0)), "000000000000000000000000");
        assert_eq!(encode_nonce(1)[11], 1);
        assert_eq!(
            hex::encode(encode_nonce(0x0102_0304_0506_0708)),
            "000000000102030405060708"
        );
        for counter in [0u64, 1, 255, 256, u64::MAX - 1, u64::MAX] {
            assert_eq!(decode_nonce(&encode_nonce(counter)).unwrap(), counter);
        }
        // A retired random nonce with a nonzero epoch prefix is invalid.
        let mut random = [0xab; NONCE_LEN];
        assert!(matches!(
            decode_nonce(&random),
            Err(SecurityError::InvalidNonce)
        ));
        random[..4].copy_from_slice(&[0, 0, 0, 0]);
        assert!(decode_nonce(&random).is_ok());
        assert!(matches!(
            decode_nonce(&[0u8; 11]),
            Err(SecurityError::InvalidNonce)
        ));
    }

    #[test]
    fn request_and_event_aad_are_exact_v2_domains() {
        assert_eq!(
            String::from_utf8(request_aad(&sample_request_aad())).unwrap(),
            "whalerein/v2/request|req-1|host-1|device-1|REQUEST_KIND_QUERY||1700000000000"
        );
        assert_eq!(
            String::from_utf8(event_aad(&sample_event_aad())).unwrap(),
            "whalerein/v2/event|host-1|device-1|chan-1|1700000000000"
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

        // A new record under a different AAD fails authentication; a fresh
        // counter keeps this distinct from replay rejection.
        let mut other = sample_request_aad();
        other.request_id = "req-2";
        let mismatched = channel.seal_request(&other, b"payload").unwrap();
        assert!(matches!(
            channel.peer_view().open_request(&aad, &mismatched),
            Err(SecurityError::AuthenticationFailed)
        ));
        // A host response is sealed with h2c and opened by the client.
        let response = channel.peer_view().seal_request(&aad, b"response").unwrap();
        assert_eq!(channel.open_request(&aad, &response).unwrap(), b"response");
    }

    #[test]
    fn decrypts_valid_event_frame_and_rejects_tampered_ciphertext() {
        let channel = channel();
        let peer = channel.peer_view();
        let aad = sample_event_aad();
        let frame = peer.seal_event(&aad, b"event").unwrap();
        let mut tampered = frame.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(matches!(
            channel.open_event(&aad, &tampered),
            Err(SecurityError::AuthenticationFailed)
        ));
        // A failed authentication does not advance the window, so the intact
        // record is still accepted.
        assert_eq!(channel.open_event(&aad, &frame).unwrap(), b"event");
    }

    #[test]
    fn out_of_order_records_within_the_window_are_accepted() {
        let channel = channel();
        let peer = channel.peer_view();
        let aad = sample_event_aad();
        let first = peer.seal_event(&aad, b"one").unwrap();
        let second = peer.seal_event(&aad, b"two").unwrap();
        let third = peer.seal_event(&aad, b"three").unwrap();
        assert_eq!(channel.open_event(&aad, &third).unwrap(), b"three");
        assert_eq!(channel.open_event(&aad, &first).unwrap(), b"one");
        assert_eq!(channel.open_event(&aad, &second).unwrap(), b"two");
    }

    #[test]
    fn duplicate_and_stale_counters_are_rejected() {
        let channel = channel();
        let peer = channel.peer_view();
        let aad = sample_event_aad();
        let first = peer.seal_event(&aad, b"one").unwrap();
        assert_eq!(channel.open_event(&aad, &first).unwrap(), b"one");
        assert!(matches!(
            channel.open_event(&aad, &first),
            Err(SecurityError::ReplayDetected)
        ));

        let mut frames = Vec::new();
        for index in 0..REPLAY_WINDOW + 2 {
            frames.push(peer.seal_event(&aad, &index.to_be_bytes()).unwrap());
        }
        let highest = frames.last().unwrap();
        channel.open_event(&aad, highest).unwrap();
        assert!(matches!(
            channel.open_event(&aad, &frames[0]),
            Err(SecurityError::StaleCounter)
        ));
    }

    #[test]
    fn counter_exhaustion_fails_closed() {
        let channel = channel();
        let aad = sample_request_aad();
        channel.force_send_counter(Direction::DeviceToHost, u64::MAX - 1);
        let last = channel.seal_request(&aad, b"last").unwrap();
        assert_eq!(decode_nonce(&last.nonce).unwrap(), u64::MAX - 1);
        assert!(matches!(
            channel.seal_request(&aad, b"beyond"),
            Err(SecurityError::NonceExhausted)
        ));
        // Exhaustion never reuses a counter; the next attempt still fails.
        assert!(matches!(
            channel.seal_request(&aad, b"beyond"),
            Err(SecurityError::NonceExhausted)
        ));
    }

    #[test]
    fn transcript_hash_alone_cannot_open_an_application_frame() {
        let split = ([9u8; 32], [10u8; 32]);
        let channel = SecureChannel::from_split(split, CHANNEL_ID, "host-1", "device-1").unwrap();
        let observer_keys = retired_keys(&[9u8; 32], PROLOGUE);
        assert_ne!(observer_keys.c2h, split.0);
        assert_ne!(observer_keys.h2c, split.1);
        let observer = SecureChannel::from_split(
            (observer_keys.c2h, observer_keys.h2c),
            CHANNEL_ID,
            "host-1",
            "device-1",
        )
        .unwrap();

        let aad = sample_request_aad();
        let frame = channel.seal_request(&aad, b"secret").unwrap();
        assert!(matches!(
            observer.open_request(&aad, &frame),
            Err(SecurityError::AuthenticationFailed)
        ));
    }

    #[test]
    fn old_channel_frame_is_rejected_without_side_effects() {
        let old = channel();
        let new = SecureChannel::from_split(
            ([8u8; 32], [9u8; 32]),
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
    fn raw_interop_helpers_use_the_same_counter_and_replay_window() {
        let device = channel();
        let host = device.peer_view();
        let frame = device.seal_raw(b"fixture-aad", b"raw").unwrap();
        assert_eq!(host.open_raw(b"fixture-aad", &frame).unwrap(), b"raw");
        let reply = host.seal_raw(b"fixture-aad", b"reply").unwrap();
        assert_eq!(
            device
                .open_raw_parts(b"fixture-aad", &reply.nonce, &reply.ciphertext)
                .unwrap(),
            b"reply"
        );
        assert!(matches!(
            device.open_raw_parts(b"fixture-aad", &reply.nonce, &reply.ciphertext),
            Err(SecurityError::ReplayDetected)
        ));
    }

    #[test]
    fn frames_use_monotonic_per_direction_counters() {
        let channel = channel();
        let aad = sample_request_aad();
        let first = channel.seal_request(&aad, b"payload").unwrap();
        let second = channel.seal_request(&aad, b"payload").unwrap();
        assert_eq!(decode_nonce(&first.nonce).unwrap(), 0);
        assert_eq!(decode_nonce(&second.nonce).unwrap(), 1);
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.ciphertext, second.ciphertext);
        // The host direction has its own counter sequence.
        let event = channel
            .peer_view()
            .seal_event(&sample_event_aad(), b"event")
            .unwrap();
        assert_eq!(decode_nonce(&event.nonce).unwrap(), 0);
    }

    #[test]
    fn clones_share_counters_so_they_cannot_reuse_a_nonce() {
        let channel = channel();
        let clone = channel.clone();
        let aad = sample_request_aad();
        let first = channel.seal_request(&aad, b"one").unwrap();
        let second = clone.seal_request(&aad, b"two").unwrap();
        assert_eq!(decode_nonce(&first.nonce).unwrap(), 0);
        assert_eq!(decode_nonce(&second.nonce).unwrap(), 1);
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
                fields["review_action"].as_str().unwrap(),
                fields["review_target"].as_str().unwrap(),
                fields["review_impact"].as_str().unwrap(),
                fields["review_availability"].as_str().unwrap(),
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
        let text = format!("{channel:?}{:?}", channel.keys);
        assert!(text.contains(channel.channel_id()));
        assert!(!text.contains(&hex::encode(channel.keys.c2h)));
        assert!(!text.contains(&hex::encode(channel.keys.h2c)));
        assert!(!text.contains("c2h"));
    }
}
