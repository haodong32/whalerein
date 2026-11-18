//! Noise XX handshake wrapper on top of `snow`.
//!
//! Both pairing and secure channels use `Noise_XX_25519_ChaChaPoly_SHA256`
//! with an explicit prologue. The client is always the initiator; the DSH
//! plugin is the responder. The responder constructor exists for tests and the
//! interop tooling.

use snow::params::NoiseParams;
use snow::{Builder, HandshakeState};

use super::{SecurityError, StaticIdentity};

pub const PROTOCOL_NAME: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseRole {
    Initiator,
    #[cfg(any(test, feature = "test-support", feature = "protocol-testing"))]
    Responder,
}

pub struct NoiseSession {
    state: HandshakeState,
    role: NoiseRole,
}

impl std::fmt::Debug for NoiseSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseSession")
            .field("role", &self.role)
            .field("finished", &self.state.is_handshake_finished())
            .finish()
    }
}

impl NoiseSession {
    pub fn initiator(identity: &StaticIdentity, prologue: &str) -> Result<Self, SecurityError> {
        Self::build(identity, prologue, NoiseRole::Initiator, None)
    }

    #[cfg(any(test, feature = "test-support", feature = "protocol-testing"))]
    pub fn responder(identity: &StaticIdentity, prologue: &str) -> Result<Self, SecurityError> {
        Self::build(identity, prologue, NoiseRole::Responder, None)
    }

    /// Recover the same Noise transcript using the fresh random ephemeral
    /// private key retained for this one pairing operation in SecretStore.
    pub(crate) fn pairing_initiator(
        identity: &StaticIdentity,
        prologue: &str,
        ephemeral: &[u8; 32],
    ) -> Result<Self, SecurityError> {
        Self::build(identity, prologue, NoiseRole::Initiator, Some(ephemeral))
    }

    fn build(
        identity: &StaticIdentity,
        prologue: &str,
        role: NoiseRole,
        ephemeral: Option<&[u8; 32]>,
    ) -> Result<Self, SecurityError> {
        let params: NoiseParams = PROTOCOL_NAME.parse().map_err(|_| SecurityError::Crypto)?;
        let builder = Builder::new(params)
            .local_private_key(identity.private_key())
            .prologue(prologue.as_bytes());
        let builder = if let Some(ephemeral) = ephemeral {
            // The API names fixed keys for tests; here the key is independently
            // random and is reused exclusively to resume the same transcript.
            builder.fixed_ephemeral_key_for_testing_only(ephemeral)
        } else {
            builder
        };
        let state = match role {
            NoiseRole::Initiator => builder.build_initiator(),
            #[cfg(any(test, feature = "test-support", feature = "protocol-testing"))]
            NoiseRole::Responder => builder.build_responder(),
        }
        .map_err(|_| SecurityError::Crypto)?;
        Ok(Self { state, role })
    }

    #[cfg(test)]
    pub fn role(&self) -> NoiseRole {
        self.role
    }

    #[cfg(any(test, feature = "protocol-testing"))]
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Write the next handshake message with an application payload.
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, SecurityError> {
        if self.state.is_handshake_finished() {
            return Err(SecurityError::HandshakeComplete);
        }
        let mut message = vec![0u8; 65_535];
        let len = self
            .state
            .write_message(payload, &mut message)
            .map_err(|_| SecurityError::InvalidState)?;
        message.truncate(len);
        Ok(message)
    }

    /// Read the next handshake message and return its plaintext payload.
    pub fn read(&mut self, message: &[u8]) -> Result<Vec<u8>, SecurityError> {
        if self.state.is_handshake_finished() {
            return Err(SecurityError::HandshakeComplete);
        }
        let mut payload = vec![0u8; message.len()];
        let len = self
            .state
            .read_message(message, &mut payload)
            .map_err(|_| SecurityError::AuthenticationFailed)?;
        payload.truncate(len);
        Ok(payload)
    }

    /// Handshake hash after the final handshake message.
    pub fn handshake_hash(&self) -> Result<[u8; 32], SecurityError> {
        if !self.state.is_handshake_finished() {
            return Err(SecurityError::HandshakeIncomplete);
        }
        self.state
            .get_handshake_hash()
            .try_into()
            .map_err(|_| SecurityError::Crypto)
    }

    /// Remote static public key learned during a completed handshake.
    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.state
            .get_remote_static()
            .and_then(|key| key.try_into().ok())
    }
}

/// SHA-256 fingerprint of a remote static public key, lowercase hex.
pub fn fingerprint(public_key: &[u8]) -> String {
    crate::util::sha256_hex(public_key)
}

/// Human-friendly fingerprint (4-character groups).
#[cfg(test)]
pub fn display_fingerprint(public_key: &[u8]) -> String {
    crate::util::format_fingerprint(&fingerprint(public_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::StaticIdentity;

    const PROLOGUE: &str = "whalerein/v1/channel|host|device|negotiation";

    fn complete_pair(
        initiator: &StaticIdentity,
        responder: &StaticIdentity,
    ) -> (NoiseSession, NoiseSession) {
        let mut client = NoiseSession::initiator(initiator, PROLOGUE).unwrap();
        let mut host = NoiseSession::responder(responder, PROLOGUE).unwrap();

        let msg1 = client.write(b"hello").unwrap();
        assert_eq!(host.read(&msg1).unwrap(), b"hello");
        let msg2 = host.write(b"ready").unwrap();
        assert_eq!(client.read(&msg2).unwrap(), b"ready");
        let msg3 = client.write(b"finish").unwrap();
        assert_eq!(host.read(&msg3).unwrap(), b"finish");

        (client, host)
    }

    #[test]
    fn full_xx_handshake_matches_hashes_and_keys() {
        let client = StaticIdentity::generate();
        let host = StaticIdentity::generate();
        let (client_session, host_session) = complete_pair(&client, &host);

        assert_eq!(client_session.remote_static(), Some(*host.public_key()));
        assert_eq!(host_session.remote_static(), Some(*client.public_key()));
        assert_eq!(
            client_session.handshake_hash().unwrap(),
            host_session.handshake_hash().unwrap()
        );
        assert!(client_session.is_finished());
        assert!(host_session.is_finished());
        assert_eq!(client_session.role(), NoiseRole::Initiator);
        assert_eq!(host_session.role(), NoiseRole::Responder);
    }

    #[test]
    fn different_prologue_breaks_handshake() {
        let client = StaticIdentity::generate();
        let host = StaticIdentity::generate();
        let mut initiator = NoiseSession::initiator(&client, "one").unwrap();
        let mut responder = NoiseSession::responder(&host, "two").unwrap();
        let msg1 = initiator.write(b"hello").unwrap();
        assert_eq!(responder.read(&msg1).unwrap(), b"hello");
        let msg2 = responder.write(b"ready").unwrap();
        assert!(matches!(
            initiator.read(&msg2),
            Err(SecurityError::AuthenticationFailed)
        ));
    }

    #[test]
    fn tampered_message_is_rejected() {
        let client = StaticIdentity::generate();
        let host = StaticIdentity::generate();
        let mut initiator = NoiseSession::initiator(&client, PROLOGUE).unwrap();
        let mut responder = NoiseSession::responder(&host, PROLOGUE).unwrap();
        let msg1 = initiator.write(b"hello").unwrap();
        responder.read(&msg1).unwrap();
        let mut msg2 = responder.write(b"ready").unwrap();
        msg2[40] ^= 0x80;
        assert!(initiator.read(&msg2).is_err());
    }

    #[test]
    fn hash_is_unavailable_before_completion() {
        let client = StaticIdentity::generate();
        let mut session = NoiseSession::initiator(&client, PROLOGUE).unwrap();
        assert!(matches!(
            session.handshake_hash(),
            Err(SecurityError::HandshakeIncomplete)
        ));
        session.write(b"").unwrap();
        assert!(matches!(
            session.handshake_hash(),
            Err(SecurityError::HandshakeIncomplete)
        ));
    }

    #[test]
    fn fingerprint_is_sha256_hex() {
        let key = [0xab; 32];
        let expected = crate::util::sha256_hex(&key);
        assert_eq!(fingerprint(&key), expected);
        assert_eq!(
            display_fingerprint(&key),
            crate::util::format_fingerprint(&expected)
        );
        assert_eq!(fingerprint(&key).len(), 64);
    }

    #[test]
    fn debug_does_not_expose_keys() {
        let client = StaticIdentity::generate();
        let host = StaticIdentity::generate();
        let (client_session, host_session) = complete_pair(&client, &host);
        let text = format!("{client_session:?}{host_session:?}{client:?}{host:?}");
        assert!(!text.contains(&client.public_key_hex()));
        assert!(!text.contains(&hex::encode(client.private_key())));
    }
}
