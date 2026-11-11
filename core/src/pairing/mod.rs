//! Pairing: `whalerein://pair` URI parsing and the Noise XX state machine.
//!
//! The fingerprint shown to the user is always computed from the Noise remote
//! static key. Values reported by Access are deliberately ignored.

use prost::Message;
use whalerein_proto::{
    Capability, CapabilitySet, PairingClaimRequest, PairingClaimResponse, PairingFinish,
    PairingHello, PairingHelloReply, PairingPhase,
};

use crate::security::{fingerprint, pairing_prologue, NoiseSession, SecurityError, StaticIdentity};
use crate::util::{format_fingerprint, normalize_fingerprint, random_id, sha256_hex};

pub const PROTOCOL_VERSION: u32 = whalerein_proto::PROTOCOL_VERSION;
pub const PAIRING_URI_VERSION: &str = "1";

/// Parsed pairing invitation. The token is retained for FINISH and is never
/// rendered by `Debug`.
#[derive(Clone)]
pub struct PairingUri {
    pub access_url: url::Url,
    /// Optional `mqtt=` override from the invitation.
    pub mqtt_url: Option<String>,
    pub host_id: String,
    pub token: String,
    pub token_hash_hex: String,
}

impl std::fmt::Debug for PairingUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingUri")
            .field("access_url", &self.access_url.as_str())
            .field("mqtt_url", &self.mqtt_url)
            .field("host_id", &self.host_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairingError {
    #[error("invalid pairing URI")]
    InvalidUri,
    #[error("unsupported pairing URI version")]
    UnsupportedVersion,
    #[error("pairing is in the wrong state")]
    InvalidState,
    #[error("pairing was cancelled")]
    Cancelled,
    #[error("host fingerprint does not match")]
    FingerprintMismatch,
    #[error("protocol version incompatible")]
    ProtocolIncompatible,
    #[error("pairing response does not match this claim")]
    ResponseMismatch,
    #[error("noise handshake failed")]
    Noise,
    #[error("device key reported by Access does not match")]
    DeviceKeyMismatch,
}

impl From<SecurityError> for PairingError {
    fn from(_: SecurityError) -> Self {
        PairingError::Noise
    }
}

/// Parse `whalerein://pair?v=1&access=<url>&host=<id>&token=<token>`.
pub fn parse_pairing_uri(value: &str) -> Result<PairingUri, PairingError> {
    let url = url::Url::parse(value).map_err(|_| PairingError::InvalidUri)?;
    if url.scheme() != "whalerein" || url.host_str() != Some("pair") {
        return Err(PairingError::InvalidUri);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PairingError::InvalidUri);
    }

    let mut version: Option<String> = None;
    let mut access: Option<String> = None;
    let mut host: Option<String> = None;
    let mut token: Option<String> = None;
    let mut mqtt: Option<String> = None;
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "v" => &mut version,
            "access" => &mut access,
            "host" => &mut host,
            "token" => &mut token,
            "mqtt" => &mut mqtt,
            _ => continue,
        };
        if slot.is_some() {
            return Err(PairingError::InvalidUri);
        }
        *slot = Some(value.into_owned());
    }

    if version.as_deref() != Some(PAIRING_URI_VERSION) {
        return Err(PairingError::UnsupportedVersion);
    }
    let access = access.ok_or(PairingError::InvalidUri)?;
    let host_id = host.ok_or(PairingError::InvalidUri)?;
    let token = token.ok_or(PairingError::InvalidUri)?;
    if host_id.is_empty() || token.is_empty() {
        return Err(PairingError::InvalidUri);
    }

    let mut access_url = url::Url::parse(&access).map_err(|_| PairingError::InvalidUri)?;
    if !matches!(access_url.scheme(), "https" | "http") {
        return Err(PairingError::InvalidUri);
    }
    if access_url.host_str().is_none()
        || !access_url.username().is_empty()
        || access_url.password().is_some()
        || access_url.query().is_some()
        || access_url.fragment().is_some()
    {
        return Err(PairingError::InvalidUri);
    }
    access_url.set_fragment(None);
    let mqtt_url = match mqtt {
        Some(value) => {
            crate::access::MqttSettings::parse(&value).map_err(|_| PairingError::InvalidUri)?;
            Some(value)
        }
        None => None,
    };
    let token_hash_hex = sha256_hex(token.as_bytes());

    Ok(PairingUri {
        access_url,
        mqtt_url,
        host_id,
        token,
        token_hash_hex,
    })
}

pub fn default_capabilities() -> CapabilitySet {
    CapabilitySet {
        capabilities: vec![
            Capability::Sessions as i32,
            Capability::Events as i32,
            Capability::Messages as i32,
            Capability::Approvals as i32,
            Capability::Interrupt as i32,
            Capability::Sync as i32,
            Capability::Snapshot as i32,
        ],
        protocol_version: PROTOCOL_VERSION,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingState {
    AwaitingHello,
    AwaitingConfirmation,
    AwaitingFinish,
    Complete,
    Cancelled,
    Failed,
}

/// What the user is asked to confirm after HELLO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingConfirmation {
    pub host_id: String,
    pub access_url: String,
    pub fingerprint: String,
    pub fingerprint_display: String,
}

/// User decision. `expected_fingerprint` is compared against the Noise key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingDecision {
    pub approve: bool,
    pub expected_fingerprint: Option<String>,
}

impl PairingDecision {
    pub fn approve() -> Self {
        Self {
            approve: true,
            expected_fingerprint: None,
        }
    }

    pub fn reject() -> Self {
        Self {
            approve: false,
            expected_fingerprint: None,
        }
    }

    pub fn expect(fingerprint: impl Into<String>) -> Self {
        Self {
            approve: true,
            expected_fingerprint: Some(fingerprint.into()),
        }
    }
}

/// One-time material returned after FINISH. The token is stored immediately by
/// the caller and never logged or persisted anywhere else.
pub struct PairingCompletion {
    pub host_id: String,
    pub device_id: String,
    pub host_fingerprint: String,
    pub device_token: String,
}

impl std::fmt::Debug for PairingCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCompletion")
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("host_fingerprint", &self.host_fingerprint)
            .field("device_token", &"<redacted>")
            .finish()
    }
}

/// Client side of one pairing claim.
pub struct PairingInitiator {
    session: NoiseSession,
    host_id: String,
    device_id: String,
    claim_id: String,
    token: String,
    access_url: String,
    protocol_version: u32,
    fingerprint: Option<String>,
    public_key: [u8; 32],
    state: PairingState,
}

impl std::fmt::Debug for PairingInitiator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingInitiator")
            .field("host_id", &self.host_id)
            .field("device_id", &self.device_id)
            .field("claim_id", &self.claim_id)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

const MAX_DISPLAY_NAME_CHARS: usize = 64;

/// Keep an allowlisted OS token. Anything else becomes an empty label.
fn sanitize_os(value: &str) -> String {
    let text = value.trim().to_ascii_lowercase();
    match text.as_str() {
        "linux" | "macos" | "ios" | "freebsd" | "dragonfly" | "netbsd" | "openbsd" | "solaris"
        | "android" | "windows" => text,
        _ => String::new(),
    }
}

/// Trim, drop C0 controls and DEL, then keep at most 64 code points.
fn sanitize_display_name(value: &str) -> String {
    let stripped: String = value
        .chars()
        .filter(|ch| {
            let code = u32::from(*ch);
            code > 0x1F && code != 0x7F
        })
        .collect();
    stripped
        .trim()
        .chars()
        .take(MAX_DISPLAY_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl PairingInitiator {
    /// Start the flow and produce the HELLO claim.
    #[cfg(test)]
    pub fn start(
        uri: &PairingUri,
        device_id: impl Into<String>,
        identity: &StaticIdentity,
        capabilities: CapabilitySet,
    ) -> Result<(Self, PairingClaimRequest), PairingError> {
        Self::start_named(uri, device_id, identity, capabilities, "")
    }

    /// Start the flow and carry a sanitized display name inside HELLO.
    pub fn start_named(
        uri: &PairingUri,
        device_id: impl Into<String>,
        identity: &StaticIdentity,
        capabilities: CapabilitySet,
        display_name: impl AsRef<str>,
    ) -> Result<(Self, PairingClaimRequest), PairingError> {
        let device_id = device_id.into();
        let claim_id = random_id(16);
        let prologue = pairing_prologue(&uri.host_id, &uri.token_hash_hex, &claim_id);
        let mut session = NoiseSession::initiator(identity, &prologue)?;

        let hello = PairingHello {
            device_id: device_id.clone(),
            host_id: uri.host_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Some(capabilities),
            device_nonce: random_id(16),
            display_name: sanitize_display_name(display_name.as_ref()),
            os: sanitize_os(std::env::consts::OS),
        };
        let noise_message = session.write(&hello.encode_to_vec())?;

        let request = PairingClaimRequest {
            token: uri.token.clone(),
            claim_id: claim_id.clone(),
            phase: PairingPhase::Hello as i32,
            noise_message,
            device_id: device_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            confirmed: false,
        };

        Ok((
            Self {
                session,
                host_id: uri.host_id.clone(),
                device_id,
                claim_id,
                token: uri.token.clone(),
                access_url: uri.access_url.to_string(),
                protocol_version: PROTOCOL_VERSION,
                fingerprint: None,
                public_key: *identity.public_key(),
                state: PairingState::AwaitingHello,
            },
            request,
        ))
    }

    #[cfg(test)]
    pub fn state(&self) -> PairingState {
        self.state
    }

    #[cfg(test)]
    pub fn claim_id(&self) -> &str {
        &self.claim_id
    }

    /// Consume the HELLO response, complete the XX read of msg2 and compute the
    /// fingerprint from the Noise remote static key.
    pub fn accept_hello(
        &mut self,
        response: &PairingClaimResponse,
    ) -> Result<PairingConfirmation, PairingError> {
        if self.state != PairingState::AwaitingHello {
            return Err(PairingError::InvalidState);
        }
        if !response.claim_id.is_empty() && response.claim_id != self.claim_id {
            self.state = PairingState::Failed;
            return Err(PairingError::ResponseMismatch);
        }
        if !response.host_id.is_empty() && response.host_id != self.host_id {
            self.state = PairingState::Failed;
            return Err(PairingError::ResponseMismatch);
        }
        if response.phase != PairingPhase::Unspecified as i32
            && response.phase != PairingPhase::Hello as i32
        {
            self.state = PairingState::Failed;
            return Err(PairingError::ResponseMismatch);
        }
        if response.noise_message.is_empty() {
            self.state = PairingState::Failed;
            return Err(PairingError::ResponseMismatch);
        }

        let payload = self.session.read(&response.noise_message)?;
        let reply = PairingHelloReply::decode_payload(&payload).ok_or(PairingError::Noise)?;
        if reply.protocol_version != PROTOCOL_VERSION {
            self.state = PairingState::Failed;
            return Err(PairingError::ProtocolIncompatible);
        }
        if !reply.host_id.is_empty() && reply.host_id != self.host_id {
            self.state = PairingState::Failed;
            return Err(PairingError::ResponseMismatch);
        }
        if !reply.device_id.is_empty() && reply.device_id != self.device_id {
            self.state = PairingState::Failed;
            return Err(PairingError::ResponseMismatch);
        }

        // The remote static key comes from Noise, never from Access.
        let remote = self.session.remote_static().ok_or(PairingError::Noise)?;
        let fingerprint = fingerprint(&remote);
        self.fingerprint = Some(fingerprint.clone());
        self.access_url = self.access_url.clone();
        self.state = PairingState::AwaitingConfirmation;

        Ok(PairingConfirmation {
            host_id: self.host_id.clone(),
            access_url: self.access_url.clone(),
            fingerprint_display: format_fingerprint(&fingerprint),
            fingerprint,
        })
    }

    /// Apply the user decision. A rejection or a fingerprint mismatch never
    /// produces msg3.
    pub fn confirm(&mut self, decision: &PairingDecision) -> Result<(), PairingError> {
        if self.state != PairingState::AwaitingConfirmation {
            return Err(PairingError::InvalidState);
        }
        if !decision.approve {
            self.state = PairingState::Cancelled;
            return Err(PairingError::Cancelled);
        }
        if let Some(expected) = &decision.expected_fingerprint {
            let expected = normalize_fingerprint(expected);
            if expected != self.fingerprint.clone().unwrap_or_default() {
                self.state = PairingState::Failed;
                return Err(PairingError::FingerprintMismatch);
            }
        }
        Ok(())
    }

    /// Produce the FINISH claim. Only legal after a successful confirmation.
    pub fn write_finish(&mut self) -> Result<PairingClaimRequest, PairingError> {
        if self.state != PairingState::AwaitingConfirmation {
            return Err(PairingError::InvalidState);
        }
        let host_fingerprint = self.fingerprint.clone().ok_or(PairingError::InvalidState)?;
        let finish = PairingFinish {
            device_id: self.device_id.clone(),
            host_id: self.host_id.clone(),
            host_fingerprint,
        };
        let noise_message = self.session.write(&finish.encode_to_vec())?;
        self.state = PairingState::AwaitingFinish;
        Ok(PairingClaimRequest {
            token: self.token.clone(),
            claim_id: self.claim_id.clone(),
            phase: PairingPhase::Finish as i32,
            noise_message,
            device_id: self.device_id.clone(),
            protocol_version: self.protocol_version,
            confirmed: true,
        })
    }

    /// Consume the FINISH response and return the one-time device token.
    pub fn complete(
        &mut self,
        response: &PairingClaimResponse,
    ) -> Result<PairingCompletion, PairingError> {
        if self.state != PairingState::AwaitingFinish {
            return Err(PairingError::InvalidState);
        }
        if !response.claim_id.is_empty() && response.claim_id != self.claim_id {
            return Err(PairingError::ResponseMismatch);
        }
        if !response.host_id.is_empty() && response.host_id != self.host_id {
            return Err(PairingError::ResponseMismatch);
        }
        if !response.device_id.is_empty() && response.device_id != self.device_id {
            return Err(PairingError::ResponseMismatch);
        }
        if response.protocol_version != 0 && response.protocol_version != PROTOCOL_VERSION {
            return Err(PairingError::ProtocolIncompatible);
        }
        if !response.device_public_key.is_empty() && response.device_public_key != self.public_key {
            return Err(PairingError::DeviceKeyMismatch);
        }
        if response.device_token.is_empty() {
            return Err(PairingError::ResponseMismatch);
        }
        let device_token = response.device_token.clone();
        self.state = PairingState::Complete;
        Ok(PairingCompletion {
            host_id: self.host_id.clone(),
            device_id: self.device_id.clone(),
            host_fingerprint: self.fingerprint.clone().unwrap_or_default(),
            device_token,
        })
    }

    /// Abort the flow. No further message can be produced from this state.
    pub fn cancel(&mut self) {
        if !matches!(
            self.state,
            PairingState::Complete | PairingState::Cancelled | PairingState::Failed
        ) {
            self.state = PairingState::Cancelled;
        }
    }
}

trait DecodePayload: Sized {
    fn decode_payload(bytes: &[u8]) -> Option<Self>;
}

impl DecodePayload for PairingHelloReply {
    fn decode_payload(bytes: &[u8]) -> Option<Self> {
        Self::decode(bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{pairing_prologue, NoiseSession};

    const ACCESS: &str = "https://access.example.test";
    const TOKEN: &str = "one-time-token";

    fn uri() -> PairingUri {
        parse_pairing_uri(&format!(
            "whalerein://pair?v=1&access={}&host=host-1&token={TOKEN}",
            urlencoding(ACCESS)
        ))
        .unwrap()
    }

    fn urlencoding(value: &str) -> String {
        url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
    }

    struct TestHost {
        session: NoiseSession,
        identity: StaticIdentity,
    }

    impl TestHost {
        fn new(uri: &PairingUri, claim_id: &str) -> Self {
            let identity = StaticIdentity::generate();
            let prologue = pairing_prologue(&uri.host_id, &uri.token_hash_hex, claim_id);
            Self {
                session: NoiseSession::responder(&identity, &prologue).unwrap(),
                identity,
            }
        }

        fn read_hello(&mut self, request: &PairingClaimRequest) -> PairingHello {
            let payload = self.session.read(&request.noise_message).unwrap();
            PairingHello::decode_payload(&payload).unwrap()
        }

        fn hello_response(
            &mut self,
            request: &PairingClaimRequest,
            lie_about_fingerprint: bool,
        ) -> PairingClaimResponse {
            let hello = self.read_hello(request);
            assert_eq!(hello.device_id, request.device_id);
            let reply = PairingHelloReply {
                host_id: "host-1".into(),
                device_id: request.device_id.clone(),
                protocol_version: PROTOCOL_VERSION,
                capabilities: Some(default_capabilities()),
                host_nonce: random_id(8),
            };
            let noise_message = self.session.write(&reply.encode_to_vec()).unwrap();
            let real = fingerprint(self.identity.public_key());
            PairingClaimResponse {
                claim_id: request.claim_id.clone(),
                host_id: "host-1".into(),
                phase: PairingPhase::Hello as i32,
                noise_message,
                host_public_key: self.identity.public_key().to_vec(),
                host_fingerprint: if lie_about_fingerprint {
                    "00".repeat(32)
                } else {
                    real
                },
                device_id: request.device_id.clone(),
                protocol_version: PROTOCOL_VERSION,
                ..Default::default()
            }
        }

        fn finish_response(&mut self, request: &PairingClaimRequest) -> PairingClaimResponse {
            let payload = self.session.read(&request.noise_message).unwrap();
            let finish = PairingFinish::decode_payload(&payload).unwrap();
            assert_eq!(finish.device_id, request.device_id);
            PairingClaimResponse {
                claim_id: request.claim_id.clone(),
                host_id: "host-1".into(),
                phase: PairingPhase::Finish as i32,
                device_id: request.device_id.clone(),
                // Empty means "not reported"; the client verifies only when present.
                device_public_key: Vec::new(),
                device_token: "device-token".into(),
                protocol_version: PROTOCOL_VERSION,
                ..Default::default()
            }
        }
    }

    trait DecodeAny: Sized {
        fn decode_payload(bytes: &[u8]) -> Option<Self>;
    }

    impl DecodeAny for PairingHello {
        fn decode_payload(bytes: &[u8]) -> Option<Self> {
            Self::decode(bytes).ok()
        }
    }

    impl DecodeAny for PairingFinish {
        fn decode_payload(bytes: &[u8]) -> Option<Self> {
            Self::decode(bytes).ok()
        }
    }

    #[test]
    fn parses_optional_mqtt_override() {
        let encoded = urlencoding(ACCESS);
        let parsed = parse_pairing_uri(&format!(
            "whalerein://pair?v=1&access={encoded}&host=host-1&token=t&mqtt=mqtts%3A%2F%2Fbroker.test%3A8884"
        ))
        .unwrap();
        assert_eq!(parsed.mqtt_url.as_deref(), Some("mqtts://broker.test:8884"));
        let parsed = parse_pairing_uri(&format!(
            "whalerein://pair?v=1&access={encoded}&host=host-1&token=t&mqtt=http%3A%2F%2Fbroker.test"
        ));
        assert!(parsed.is_err());
    }

    #[test]
    fn parses_valid_uri_without_leaking_token_in_debug() {
        let parsed = uri();
        assert_eq!(parsed.host_id, "host-1");
        assert_eq!(parsed.access_url.as_str(), "https://access.example.test/");
        assert_eq!(parsed.token_hash_hex, sha256_hex(TOKEN.as_bytes()));
        let debug = format!("{parsed:?}");
        assert!(!debug.contains(TOKEN));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn rejects_invalid_uris() {
        let cases = [
            "",
            "https://pair?v=1&access=x&host=h&token=t",
            "whalerein://other?v=1&access=https%3A%2F%2Fa.test&host=h&token=t",
            "whalerein://pair?access=https%3A%2F%2Fa.test&host=h&token=t",
            "whalerein://pair?v=2&access=https%3A%2F%2Fa.test&host=h&token=t",
            "whalerein://pair?v=1&host=h&token=t",
            "whalerein://pair?v=1&access=https%3A%2F%2Fa.test&token=t",
            "whalerein://pair?v=1&access=https%3A%2F%2Fa.test&host=h",
            "whalerein://pair?v=1&access=https%3A%2F%2Fa.test&host=&token=t",
            "whalerein://pair?v=1&access=https%3A%2F%2Fa.test&host=h&token=",
            "whalerein://pair?v=1&access=ftp%3A%2F%2Fa.test&host=h&token=t",
            "whalerein://pair?v=1&access=https%3A%2F%2Fuser%3Apw%40a.test&host=h&token=t",
            "whalerein://pair?v=1&access=https%3A%2F%2Fa.test&host=h&token=t&v=1",
        ];
        for case in cases {
            assert!(parse_pairing_uri(case).is_err(), "expected reject: {case}");
        }
    }

    #[test]
    fn full_pairing_flow_uses_noise_fingerprint() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        assert_eq!(client.state(), PairingState::AwaitingHello);

        let mut host = TestHost::new(&uri, client.claim_id());
        let response = host.hello_response(&hello, true);
        let confirmation = client.accept_hello(&response).unwrap();
        assert_eq!(client.state(), PairingState::AwaitingConfirmation);
        assert_eq!(
            confirmation.fingerprint,
            fingerprint(host.identity.public_key())
        );
        assert_eq!(
            confirmation.fingerprint_display,
            format_fingerprint(&fingerprint(host.identity.public_key()))
        );
        assert_eq!(confirmation.host_id, "host-1");

        client.confirm(&PairingDecision::approve()).unwrap();
        let finish = client.write_finish().unwrap();
        assert!(finish.confirmed);
        assert_eq!(finish.phase, PairingPhase::Finish as i32);
        assert_eq!(client.state(), PairingState::AwaitingFinish);

        let response = host.finish_response(&finish);
        let completion = client.complete(&response).unwrap();
        assert_eq!(completion.device_token, "device-token");
        assert_eq!(completion.device_id, "device-1");
        assert_eq!(client.state(), PairingState::Complete);
        assert!(!format!("{completion:?}").contains("device-token"));
    }

    #[test]
    fn rejection_sends_no_msg3() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        let response = host.hello_response(&hello, false);
        client.accept_hello(&response).unwrap();

        assert!(matches!(
            client.confirm(&PairingDecision::reject()),
            Err(PairingError::Cancelled)
        ));
        assert_eq!(client.state(), PairingState::Cancelled);
        assert!(matches!(
            client.write_finish(),
            Err(PairingError::InvalidState)
        ));
        assert!(!host.session.is_finished());
    }

    #[test]
    fn fingerprint_mismatch_sends_no_msg3() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        let response = host.hello_response(&hello, false);
        client.accept_hello(&response).unwrap();

        assert!(matches!(
            client.confirm(&PairingDecision::expect("00".repeat(32))),
            Err(PairingError::FingerprintMismatch)
        ));
        assert_eq!(client.state(), PairingState::Failed);
        assert!(client.write_finish().is_err());
        assert!(!host.session.is_finished());
    }

    #[test]
    fn expected_fingerprint_accepts_grouped_display() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        let response = host.hello_response(&hello, false);
        let confirmation = client.accept_hello(&response).unwrap();
        client
            .confirm(&PairingDecision::expect(
                confirmation.fingerprint_display.clone(),
            ))
            .unwrap();
    }

    #[test]
    fn protocol_version_mismatch_is_rejected_before_confirmation() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        let payload = host.session.read(&hello.noise_message).unwrap();
        let _hello = PairingHello::decode_payload(&payload).unwrap();
        let reply = PairingHelloReply {
            host_id: "host-1".into(),
            device_id: hello.device_id.clone(),
            protocol_version: 99,
            capabilities: Some(default_capabilities()),
            host_nonce: "n".into(),
        };
        let noise_message = host.session.write(&reply.encode_to_vec()).unwrap();
        let response = PairingClaimResponse {
            claim_id: hello.claim_id.clone(),
            host_id: "host-1".into(),
            phase: PairingPhase::Hello as i32,
            noise_message,
            protocol_version: 99,
            ..Default::default()
        };
        assert!(matches!(
            client.accept_hello(&response),
            Err(PairingError::ProtocolIncompatible)
        ));
        assert_eq!(client.state(), PairingState::Failed);
        assert!(client.write_finish().is_err());
    }

    #[test]
    fn response_mismatches_are_rejected() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, _hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut response = PairingClaimResponse {
            claim_id: "other".into(),
            ..Default::default()
        };
        assert!(matches!(
            client.accept_hello(&response),
            Err(PairingError::ResponseMismatch)
        ));

        let (mut client, _hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        response = PairingClaimResponse {
            claim_id: client.claim_id().into(),
            host_id: "other".into(),
            ..Default::default()
        };
        assert!(matches!(
            client.accept_hello(&response),
            Err(PairingError::ResponseMismatch)
        ));

        let (mut client, _hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        response = PairingClaimResponse {
            claim_id: client.claim_id().into(),
            host_id: "host-1".into(),
            phase: PairingPhase::Finish as i32,
            ..Default::default()
        };
        assert!(matches!(
            client.accept_hello(&response),
            Err(PairingError::ResponseMismatch)
        ));
    }

    #[test]
    fn out_of_order_calls_are_rejected() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (mut client, _hello) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        assert!(matches!(
            client.write_finish(),
            Err(PairingError::InvalidState)
        ));
        assert!(matches!(
            client.confirm(&PairingDecision::approve()),
            Err(PairingError::InvalidState)
        ));
        assert!(matches!(
            client.complete(&Default::default()),
            Err(PairingError::InvalidState)
        ));
        client.cancel();
        assert_eq!(client.state(), PairingState::Cancelled);
        client.cancel();
        assert_eq!(client.state(), PairingState::Cancelled);
    }

    fn flow_until_finish(uri: &PairingUri) -> (PairingInitiator, TestHost, PairingClaimRequest) {
        let identity = StaticIdentity::generate();
        let (mut client, hello) =
            PairingInitiator::start(uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut host = TestHost::new(uri, client.claim_id());
        let response = host.hello_response(&hello, false);
        client.accept_hello(&response).unwrap();
        client.confirm(&PairingDecision::approve()).unwrap();
        let finish = client.write_finish().unwrap();
        (client, host, finish)
    }

    #[test]
    fn completion_rejects_wrong_device_key() {
        let uri = uri();
        let (mut client, mut host, finish) = flow_until_finish(&uri);
        let mut wrong_key = host.finish_response(&finish);
        wrong_key.device_public_key = vec![1; 32];
        assert!(matches!(
            client.complete(&wrong_key),
            Err(PairingError::DeviceKeyMismatch)
        ));
    }

    #[test]
    fn completion_rejects_missing_token() {
        let uri = uri();
        let (mut client, mut host, finish) = flow_until_finish(&uri);
        let mut missing_token = host.finish_response(&finish);
        missing_token.device_token.clear();
        assert!(matches!(
            client.complete(&missing_token),
            Err(PairingError::ResponseMismatch)
        ));
    }

    #[test]
    fn start_named_sanitizes_the_display_name() {
        let uri = uri();
        let identity = StaticIdentity::generate();
        let (client, request) = PairingInitiator::start_named(
            &uri,
            "device-1",
            &identity,
            default_capabilities(),
            "  办公室的笔记本\n",
        )
        .unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        assert_eq!(host.read_hello(&request).display_name, "办公室的笔记本");

        let identity = StaticIdentity::generate();
        let (client, request) = PairingInitiator::start_named(
            &uri,
            "device-1",
            &identity,
            default_capabilities(),
            "x".repeat(80),
        )
        .unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        let hello = host.read_hello(&request);
        assert_eq!(hello.display_name.chars().count(), 64);
        assert!(hello.display_name.chars().all(|ch| ch == 'x'));

        assert_eq!(sanitize_display_name(&"鲸".repeat(80)), "鲸".repeat(64));
        assert_eq!(sanitize_display_name(&"鲸".repeat(64)), "鲸".repeat(64));
        assert_eq!(sanitize_display_name("a\u{0001}b"), "ab");
        assert_eq!(sanitize_display_name(" \n\u{007F} "), "");

        let identity = StaticIdentity::generate();
        let (client, request) =
            PairingInitiator::start(&uri, "device-1", &identity, default_capabilities()).unwrap();
        let mut host = TestHost::new(&uri, client.claim_id());
        let hello = host.read_hello(&request);
        assert_eq!(hello.display_name, "");
        assert_eq!(hello.os, sanitize_os(std::env::consts::OS));
        assert_eq!(sanitize_os(" macOS "), "macos");
        assert_eq!(sanitize_os("browser"), "");
    }

    #[test]
    fn default_capabilities_cover_first_version_features() {
        let capabilities = default_capabilities();
        assert_eq!(capabilities.protocol_version, PROTOCOL_VERSION);
        let mut actual = capabilities.capabilities;
        actual.sort_unstable();
        let mut expected = vec![
            Capability::Sessions as i32,
            Capability::Events as i32,
            Capability::Messages as i32,
            Capability::Approvals as i32,
            Capability::Interrupt as i32,
            Capability::Sync as i32,
            Capability::Snapshot as i32,
        ];
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }
}
