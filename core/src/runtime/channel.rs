//! Two-step Noise channel establishment over `HostRequest{kind=HANDSHAKE}`.

use prost::Message;
use whalerein_proto::{ChannelHello, ChannelReady, HostRequest, RequestKind};

use super::{ConnectionState, HandshakePhase, Runtime};
use crate::access::AccessClient;
use crate::api::{CoreError, CoreResult, OnlineStatus};
use crate::pairing::{default_capabilities, PROTOCOL_VERSION};
use crate::security::{channel_prologue, fingerprint, NoiseSession, SecureChannel, StaticIdentity};
use crate::sync::SyncState;
use crate::util::{now_unix_ms, random_id};

pub const HANDSHAKE_TIMEOUT_MS: i64 = 30_000;

/// Ensure a channel exists, optionally forcing a fresh handshake.
///
/// A reconnect always creates a new channel and discards the old one, so old
/// ciphertext can never be opened with the new keys.
pub async fn ensure_channel(
    runtime: &Runtime,
    access: &AccessClient,
    identity: &StaticIdentity,
    host_id: &str,
    device_id: &str,
    force_new: bool,
    expected_fingerprint: Option<&str>,
) -> CoreResult<SecureChannel> {
    if runtime.is_revoked() {
        return Err(CoreError::Revoked);
    }
    if !force_new {
        if let Some(channel) = runtime.channel(host_id) {
            return Ok(channel);
        }
    }

    if runtime.channel(host_id).is_some() {
        runtime.set_state(host_id, ConnectionState::Reconnecting)?;
        runtime.clear_channel(host_id);
    }
    let current = runtime.state(host_id);
    if matches!(
        current,
        ConnectionState::Connected
            | ConnectionState::Reconnecting
            | ConnectionState::Syncing
            | ConnectionState::Handshaking
    ) {
        if current == ConnectionState::Connected {
            runtime.set_state(host_id, ConnectionState::Reconnecting)?;
        }
        if runtime.state(host_id) == ConnectionState::Reconnecting {
            runtime.set_state(host_id, ConnectionState::Connecting)?;
        }
    } else {
        runtime.set_state(host_id, ConnectionState::Connecting)?;
    }
    runtime.set_state(host_id, ConnectionState::Handshaking)?;
    runtime.set_handshake_phase(host_id, HandshakePhase::Msg1Sent);

    match handshake(access, identity, host_id, device_id, expected_fingerprint).await {
        Ok(established) => {
            runtime.set_channel(host_id, established.channel.clone());
            runtime.set_handshake_phase(host_id, HandshakePhase::Complete);
            runtime.set_state(host_id, ConnectionState::Syncing)?;
            runtime.set_sync_state(host_id, SyncState::Idle);
            runtime.set_state(host_id, ConnectionState::Connected)?;
            runtime.reset_reconnect(host_id);
            Ok(established.channel)
        }
        Err(error) => {
            runtime.set_handshake_phase(host_id, HandshakePhase::Failed);
            runtime.record_error(error.code());
            let target = if matches!(
                error,
                CoreError::HostOffline | CoreError::Unavailable(_) | CoreError::Timeout
            ) {
                ConnectionState::Offline
            } else {
                ConnectionState::Failed
            };
            let _ = runtime.set_state(host_id, target);
            Err(error)
        }
    }
}

struct Established {
    channel: SecureChannel,
    #[allow(dead_code)]
    host_public_key: [u8; 32],
}

async fn handshake(
    access: &AccessClient,
    identity: &StaticIdentity,
    host_id: &str,
    device_id: &str,
    expected_fingerprint: Option<&str>,
) -> CoreResult<Established> {
    // The negotiation id is random and bound into the prologue. The derived
    // application channel id is different and is used for encrypted frames.
    let negotiation_id = random_id(16);
    let prologue = channel_prologue(host_id, device_id, &negotiation_id);
    let mut noise = NoiseSession::initiator(identity, &prologue)?;

    let hello = ChannelHello {
        device_id: device_id.to_owned(),
        host_id: host_id.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: Some(default_capabilities()),
        client_nonce: random_id(16),
    };
    let msg1 = noise.write(&hello.encode_to_vec())?;
    let msg2 = send_handshake(access, host_id, device_id, &negotiation_id, msg1).await?;

    let ready_payload = noise.read(&msg2)?;
    let ready = ChannelReady::decode(ready_payload.as_slice())
        .map_err(|_| CoreError::Security("invalid ChannelReady payload".into()))?;
    if ready.protocol_version != PROTOCOL_VERSION {
        return Err(CoreError::ProtocolIncompatible);
    }
    if !ready.host_id.is_empty() && ready.host_id != host_id {
        return Err(CoreError::Unauthorized);
    }
    if !ready.device_id.is_empty() && ready.device_id != device_id {
        return Err(CoreError::Unauthorized);
    }

    // XX message 2 authenticates the responder static key. Check its pairing
    // pin before message 3 discloses the device identity to the responder.
    let host_public_key = noise.remote_static().ok_or(CoreError::Unauthorized)?;
    if let Some(expected) = expected_fingerprint {
        let actual = fingerprint(&host_public_key);
        if !expected.eq_ignore_ascii_case(&actual) {
            return Err(CoreError::FingerprintMismatch {
                expected: expected.to_owned(),
                actual,
            });
        }
    }

    // The plugin rejects a non-empty msg3 payload.
    let msg3 = noise.write(&[])?;
    let result = send_handshake(access, host_id, device_id, &negotiation_id, msg3).await?;
    if !result.is_empty() {
        return Err(CoreError::Security(
            "unexpected payload in handshake completion".into(),
        ));
    }

    let hash = noise.handshake_hash()?;
    let channel =
        SecureChannel::from_handshake(&hash, &prologue, &negotiation_id, host_id, device_id)?;
    tracing::debug!(
        trace_id = %access.trace_id(),
        host_fingerprint = %fingerprint(&host_public_key),
        "secure channel established"
    );
    Ok(Established {
        channel,
        host_public_key,
    })
}

async fn send_handshake(
    access: &AccessClient,
    host_id: &str,
    device_id: &str,
    negotiation_id: &str,
    body: Vec<u8>,
) -> CoreResult<Vec<u8>> {
    let request_id = random_id(16);
    let request = HostRequest {
        request_id: request_id.clone(),
        host_id: host_id.to_owned(),
        device_id: device_id.to_owned(),
        kind: RequestKind::Handshake as i32,
        command_id: String::new(),
        payload_digest: Vec::new(),
        expires_at_unix_ms: now_unix_ms() + HANDSHAKE_TIMEOUT_MS,
        client_transport_version: 2,
        relay_transport_version: 0,
        trace_id: access.trace_id().to_owned(),
        protocol_version: PROTOCOL_VERSION,
        channel_id: negotiation_id.to_owned(),
        body,
    };
    let receipt = access.submit_request(host_id, &request).await?;
    match OnlineStatus::from_proto(receipt.status) {
        Some(OnlineStatus::HostOffline) => return Err(CoreError::HostOffline),
        Some(OnlineStatus::Rejected) => return Err(CoreError::HostRejected),
        Some(OnlineStatus::Failed) | Some(OnlineStatus::Unknown) => {
            return Err(CoreError::UnknownResult)
        }
        _ => {}
    }

    let state = access.wait_for_request(&request_id, false).await?;
    match OnlineStatus::from_proto(state.status) {
        Some(OnlineStatus::Succeeded) => Ok(state.result_body),
        Some(OnlineStatus::HostOffline) => Err(CoreError::HostOffline),
        Some(OnlineStatus::Rejected) => Err(CoreError::HostRejected),
        Some(OnlineStatus::Failed) | Some(OnlineStatus::Unknown) => Err(CoreError::UnknownResult),
        _ => Err(CoreError::Internal("handshake did not complete".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    // The full two-step handshake with a fake Access host lives in the
    // integration-style tests in `api::tests`. These unit tests cover the
    // request shape and state guards without a network.

    #[test]
    fn revoked_runtime_refuses_handshake() {
        let runtime = Runtime::new();
        runtime.revoke();
        let access = AccessClient::new(
            url::Url::parse("http://127.0.0.1:1").unwrap(),
            Some("token".into()),
        )
        .unwrap();
        let identity = StaticIdentity::generate();
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(ensure_channel(
                &runtime, &access, &identity, "h", "d", false, None,
            ))
            .unwrap_err();
        assert!(matches!(error, CoreError::Revoked));
    }
}

#[cfg(test)]
mod fake_host_tests {
    use super::*;
    use crate::api::test_support::FakeAccess;

    #[tokio::test]
    async fn reconnect_always_uses_a_new_channel() {
        let access = FakeAccess::start().await;
        let client = AccessClient::new(access.url(), Some("device-token".into())).unwrap();
        let identity = StaticIdentity::generate();
        let runtime = Runtime::new();
        let first = ensure_channel(&runtime, &client, &identity, "host-1", "d-1", false, None)
            .await
            .unwrap();
        assert_eq!(runtime.state("host-1"), ConnectionState::Connected);
        let second = ensure_channel(&runtime, &client, &identity, "host-1", "d-1", true, None)
            .await
            .unwrap();
        assert_ne!(first.channel_id(), second.channel_id());
        assert_eq!(
            runtime.channel("host-1").unwrap().channel_id(),
            second.channel_id()
        );
        assert_eq!(runtime.reconnect_count("host-1"), 0);
    }

    #[tokio::test]
    async fn offline_host_marks_the_runtime_offline_and_records_the_error() {
        let access = FakeAccess::start().await;
        access.host.lock().unwrap().host_online = false;
        let client = AccessClient::new(access.url(), Some("device-token".into())).unwrap();
        let identity = StaticIdentity::generate();
        let runtime = Runtime::new();
        let error = ensure_channel(&runtime, &client, &identity, "host-1", "d-1", false, None)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::HostOffline));
        assert_eq!(runtime.state("host-1"), ConnectionState::Offline);
        assert_eq!(runtime.handshake_phase("host-1"), HandshakePhase::Failed);
        assert_eq!(runtime.recent_errors(), vec!["host_offline".to_owned()]);
    }
}
