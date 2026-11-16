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

fn key(host: &str, session: &str) -> crate::api::SessionKey {
    crate::api::SessionKey::new(host, session).unwrap()
}

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
    core.sync_session(&crate::api::SessionKey::new("host-1", "session-1").unwrap())
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    core.config.write().unwrap().as_mut().unwrap().mqtt_url =
        Some(format!("mqtt://{}", listener.local_addr().unwrap()));
    (dir, core, access, listener)
}

/// Same wiring as `setup` but with an empty history cursor, so the first watch
/// takes the cold-cache snapshot path.
async fn setup_cold() -> (tempfile::TempDir, Core, FakeAccess, TcpListener) {
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    core.config.write().unwrap().as_mut().unwrap().mqtt_url =
        Some(format!("mqtt://{}", listener.local_addr().unwrap()));
    (dir, core, access, listener)
}

fn hold_subscription(listener: TcpListener) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    })
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
    // AC-07.cancel: cancellation releases the profile watch lease with the
    // pump, so a fresh watch attaches instead of conflicting with the finished
    // one.
    assert!(matches!(
        core.watch_session(&key, Some(Duration::from_millis(100)), |_| true)
            .await,
        Err(CoreError::Timeout)
    ));
}
#[tokio::test]
async fn host_scoped_sync_is_idempotent_and_summary_refresh_does_not_advance_it() {
    let (_dir, core, _access, _listener) = setup().await;
    let key = core.session_key("session-1").await.unwrap();
    let before = core.cache().unwrap().history_cursor(&key).unwrap();
    core.sessions("host-1").await.unwrap();
    assert_eq!(core.cache().unwrap().history_cursor(&key).unwrap(), before);
    // An explicit sync reports the whole committed history even when the
    // cursor is already current; only watch attach suppresses the baseline.
    assert_eq!(core.sync_session(&key).await.unwrap(), 1);
    let other = crate::api::SessionKey {
        host_id: "other".into(),
        session_id: key.session_id.clone(),
    };
    assert!(core.cached_messages(&other).unwrap().is_empty());
}
#[tokio::test]
async fn accepted_command_recovers_result_without_resending() {
    let (_dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().drop_channel_after_command = true;
    let result = core.send_message(&key("host-1", "session-1"), "").await;
    assert_eq!(result.unwrap().status, OnlineStatus::Succeeded);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(core.is_paired());
}

#[tokio::test]
async fn revocation_while_waiting_for_command_ends_pairing() {
    let (_dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().revoke_after_command = true;
    let error = core
        .send_message(&key("host-1", "session-1"), "")
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            CoreError::Revoked | CoreError::RevokedRetentionUnverified
        ),
        "expected a confirmed revocation: {error:?}"
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

// ---------------------------------------------------------------------------
// STREAM-01: a snapshot replay is not new output for `tail --once`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cold_cache_snapshot_replay_is_not_once_watch_output() {
    let (_dir, core, _access, listener) = setup_cold().await;
    let peer = hold_subscription(listener);
    let key = core.session_key("session-1").await.unwrap();
    let mut messages = 0;
    let mut previews = 0;
    let result = core
        .watch_session(&key, Some(Duration::from_millis(400)), |update| {
            match update {
                ViewUpdate::Message { .. } => messages += 1,
                ViewUpdate::Preview { .. } => previews += 1,
                _ => {}
            }
            true
        })
        .await;
    assert!(
        matches!(result, Err(CoreError::Timeout)),
        "a cold-cache snapshot contains history, not new output: {result:?}"
    );
    assert_eq!(
        messages, 0,
        "replayed snapshot Messages must not be visible output"
    );
    assert_eq!(previews, 0);
    assert_eq!(
        core.cached_messages(&key).unwrap().len(),
        1,
        "the snapshot is still committed to the local cache"
    );
    peer.abort();
}

#[tokio::test]
async fn post_sync_watch_reports_only_the_new_committed_turn() {
    let (_dir, core, access, listener) = setup().await;
    let peer = hold_subscription(listener);
    access.host.lock().unwrap().history_advance = Some((
        vec![crate::api::test_support::history_message(
            "message-2",
            "new",
        )],
        "bookmark:turn-2".into(),
    ));
    let key = core.session_key("session-1").await.unwrap();
    let mut delivered = Vec::new();
    let count = core
        .watch_session(&key, Some(Duration::from_secs(2)), |update| {
            if let ViewUpdate::Message { message, .. } = update {
                delivered.push(message.message_id.clone());
                return false;
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(delivered, ["message-2"]);
    assert_eq!(count, 1);
    assert_eq!(core.cached_messages(&key).unwrap().len(), 2);
    peer.abort();
}

#[tokio::test]
async fn cold_cache_watch_reports_new_live_output_but_not_the_snapshot() {
    use tokio::sync::oneshot;
    let (_dir, core, _access, listener) = setup_cold().await;
    let peer_core = core.clone();
    let (state_tx, state_rx) = oneshot::channel();
    let mut state_tx = Some(state_tx);
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let _ = state_rx.await;
        publish(&mut stream, event(&peer_core, "session-1", "watch-1")).await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut messages = 0;
    let mut previews = 0;
    let mut state_seen = false;
    let count = core
        .watch_session(&key, Some(Duration::from_secs(3)), |update| {
            match update {
                ViewUpdate::Message { .. } => messages += 1,
                ViewUpdate::Preview { .. } => {
                    previews += 1;
                    return false;
                }
                ViewUpdate::State(_) => {
                    state_seen = true;
                    if let Some(sender) = state_tx.take() {
                        let _ = sender.send(());
                    }
                }
                _ => {}
            }
            true
        })
        .await
        .unwrap();
    assert!(state_seen);
    assert_eq!(messages, 0, "the snapshot history was not new output");
    assert_eq!(previews, 1);
    assert_eq!(count, 1);
    peer.abort();
}

// ---------------------------------------------------------------------------
// STREAM-04: interrupt success requires a terminal receipt.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn interrupt_waits_for_the_terminal_receipt_instead_of_returning_host_accepted() {
    let (_dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().pending_result_polls = 1;
    let handle = core.interrupt(&key("host-1", "session-1")).await.unwrap();
    assert_eq!(
        handle.status,
        OnlineStatus::Succeeded,
        "an accepted interrupt must be followed to its terminal receipt"
    );
    assert_eq!(access.host.lock().unwrap().pending_result_polls, 0);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(core.pending_commands().unwrap().is_empty());
}

#[tokio::test]
async fn interrupt_without_a_terminal_receipt_is_unknown_not_success() {
    let (_dir, core, access, _listener) = setup().await;
    {
        let mut host = access.host.lock().unwrap();
        host.pending_result_polls = 1;
        host.settled_command_status = whalerein_proto::OnlineStatus::Unknown;
    }
    let error = core
        .interrupt(&key("host-1", "session-1"))
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::UnknownResult), "{error:?}");
    assert_eq!(error.code(), "unknown");
    assert_eq!(error.exit_code(), 7);
    assert_eq!(
        core.pending_commands().unwrap().len(),
        1,
        "the unconfirmed stop stays recoverable by command id"
    );
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
}

#[tokio::test]
async fn interrupt_definitive_failure_is_not_reported_as_success() {
    let (_dir, core, access, _listener) = setup().await;
    {
        let mut host = access.host.lock().unwrap();
        host.pending_result_polls = 1;
        host.settled_command_status = whalerein_proto::OnlineStatus::Failed;
        host.settled_error_code = Some(whalerein_proto::ErrorCode::Unavailable);
    }
    let error = core
        .interrupt(&key("host-1", "session-1"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "unavailable");
    assert_eq!(error.exit_code(), 5);
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    assert!(core.pending_commands().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// STREAM-02/12: reconnect and gap recovery deliver unseen output exactly once.
// ---------------------------------------------------------------------------

fn sealed_event(core: &Core, update: SessionUpdate) -> EventEnvelope {
    let channel = core.runtime.channel("host-1").unwrap();
    let aad = crate::security::EventAad {
        host_id: channel.host_id(),
        device_id: channel.device_id(),
        channel_id: channel.channel_id(),
        created_at_unix_ms: 1,
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

fn update_for(session: &str, watch: &str, payload: session_update::Payload) -> SessionUpdate {
    SessionUpdate {
        watch_id: watch.into(),
        session: Some(SessionKey {
            host_id: "host-1".into(),
            session_id: session.into(),
        }),
        payload: Some(payload),
    }
}

fn reset_event(core: &Core, session: &str, watch: &str) -> EventEnvelope {
    sealed_event(
        core,
        update_for(
            session,
            watch,
            session_update::Payload::Reset(whalerein_proto::HistoryReset {
                reason: "fixture reset".into(),
            }),
        ),
    )
}

fn history_event(
    core: &Core,
    session: &str,
    watch: &str,
    from: &str,
    to: &str,
    batch: &str,
    messages: &[(&str, &str)],
) -> EventEnvelope {
    use whalerein_proto::{HistoryBatch, MessageUpsert};
    sealed_event(
        core,
        update_for(
            session,
            watch,
            session_update::Payload::History(HistoryBatch {
                from_cursor: from.into(),
                to_cursor: to.into(),
                batch_id: batch.into(),
                changes: messages
                    .iter()
                    .map(|(id, text)| MessageUpsert {
                        message: Some(crate::api::test_support::history_message(id, text)),
                        origin: None,
                    })
                    .collect(),
            }),
        ),
    )
}

#[tokio::test]
async fn gap_recovery_delivers_unseen_committed_messages_and_not_the_baseline() {
    use crate::api::test_support::history_message;
    use tokio::sync::oneshot;
    let (_dir, core, access, listener) = setup().await;
    let peer_core = core.clone();
    let (gap_tx, gap_rx) = oneshot::channel();
    let mut gap_tx = Some(gap_tx);
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let _ = gap_rx.await;
        // A live history frame that cannot continue the local cursor is a gap.
        publish(
            &mut stream,
            history_event(
                &peer_core,
                "session-1",
                "watch-1",
                "stale",
                "stale-2",
                "batch:gap",
                &[("message-2", "new")],
            ),
        )
        .await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut delivered = Vec::new();
    let count = core
        .watch_session(&key, Some(Duration::from_secs(3)), |update| {
            if let ViewUpdate::State(_) = update {
                // Make the recovery read return the whole committed history,
                // including the message the viewer already saw before attach.
                {
                    let mut host = access.host.lock().unwrap();
                    host.history_messages
                        .push(history_message("message-2", "new"));
                    host.history_cut = "bookmark:turn-2".into();
                }
                if let Some(sender) = gap_tx.take() {
                    let _ = sender.send(());
                }
            }
            if let ViewUpdate::Message { message, .. } = update {
                delivered.push(message.message_id.clone());
                if !delivered.is_empty() {
                    return false;
                }
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(
        delivered,
        ["message-2"],
        "gap recovery must deliver missed output, never replay the baseline"
    );
    assert_eq!(count, 1);
    let ids = core.cache().unwrap().history_message_ids(&key).unwrap();
    assert_eq!(ids, ["message-1", "message-2"]);
    peer.abort();
}

#[tokio::test]
async fn reconnect_does_not_replay_committed_history() {
    use tokio::sync::oneshot;
    let (_dir, core, _access, listener) = setup().await;
    let peer_core = core.clone();
    let (reset_tx, reset_rx) = oneshot::channel();
    let mut reset_tx = Some(reset_tx);
    let peer = tokio::spawn(async move {
        // First connection: the source resets, forcing a reconnect.
        let mut first = subscribe(&listener).await;
        let _ = reset_rx.await;
        publish(&mut first, reset_event(&peer_core, "session-1", "watch-1")).await;
        let mut b = [0; 16];
        let _ = first.read(&mut b).await;
        // Second connection: only new live output may be delivered.
        let mut second = subscribe(&listener).await;
        publish(&mut second, event(&peer_core, "session-1", "watch-1")).await;
        let _ = second.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut messages = 0;
    let mut previews = 0;
    let count = core
        .watch_session(&key, Some(Duration::from_secs(5)), |update| {
            match update {
                ViewUpdate::State(_) => {
                    if let Some(sender) = reset_tx.take() {
                        let _ = sender.send(());
                    }
                }
                ViewUpdate::Message { .. } => messages += 1,
                ViewUpdate::Preview { .. } => {
                    previews += 1;
                    return false;
                }
                _ => {}
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(messages, 0, "a reconnect must not replay committed history");
    assert_eq!(
        previews, 1,
        "the reconnected watch still receives live output"
    );
    assert_eq!(count, 1);
    assert_eq!(core.cached_messages(&key).unwrap().len(), 1);
    peer.abort();
}

#[tokio::test]
async fn refresh_head_gap_delivers_the_missed_turn_exactly_once() {
    use crate::api::test_support::history_message;
    let (_dir, core, access, listener) = setup().await;
    let peer = hold_subscription(listener);
    let key = core.session_key("session-1").await.unwrap();
    let mut delivered = Vec::new();
    let mut advanced = false;
    let count = core
        .watch_session(&key, Some(Duration::from_secs(8)), |update| {
            if let ViewUpdate::State(_) = update {
                if !advanced {
                    advanced = true;
                    let mut host = access.host.lock().unwrap();
                    host.history_messages
                        .push(history_message("message-2", "new"));
                    host.history_cut = "bookmark:turn-2".into();
                }
            }
            if let ViewUpdate::Message { message, .. } = update {
                delivered.push(message.message_id.clone());
                return false;
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(delivered, ["message-2"]);
    assert_eq!(count, 1);
    assert_eq!(core.cached_messages(&key).unwrap().len(), 2);
    peer.abort();
}

// ---------------------------------------------------------------------------
// STREAM-11 / XCUT-05 / XCUT-07: cache key and file recovery preserve pairing.
// ---------------------------------------------------------------------------

async fn paired_core() -> (tempfile::TempDir, Core, FakeAccess, Arc<MemorySecretStore>) {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secrets: Arc<MemorySecretStore> = Arc::new(MemorySecretStore::new());
    let core = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.sessions("host-1").await.unwrap();
    (dir, core, access, secrets)
}

#[tokio::test]
async fn a_missing_cache_key_rebuilds_the_cache_and_keeps_pairing() {
    let (dir, core, access, secrets) = paired_core().await;
    assert_eq!(core.cached_sessions("host-1").unwrap().len(), 1);
    let device_id = core.require_config().unwrap().device_id;
    let options = core.options.clone();
    drop(core);
    secrets
        .delete(&crate::security::keys::cache_key(&device_id))
        .unwrap();

    let reopened = Core::new(options).unwrap();
    // The first cache access detects that the new key cannot read the old
    // database, quarantines it and rebuilds without touching identity.
    assert!(reopened.cached_sessions("host-1").unwrap().is_empty());
    assert!(reopened.is_paired());
    assert_eq!(reopened.require_config().unwrap().device_id, device_id);
    assert_eq!(
        reopened.cache_recovery_reason().unwrap().as_deref(),
        Some("cache_key_invalid")
    );
    assert!(dir.path().join("cache.sqlite3.quarantine").is_file());
    // Doctor and Host discovery keep working after the rebuild.
    assert!(reopened.doctor().is_ok());
    assert_eq!(reopened.hosts().await.unwrap().len(), 1);
    assert_eq!(
        reopened.hosts().await.unwrap()[0].host_id,
        access.host.lock().unwrap().host_id
    );
}

#[tokio::test]
async fn a_tampered_cache_key_is_reported_as_cache_error_and_rebuilt() {
    let (dir, core, _access, secrets) = paired_core().await;
    let device_id = core.require_config().unwrap().device_id;
    let options = core.options.clone();
    drop(core);
    secrets
        .save(&crate::security::keys::cache_key(&device_id), &[7u8; 32])
        .unwrap();

    let reopened = Core::new(options).unwrap();
    assert!(reopened.cached_sessions("host-1").unwrap().is_empty());
    assert!(reopened.is_paired());
    assert_eq!(
        reopened.cache_recovery_reason().unwrap().as_deref(),
        Some("cache_key_invalid")
    );
    assert!(dir.path().join("cache.sqlite3.quarantine").is_file());
}

#[tokio::test]
async fn a_file_level_corrupt_cache_is_quarantined_not_treated_as_identity_damage() {
    let (dir, core, _access, _secrets) = paired_core().await;
    let options = core.options.clone();
    drop(core);
    std::fs::write(dir.path().join("cache.sqlite3"), b"not a sqlite database").unwrap();

    let reopened = Core::new(options).unwrap();
    assert!(reopened.cached_sessions("host-1").unwrap().is_empty());
    assert_eq!(
        reopened.cache_recovery_reason().unwrap().as_deref(),
        Some("cache_file_corrupt")
    );
    assert!(reopened.is_paired(), "cache damage must not revoke pairing");
    assert!(reopened.doctor().is_ok());
    assert!(
        dir.path()
            .join("cache.sqlite3.quarantine")
            .metadata()
            .unwrap()
            .len()
            > 0
    );
}

#[tokio::test]
async fn a_future_schema_cache_is_refused_without_destroying_it() {
    let (dir, core, _access, _secrets) = paired_core().await;
    let options = core.options.clone();
    drop(core);
    // A cache from a newer build must not be rebuilt over blindly.
    let path = dir.path().join("cache.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("UPDATE meta SET value='999' WHERE key='schema_version'", [])
        .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let reopened = Core::new(options).unwrap();
    let error = reopened.cached_sessions("host-1").unwrap_err();
    assert_eq!(error.code(), "cache_error");
    assert_eq!(error.exit_code(), 1);
    assert!(reopened.is_paired());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("cache.sqlite3.quarantine").exists());
    assert!(reopened.cache_recovery_reason().unwrap().is_none());
}

// ---------------------------------------------------------------------------
// STREAM-10: an unknown send keeps its identity and a resend recovers it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unconfirmed_send_reports_unknown_and_a_resend_recovers_without_resubmitting() {
    let (_dir, core, access, _listener) = setup().await;
    access.host.lock().unwrap().pending_result_polls = 50;
    let error = core
        .send_message(&key("host-1", "session-1"), "same text")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "unknown");
    assert_eq!(error.exit_code(), 7);
    let pending = core.pending_commands().unwrap();
    assert_eq!(pending.len(), 1);
    let retained = pending[0].request_id.clone();
    assert_eq!(access.host.lock().unwrap().commands_seen, 1);

    // A blind resend of the same text must recover the retained command, not
    // submit a second user message.
    {
        let mut host = access.host.lock().unwrap();
        host.pending_result_polls = 0;
        host.settled_command_status = whalerein_proto::OnlineStatus::Succeeded;
    }
    let recovered = core
        .send_message(&key("host-1", "session-1"), "same text")
        .await
        .unwrap();
    assert_eq!(recovered.request_id, retained);
    assert_eq!(recovered.status, OnlineStatus::Succeeded);
    assert_eq!(
        access.host.lock().unwrap().commands_seen,
        1,
        "the retry must recover the original command instead of submitting again"
    );
    assert!(core.pending_commands().unwrap().is_empty());
}

#[tokio::test]
async fn an_unadvertised_delivery_mode_is_rejected_before_any_submission() {
    let (_dir, core, access, _listener) = setup().await;
    {
        let mut host = access.host.lock().unwrap();
        host.sessions[0].source_capabilities = Some(whalerein_proto::SourceCapabilities {
            delivery_modes: vec![whalerein_proto::DeliveryMode::EnqueueAfterRun as i32],
            ..Default::default()
        });
    }
    let before = access.host.lock().unwrap().commands_seen;
    let error = core
        .send_message_with_delivery(
            &key("host-1", "session-1"),
            "fixture",
            crate::api::DeliveryMode::Steer,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "unsupported_capability");
    assert_eq!(error.exit_code(), 9);
    assert_eq!(
        access.host.lock().unwrap().commands_seen,
        before,
        "an unadvertised mode must not submit a command"
    );
    // The default mode still works, and a legacy Host with no advertisement
    // keeps the old pass-through path.
    core.send_message(&key("host-1", "session-1"), "fixture")
        .await
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.sessions[0].source_capabilities = Some(whalerein_proto::SourceCapabilities::default());
    }
    core.send_message_with_delivery(
        &key("host-1", "session-1"),
        "fixture",
        crate::api::DeliveryMode::Steer,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_bounded_tail_on_an_offline_host_reports_host_offline_not_timeout() {
    let (_dir, core, access, _listener) = setup().await;
    {
        let mut host = access.host.lock().unwrap();
        host.host_online = false;
        host.drop_channel();
    }
    // Drop the cached runtime channel so the watch must handshake with the
    // now-offline Host instead of reusing a stale channel.
    core.runtime.clear_channel("host-1");
    let key = core.session_key("session-1").await.unwrap();
    let error = core
        .watch_session(&key, Some(Duration::from_secs(2)), |_| true)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "host_offline");
    assert_eq!(error.exit_code(), 5);
}

#[tokio::test]
async fn a_tail_deadline_after_visible_output_is_success_not_timeout() {
    let (_dir, core, _access, listener) = setup().await;
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(&mut stream, event(&peer_core, "session-1", "watch-1")).await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut previews = 0;
    let count = core
        .watch_session(&key, Some(Duration::from_millis(600)), |update| {
            if matches!(update, ViewUpdate::Preview { .. }) {
                previews += 1;
            }
            true
        })
        .await
        .expect("output before the deadline is a success");
    assert_eq!(previews, 1);
    assert_eq!(count, 1);
    peer.abort();
}

fn live_frame_event(
    core: &Core,
    session: &str,
    watch: &str,
    seq: u64,
    text: &str,
) -> EventEnvelope {
    let payload = if seq == 1 {
        live_frame::Payload::Started(whalerein_proto::AttemptStarted {})
    } else {
        live_frame::Payload::Delta(TextContent { text: text.into() })
    };
    sealed_event(
        core,
        update_for(
            session,
            watch,
            session_update::Payload::Live(LiveFrame {
                attempt: Some(AttemptRef {
                    live_epoch: "epoch".into(),
                    attempt_id: "a".into(),
                }),
                block_id: "text".into(),
                first_seq: seq,
                last_seq: seq,
                payload: Some(payload),
            }),
        ),
    )
}

#[tokio::test]
async fn a_live_burst_during_attach_does_not_drop_preview_frames() {
    let (_dir, core, _access, listener) = setup().await;
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        for seq in 1..=100u64 {
            publish(
                &mut stream,
                live_frame_event(&peer_core, "session-1", "watch-1", seq, "x"),
            )
            .await;
        }
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut previews = 0;
    let _ = core
        .watch_session(&key, Some(Duration::from_secs(3)), |update| {
            if matches!(update, ViewUpdate::Preview { .. }) {
                previews += 1;
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(
        previews, 99,
        "a burst while the snapshot attaches must not drop live frames"
    );
    peer.abort();
}

#[tokio::test]
async fn repeated_interrupts_each_reach_terminal_success() {
    let (_dir, core, access, _listener) = setup().await;
    for _ in 0..2 {
        assert_eq!(
            core.interrupt(&key("host-1", "session-1"))
                .await
                .unwrap()
                .status,
            OnlineStatus::Succeeded
        );
    }
    assert_eq!(access.host.lock().unwrap().commands_seen, 2);
}

#[tokio::test]
async fn gap_recovery_with_a_lost_local_cursor_delivers_the_missed_messages() {
    use crate::api::test_support::history_message;
    use tokio::sync::oneshot;
    let (_dir, core, access, listener) = setup().await;
    let peer_core = core.clone();
    let (gap_tx, gap_rx) = oneshot::channel();
    let mut gap_tx = Some(gap_tx);
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let _ = gap_rx.await;
        publish(
            &mut stream,
            history_event(
                &peer_core,
                "session-1",
                "watch-1",
                "stale",
                "stale-2",
                "batch:gap",
                &[("message-2", "new")],
            ),
        )
        .await;
        let mut b = [0; 16];
        let _ = stream.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut delivered = Vec::new();
    let count = core
        .watch_session(&key, Some(Duration::from_secs(3)), |update| {
            if let ViewUpdate::State(_) = update {
                {
                    let mut host = access.host.lock().unwrap();
                    host.history_messages
                        .push(history_message("message-2", "new"));
                    host.history_cut = "bookmark:turn-2".into();
                }
                // Simulate a lost/retention-evicted local view: the gap
                // recovery must re-snapshot *and* deliver unseen messages.
                core.cache().unwrap().clear().unwrap();
                if let Some(sender) = gap_tx.take() {
                    let _ = sender.send(());
                }
            }
            if let ViewUpdate::Message { message, .. } = update {
                delivered.push(message.message_id.clone());
                if !delivered.is_empty() {
                    return false;
                }
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(
        delivered,
        ["message-2"],
        "a gap that forces a snapshot must still deliver output the viewer never saw"
    );
    assert_eq!(count, 1);
    peer.abort();
}

#[tokio::test]
async fn reconnect_delivers_one_committed_id_once_across_attach_and_buffered_batches() {
    use crate::api::test_support::history_message;
    let (_dir, core, access, listener) = setup().await;
    let host_state = access.host.clone();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut first = subscribe(&listener).await;
        // The source commits a new turn while this watch is still attached,
        // then resets the transport so the client must reconnect.
        {
            let mut host = host_state.lock().unwrap();
            host.history_messages
                .push(history_message("message-2", "new"));
            host.history_cut = "bookmark:turn-2".into();
        }
        publish(&mut first, reset_event(&peer_core, "session-1", "watch-1")).await;
        let mut b = [0; 16];
        let _ = first.read(&mut b).await;
        // The reconnected connection also buffers the live history batch for
        // the same committed message. The attach sync and the live batch must
        // resolve to one delivery for that message id.
        let mut second = subscribe(&listener).await;
        publish(
            &mut second,
            history_event(
                &peer_core,
                "session-1",
                "watch-1",
                "bookmark:committed",
                "bookmark:turn-2",
                "batch:turn-2",
                &[("message-2", "new")],
            ),
        )
        .await;
        let _ = second.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut delivered = Vec::new();
    let _ = core
        .watch_session(&key, Some(Duration::from_secs(3)), |update| {
            if let ViewUpdate::Message { message, .. } = update {
                delivered.push(message.message_id.clone());
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(
        delivered,
        ["message-2"],
        "a reconnected watch must deliver each committed id exactly once"
    );
    peer.abort();
}

#[tokio::test]
async fn a_user_and_assistant_pair_are_two_distinct_ids_each_delivered_once() {
    use crate::api::test_support::history_message;
    let (_dir, core, access, listener) = setup().await;
    let host_state = access.host.clone();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut first = subscribe(&listener).await;
        {
            let mut host = host_state.lock().unwrap();
            let mut user = history_message("message-2", "same text");
            user.role = whalerein_proto::MessageRole::User as i32;
            let mut assistant = history_message("message-3", "same text");
            assistant.role = whalerein_proto::MessageRole::Assistant as i32;
            host.history_messages.extend([user, assistant]);
            host.history_cut = "bookmark:turn-2".into();
        }
        publish(&mut first, reset_event(&peer_core, "session-1", "watch-1")).await;
        let mut b = [0; 16];
        let _ = first.read(&mut b).await;
        let mut second = subscribe(&listener).await;
        let _ = second.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut delivered = Vec::new();
    let _ = core
        .watch_session(&key, Some(Duration::from_secs(3)), |update| {
            if let ViewUpdate::Message { message, .. } = update {
                delivered.push(message.message_id.clone());
            }
            true
        })
        .await
        .unwrap();
    assert_eq!(
        delivered,
        ["message-2", "message-3"],
        "the user/assistant echo pair has distinct ids and each is delivered once"
    );
    peer.abort();
}

#[tokio::test]
async fn a_transient_channel_path_failure_reconnects_the_watch_and_keeps_output() {
    use tokio::sync::oneshot;
    let (_dir, core, access, listener) = setup().await;
    let host_state = access.host.clone();
    let peer_core = core.clone();
    let (gap_tx, gap_rx) = oneshot::channel();
    let mut gap_tx = Some(gap_tx);
    let peer = tokio::spawn(async move {
        let mut first = subscribe(&listener).await;
        let _ = gap_rx.await;
        // A gap forces a history read; make that read fail the way a channel
        // path change does, then let the retry succeed.
        host_state.lock().unwrap().query_error = Some(whalerein_proto::ErrorCode::Internal);
        publish(
            &mut first,
            history_event(
                &peer_core,
                "session-1",
                "watch-1",
                "stale",
                "stale-2",
                "batch:gap",
                &[("message-2", "new")],
            ),
        )
        .await;
        let mut b = [0; 16];
        let _ = first.read(&mut b).await;
        host_state.lock().unwrap().query_error = None;
        let mut second = subscribe(&listener).await;
        publish(
            &mut second,
            live_frame_event(&peer_core, "session-1", "watch-1", 2, "x"),
        )
        .await;
        let _ = second.read(&mut b).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut previews = 0;
    let mut armed = true;
    let count = core
        .watch_session(&key, Some(Duration::from_secs(5)), |update| {
            if let ViewUpdate::State(_) = update {
                if armed {
                    armed = false;
                    if let Some(sender) = gap_tx.take() {
                        let _ = sender.send(());
                    }
                }
            }
            if matches!(update, ViewUpdate::Preview { .. }) {
                previews += 1;
                return false;
            }
            true
        })
        .await
        .expect("a transient channel/path failure must reconnect, not end the watch");
    assert_eq!(previews, 1, "the rebuilt watch still receives live output");
    assert_eq!(count, 1);
    peer.abort();
}
