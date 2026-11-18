//! Two-step Noise channel establishment over `HostRequest{kind=HANDSHAKE}`.

use prost::Message;
use whalerein_proto::{
    request_payload::Payload, Capability, CapabilitySet, ChannelHello, ChannelReady, HostRequest,
    RequestKind, RequestPayload,
};

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
    let stale = force_new.then(|| {
        runtime
            .channel(host_id)
            .map(|channel| channel.channel_id().to_owned())
    });
    ensure_channel_guarded(
        runtime,
        access,
        identity,
        host_id,
        device_id,
        force_new,
        stale,
        expected_fingerprint,
    )
    .await
}

/// Refresh only if the channel which failed is still current. If another
/// observation has already reconnected, reuse its replacement.
pub(crate) async fn ensure_channel_after(
    runtime: &Runtime,
    access: &AccessClient,
    identity: &StaticIdentity,
    host_id: &str,
    device_id: &str,
    stale_channel_id: &str,
    expected_fingerprint: Option<&str>,
) -> CoreResult<SecureChannel> {
    ensure_channel_guarded(
        runtime,
        access,
        identity,
        host_id,
        device_id,
        true,
        Some(Some(stale_channel_id.to_owned())),
        expected_fingerprint,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn ensure_channel_guarded(
    runtime: &Runtime,
    access: &AccessClient,
    identity: &StaticIdentity,
    host_id: &str,
    device_id: &str,
    mut force_new: bool,
    stale_channel_id: Option<Option<String>>,
    expected_fingerprint: Option<&str>,
) -> CoreResult<SecureChannel> {
    let lock = runtime.channel_lock(host_id);
    let _guard = lock.lock().await;
    if runtime.is_revoked() {
        return Err(CoreError::Revoked);
    }
    if force_new {
        if let (Some(observed), Some(current)) = (&stale_channel_id, runtime.channel(host_id)) {
            if observed.as_deref() != Some(current.channel_id()) {
                force_new = false;
            }
        }
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
            // The capability set is read from `ChannelReady` once, next to the
            // channel it belongs to; gating reads it back from the Runtime.
            runtime.set_capabilities(host_id, established.capabilities);
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
    /// The capability set the Host advertised in `ChannelReady`. An absent
    /// field decodes to the empty set, which advertises nothing.
    capabilities: CapabilitySet,
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
    let capabilities = ready.capabilities.unwrap_or_default();

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
        capabilities,
        host_public_key,
    })
}

/// The capability a request payload requires on the negotiated channel.
///
/// Discovery and health probes (`Ping`, `GetHostCapabilities`) are never
/// gated: an older peer must still answer them so a client can decide whether
/// a later operation is supported. Session lifecycle commands (create, fork)
/// and model operations are session-scoped, so they require `Sessions`; their
/// finer-grained support is negotiated separately through `HostCapabilities`.
pub fn required_capability(payload: &RequestPayload) -> Option<Capability> {
    match payload.payload.as_ref()? {
        Payload::ListSessions(_)
        | Payload::GetSession(_)
        | Payload::GetCommandResult(_)
        | Payload::CreateSession(_)
        | Payload::ForkSession(_)
        | Payload::ListModels(_)
        | Payload::ReadSessionModel(_)
        | Payload::SelectSessionModel(_) => Some(Capability::Sessions),
        Payload::ReadHistory(_)
        | Payload::WatchSession(_)
        | Payload::RefreshWatches(_)
        | Payload::UnwatchSession(_) => Some(Capability::Events),
        Payload::SendMessage(_) | Payload::ReadPendingQueue(_) | Payload::UpdatePendingQueue(_) => {
            Some(Capability::Messages)
        }
        Payload::GetApproval(_)
        | Payload::ResolveApproval(_)
        | Payload::ListApprovals(_)
        | Payload::ListQuestions(_)
        | Payload::GetQuestion(_)
        | Payload::AnswerQuestion(_)
        | Payload::CancelQuestionAnswer(_) => Some(Capability::Approvals),
        Payload::Interrupt(_) => Some(Capability::Interrupt),
        Payload::WaitSession(_) | Payload::CancelSessionWait(_) | Payload::ReadWaitState(_) => {
            Some(Capability::Sync)
        }
        Payload::Ping(_) | Payload::GetHostCapabilities(_) => None,
    }
}

/// Reject an operation before submitting it when the channel negotiated
/// without the capability it requires.
pub fn require_payload_capability(
    runtime: &Runtime,
    host_id: &str,
    payload: &RequestPayload,
) -> CoreResult<()> {
    let Some(required) = required_capability(payload) else {
        return Ok(());
    };
    let advertised = runtime.capabilities(host_id).unwrap_or_default();
    if whalerein_proto::has_capability(&advertised, required) {
        Ok(())
    } else {
        tracing::debug!(
            host_id,
            capability = required.as_str_name(),
            "operation requires a capability this channel did not negotiate"
        );
        Err(CoreError::Unsupported)
    }
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
    async fn a_channel_that_omits_a_capability_rejects_the_operation_before_submission() {
        use crate::pairing::default_capabilities;
        use whalerein_proto::{request_payload::Payload, Capability, ListSessions, WatchSession};

        let access = FakeAccess::start().await;
        let mut advertised = default_capabilities();
        advertised
            .capabilities
            .retain(|value| *value != Capability::Events as i32);
        access.host.lock().unwrap().channel_capabilities = Some(advertised);

        let client = AccessClient::new(access.url(), Some("device-token".into())).unwrap();
        let identity = StaticIdentity::generate();
        let runtime = Runtime::new();
        let _channel = ensure_channel(&runtime, &client, &identity, "host-1", "d-1", false, None)
            .await
            .unwrap();
        let negotiated = runtime.capabilities("host-1").expect("negotiated set");
        assert!(!whalerein_proto::has_capability(
            &negotiated,
            Capability::Events
        ));

        // The omitted capability is refused with the stable unsupported code
        // before any request is submitted.
        let watch = RequestPayload {
            payload: Some(Payload::WatchSession(WatchSession {
                session_id: "session-1".into(),
                interests: Some(whalerein_proto::ObservationInterests {
                    conversation: true,
                    questions: false,
                }),
            })),
        };
        let traced = access.host.lock().unwrap().request_traces.len();
        let error = require_payload_capability(&runtime, "host-1", &watch).unwrap_err();
        assert!(matches!(error, CoreError::Unsupported));
        assert_eq!(error.code(), "unsupported_capability");
        assert_eq!(error.exit_code(), 9);
        assert_eq!(
            access.host.lock().unwrap().request_traces.len(),
            traced,
            "a missing capability must be rejected before submitting"
        );

        // An advertised capability on the same channel is still accepted.
        let list = RequestPayload {
            payload: Some(Payload::ListSessions(ListSessions {})),
        };
        assert!(require_payload_capability(&runtime, "host-1", &list).is_ok());
    }

    #[tokio::test]
    async fn a_normal_channel_advertises_every_gated_capability_and_still_works() {
        use crate::cache::Cache;
        use crate::commands::CommandService;
        use std::sync::Arc;
        use whalerein_proto::{
            request_payload::Payload, Capability, Interrupt, ListApprovals, ListSessions,
            ReadWaitState, SendMessage, WatchSession,
        };

        let access = FakeAccess::start().await;
        let client = AccessClient::new(access.url(), Some("device-token".into())).unwrap();
        let identity = StaticIdentity::generate();
        let runtime = Runtime::new();
        let channel = ensure_channel(&runtime, &client, &identity, "host-1", "d-1", false, None)
            .await
            .unwrap();
        let negotiated = runtime.capabilities("host-1").expect("negotiated set");
        for capability in [
            Capability::Sessions,
            Capability::Events,
            Capability::Messages,
            Capability::Approvals,
            Capability::Interrupt,
            Capability::Sync,
            Capability::Snapshot,
        ] {
            assert!(
                whalerein_proto::has_capability(&negotiated, capability),
                "{capability:?}"
            );
        }

        let gated = [
            RequestPayload {
                payload: Some(Payload::ListSessions(ListSessions {})),
            },
            RequestPayload {
                payload: Some(Payload::WatchSession(WatchSession {
                    session_id: "session-1".into(),
                    interests: Some(whalerein_proto::ObservationInterests {
                        conversation: true,
                        questions: false,
                    }),
                })),
            },
            RequestPayload {
                payload: Some(Payload::SendMessage(SendMessage {
                    session_id: "session-1".into(),
                    text: "hello".into(),
                    delivery_mode: whalerein_proto::DeliveryMode::StartIfIdle as i32,
                })),
            },
            RequestPayload {
                payload: Some(Payload::ListApprovals(ListApprovals {
                    session_id: String::new(),
                })),
            },
            RequestPayload {
                payload: Some(Payload::Interrupt(Interrupt {
                    session_id: "session-1".into(),
                })),
            },
            RequestPayload {
                payload: Some(Payload::ReadWaitState(ReadWaitState {
                    session_id: "session-1".into(),
                })),
            },
        ];
        for payload in &gated {
            assert!(
                require_payload_capability(&runtime, "host-1", payload).is_ok(),
                "{:?}",
                required_capability(payload)
            );
        }
        // Discovery probes stay ungated so an older peer remains inspectable.
        let ping = RequestPayload {
            payload: Some(Payload::Ping(Default::default())),
        };
        assert!(required_capability(&ping).is_none());
        assert!(require_payload_capability(&runtime, "host-1", &ping).is_ok());

        // The same channel still carries an ordinary read.
        let cache = Arc::new(Cache::open_in_memory([3u8; 32]).unwrap());
        let service = CommandService::new(Arc::new(client), cache);
        let response = service
            .query(
                "host-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(Payload::ListSessions(ListSessions {})),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            response.payload,
            Some(whalerein_proto::response_payload::Payload::SessionList(_))
        ));
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
