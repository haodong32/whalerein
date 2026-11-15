//! Regression tests using real MQTT framing and the fake Access/Noise host.
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{MemorySecretStore, PairingDecision};
use bytes::BytesMut;
use prost::Message;
use rumqttc::v5::mqttbytes::v5::{Packet, Publish};
use rumqttc::v5::mqttbytes::QoS;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use whalerein_proto::{EventEnvelope, EventPayload};

async fn setup() -> (tempfile::TempDir, Core, FakeAccess, TcpListener) {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let core = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: Arc::new(MemorySecretStore::new()),
    })
    .unwrap();
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    core.sync_session("session-1").await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    core.config.write().unwrap().as_mut().unwrap().mqtt_url =
        Some(format!("mqtt://{}", listener.local_addr().unwrap()));
    (dir, core, access, listener)
}

async fn read_packet(stream: &mut TcpStream, buffer: &mut BytesMut) -> Packet {
    loop {
        if let Ok(packet) = Packet::read(buffer, None) {
            return packet;
        }
        assert_ne!(
            stream.read_buf(buffer).await.unwrap(),
            0,
            "MQTT peer closed"
        );
    }
}

async fn subscribe(listener: &TcpListener) -> TcpStream {
    subscribe_with_reason(listener, 0).await
}

async fn subscribe_with_reason(listener: &TcpListener, reason: u8) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = BytesMut::new();
    assert!(matches!(
        read_packet(&mut stream, &mut buffer).await,
        Packet::Connect(..)
    ));
    stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
    let Packet::Subscribe(subscription) = read_packet(&mut stream, &mut buffer).await else {
        panic!("expected subscribe")
    };
    let [high, low] = subscription.pkid.to_be_bytes();
    stream
        .write_all(&[0x90, 4, high, low, 0, reason])
        .await
        .unwrap();
    stream
}

async fn publish(stream: &mut TcpStream, envelope: EventEnvelope) {
    let topic = crate::access::device_event_topic(&envelope.host_id, &envelope.device_id);
    let packet = Publish::new(topic, QoS::AtMostOnce, envelope.encode_to_vec(), None);
    let mut bytes = BytesMut::new();
    packet.write(&mut bytes).unwrap();
    stream.write_all(&bytes).await.unwrap();
}

use crate::conversation::ViewUpdate;
use whalerein_proto::{
    live_frame, session_update, AttemptRef, LiveFrame, SessionKey, SessionUpdate, TextContent,
};
fn event(core: &Core, session: &str, watch: &str) -> EventEnvelope {
    let channel = core.runtime.channel("host-1").unwrap();
    let aad = crate::security::EventAad {
        host_id: channel.host_id(),
        device_id: channel.device_id(),
        channel_id: channel.channel_id(),
        created_at_unix_ms: 1,
    };
    let update = SessionUpdate {
        watch_id: watch.into(),
        session: Some(SessionKey {
            host_id: "host-1".into(),
            session_id: session.into(),
        }),
        payload: Some(session_update::Payload::Live(LiveFrame {
            attempt: Some(AttemptRef {
                live_epoch: "epoch".into(),
                attempt_id: "a".into(),
            }),
            block_id: "text".into(),
            first_seq: 1,
            last_seq: 1,
            payload: Some(live_frame::Payload::Delta(TextContent { text: "x".into() })),
        })),
    };
    let bytes = EventPayload {
        update: Some(update),
    }
    .encode_to_vec();
    let frame = channel.seal_event(&aad, &bytes).unwrap();
    EventEnvelope {
        host_id: channel.host_id().into(),
        device_id: channel.device_id().into(),
        channel_id: channel.channel_id().into(),
        created_at_unix_ms: 1,
        nonce: frame.nonce,
        ciphertext: frame.ciphertext,
        ..Default::default()
    }
}

#[tokio::test]
async fn watch_delivers_typed_preview_without_changing_the_committed_cursor() {
    let (_dir, core, _access, listener) = setup().await;
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(&mut stream, event(&peer_core, "other", "watch-1")).await;
        publish(&mut stream, event(&peer_core, "session-1", "old-watch")).await;
        publish(&mut stream, event(&peer_core, "session-1", "watch-1")).await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut seen = 0;
    let count = core
        .watch_session(&key, Some(Duration::from_secs(2)), |update| {
            if matches!(update, ViewUpdate::Preview { .. }) {
                seen += 1;
                false
            } else {
                true
            }
        })
        .await
        .unwrap();
    assert_eq!(seen, 1);
    assert_eq!(count, 1);
    assert_eq!(
        core.cache()
            .unwrap()
            .history_cursor(&key)
            .unwrap()
            .as_deref(),
        Some("bookmark:committed")
    );
    assert_eq!(core.cached_messages(&key).unwrap().len(), 1);
    peer.abort();
}
#[tokio::test]
async fn invalid_live_ciphertext_is_reported_without_revoking_pairing() {
    let (_dir, core, _access, listener) = setup().await;
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let mut frame = event(&peer_core, "session-1", "watch-1");
        frame.nonce.clear();
        publish(&mut stream, frame).await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let error = core
        .watch_session(&key, Some(Duration::from_secs(2)), |_| true)
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Security(_)));
    assert!(core.is_paired());
    peer.abort();
}
#[tokio::test]
async fn rejected_subscription_fails_without_forgetting_identity() {
    let (_dir, core, _access, listener) = setup().await;
    let peer = tokio::spawn(async move {
        let mut stream = subscribe_with_reason(&listener, 0x87).await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    assert!(core
        .watch_session(&key, Some(Duration::from_secs(1)), |_| true)
        .await
        .is_err());
    assert!(core.is_paired());
    peer.abort();
}
#[tokio::test]
async fn watch_deadline_cancels_the_mqtt_pump_and_closes_its_connection() {
    let (_dir, core, _access, listener) = setup().await;
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let mut b = [0; 256];
        loop {
            if stream.read(&mut b).await.unwrap() == 0 {
                break;
            }
        }
    });
    let key = core.session_key("session-1").await.unwrap();
    assert!(matches!(
        core.watch_session(&key, Some(Duration::from_millis(150)), |_| true)
            .await,
        Err(CoreError::Timeout)
    ));
    tokio::time::timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn host_scoped_sync_is_idempotent_and_summary_refresh_does_not_advance_it() {
    let (_dir, core, _access, _listener) = setup().await;
    let key = core.session_key("session-1").await.unwrap();
    let before = core.cache().unwrap().history_cursor(&key).unwrap();
    core.sessions("host-1").await.unwrap();
    assert_eq!(core.cache().unwrap().history_cursor(&key).unwrap(), before);
    assert_eq!(core.sync_session("session-1").await.unwrap(), 0);
    let other = SessionKey {
        host_id: "other".into(),
        session_id: key.session_id.clone(),
    };
    assert!(core.cached_messages(&other).unwrap().is_empty());
}
#[tokio::test]
async fn accepted_command_recovers_result_without_resending() {
    let (_dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().drop_channel_after_command = true;
    let result = core.send_message("session-1", "").await;
    assert_eq!(result.unwrap().status, OnlineStatus::Succeeded);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(core.is_paired());
}

#[tokio::test]
async fn revocation_while_waiting_for_command_ends_pairing() {
    let (_dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().revoke_after_command = true;
    let error = core.send_message("session-1", "").await.unwrap_err();
    assert!(
        matches!(error, CoreError::Revoked),
        "expected confirmed revocation: {error:?}"
    );
    assert!(!core.is_paired());
    assert_eq!(core.unconfirmed_revoked_commands().unwrap().len(), 1);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn operation_trace_covers_handshake_query_and_recovery_history() {
    let (dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().drop_channel();
    core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().query_error = Some(whalerein_proto::ErrorCode::Forbidden);
    let error = core.sessions("host-1").await.unwrap_err();
    core.save_failure(error.code()).unwrap();
    let host = access.host.lock().unwrap();
    assert!(host.request_traces.len() > 4);
    assert!(host
        .request_traces
        .iter()
        .all(|trace| trace == core.diagnostic_id()));
    let record = crate::diagnostics::journal::load_failure(dir.path()).unwrap();
    assert!(record
        .events
        .iter()
        .any(|event| event.stage == "query_channel_recovery"));
    assert!(record
        .events
        .iter()
        .any(|event| event.layer == "host" && event.stage == "query" && event.code == "forbidden"));
    assert!(record
        .events
        .iter()
        .any(|event| event.stage == "authorization_check" && event.outcome == "pairing_preserved"));
    let path = dir.path().join("export.json");
    core.export_diagnostics(&path).unwrap();
    let exported: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(exported["last_failure"]["trace_id"], core.diagnostic_id());
    core.clear_diagnostics().unwrap();
    core.clear_diagnostics().unwrap();
    std::fs::create_dir(dir.path().join(crate::diagnostics::journal::FAILURE_FILE)).unwrap();
    assert!(core.clear_diagnostics().is_err());
}

#[tokio::test]
async fn grants_config_loss_and_forget_have_distinct_identity_lifetimes() {
    let (dir, core, access, _listener) = setup().await;
    let original = core.require_config().unwrap().device_id;
    assert!(matches!(core.forget_device(), Err(CoreError::Conflict(_))));
    core.revoke().await.unwrap();
    assert!(!core.is_paired());
    let repaired = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(repaired.device_id, original);
    core.options
        .secrets
        .delete(&crate::security::keys::device_token(&original))
        .unwrap();
    let renewed = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(renewed.device_id, original);
    core.sessions("host-1").await.unwrap();
    std::fs::remove_file(dir.path().join("config.json")).unwrap();
    let options = core.options.clone();
    drop(core);
    let reopened = Core::new(options).unwrap();
    let recovered = reopened
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(recovered.device_id, original);
    reopened.revoke().await.unwrap();
    reopened.forget_device().unwrap();
    let fresh = reopened
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_ne!(fresh.device_id, original);
}
