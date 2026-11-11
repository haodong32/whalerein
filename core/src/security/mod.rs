//! Cryptography: device identity, Noise XX, secure channels and secrets.

pub mod channel;
pub(crate) mod device;
pub mod identity;
pub mod noise;
pub mod secrets;

pub use channel::{
    approval_digest, decode_frame, encode_frame, EventAad, RequestAad, SecureChannel,
};
pub use identity::{
    load_device_identity, load_device_token, load_or_create_cache_key, store_device_identity,
    store_device_token, StaticIdentity,
};
pub use noise::{fingerprint, NoiseSession};
pub use secrets::{keys, FileSecretStore, MemorySecretStore, SecretError, SecretStore};

#[cfg(feature = "keyring")]
pub use secrets::KeyringSecretStore;

/// Pairing prologue bound to the host, the token hash and the claim id.
pub fn pairing_prologue(host_id: &str, token_hash_hex: &str, claim_id: &str) -> String {
    format!("whalerein/v1/pairing|{host_id}|{token_hash_hex}|{claim_id}")
}

/// Secure-channel prologue bound to both peers and the negotiation channel id.
pub fn channel_prologue(host_id: &str, device_id: &str, channel_id: &str) -> String {
    format!("whalerein/v1/channel|{host_id}|{device_id}|{channel_id}")
}

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("invalid key material")]
    InvalidKey,
    #[error("invalid handshake state")]
    InvalidState,
    #[error("handshake is not complete")]
    HandshakeIncomplete,
    #[error("handshake is already complete")]
    HandshakeComplete,
    #[error("message authentication failed")]
    AuthenticationFailed,
    #[error("channel id does not match the active channel")]
    ChannelMismatch,
    #[error("invalid nonce")]
    InvalidNonce,
    #[error("invalid message")]
    InvalidMessage,
    #[error("secret storage failed: {0}")]
    Secret(#[from] SecretError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_exact_pairing_and_channel_prologues() {
        assert_eq!(
            pairing_prologue("host", "hash", "claim"),
            "whalerein/v1/pairing|host|hash|claim"
        );
        assert_eq!(
            channel_prologue("host", "device", "nego"),
            "whalerein/v1/channel|host|device|nego"
        );
    }
}
