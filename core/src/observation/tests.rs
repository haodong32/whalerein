//! Focused observation tests. AC IDs are part of each test name.

use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{
    CoreOptions, MemorySecretStore, PairingDecision, QuestionAnswer, QuestionFreshness,
};
use bytes::BytesMut;
use prost::Message;
use rumqttc::v5::mqttbytes::v5::{Packet, Publish};
use rumqttc::v5::mqttbytes::QoS;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use whalerein_proto::{EventEnvelope, EventPayload, SessionKey as WireSessionKey, SessionUpdate};

async fn setup(sync_history: bool) -> (tempfile::TempDir, Core, FakeAccess, TcpListener) {
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
    let key = SessionKey::new("host-1", "session-1").unwrap();
    if sync_history {
        core.sync_session(&key).await.unwrap();
    }
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
    stream.write_all(&[0x90, 4, high, low, 0, 0]).await.unwrap();
    stream
}

async fn publish(stream: &mut TcpStream, envelope: EventEnvelope) {
    let topic = crate::access::device_event_topic(&envelope.host_id, &envelope.device_id);
    let packet = Publish::new(topic, QoS::AtMostOnce, envelope.encode_to_vec(), None);
    let mut bytes = BytesMut::new();
    packet.write(&mut bytes).unwrap();
    stream.write_all(&bytes).await.unwrap();
}

fn seal(core: &Core, update: SessionUpdate) -> EventEnvelope {
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

fn preview_event(core: &Core, session: &str, watch: &str, text: &str) -> EventEnvelope {
    use whalerein_proto::{live_frame, AttemptRef, LiveFrame, TextContent};
    seal(
        core,
        SessionUpdate {
            watch_id: watch.into(),
            session: Some(WireSessionKey {
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
                payload: Some(live_frame::Payload::Delta(TextContent {
                    text: text.into(),
                })),
            })),
        },
    )
}

fn question_event(
    core: &Core,
    session: &str,
    watch: &str,
    descriptor: whalerein_proto::QuestionDescriptor,
) -> EventEnvelope {
    seal(
        core,
        SessionUpdate {
            watch_id: watch.into(),
            session: Some(WireSessionKey {
                host_id: "host-1".into(),
                session_id: session.into(),
            }),
            payload: Some(session_update::Payload::Questions(descriptor)),
        },
    )
}

fn proto_batch(session: &str, batch_id: &str) -> whalerein_proto::QuestionBatch {
    whalerein_proto::QuestionBatch {
        session_id: session.into(),
        batch_id: batch_id.into(),
        source_epoch: "native".into(),
        version: 1,
        digest: vec![3; 32],
        phase: whalerein_proto::QuestionPhase::QuestionContinued as i32,
        questions: vec![whalerein_proto::QuestionItem {
            id: "q".into(),
            question: "Synthetic question".into(),
            options: vec![whalerein_proto::QuestionOption {
                label: "A".into(),
                description: None,
            }],
            ..Default::default()
        }],
        answerable: true,
        content_unavailable: false,
    }
}

fn proto_stamp(revision: u64) -> whalerein_proto::QuestionStamp {
    whalerein_proto::QuestionStamp {
        observation_epoch: "question-epoch-1".into(),
        revision,
    }
}

fn proto_page(
    revision: u64,
    next_cursor: &str,
    complete: bool,
    batches: Vec<whalerein_proto::QuestionBatch>,
) -> whalerein_proto::QuestionPage {
    whalerein_proto::QuestionPage {
        batches,
        next_cursor: next_cursor.into(),
        stamp: Some(proto_stamp(revision)),
        complete,
    }
}

fn descriptor_with(revision: u64) -> whalerein_proto::QuestionDescriptor {
    whalerein_proto::QuestionDescriptor {
        availability: whalerein_proto::QuestionAvailability::Available as i32,
        unavailable_code: String::new(),
        stamp: Some(proto_stamp(revision)),
    }
}

fn spawn_peer(
    listener: TcpListener,
) -> (
    tokio::sync::mpsc::UnboundedSender<EventEnvelope>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<EventEnvelope>();
    let task = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        while let Some(envelope) = rx.recv().await {
            publish(&mut stream, envelope).await;
        }
    });
    (tx, task)
}

async fn next_event(observation: &mut SessionObservation) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(5), observation.next_event())
        .await
        .expect("observation produced no event")
        .expect("observation ended")
}

async fn wait_question(
    observation: &mut SessionObservation,
    want: impl Fn(&QuestionView) -> bool,
) -> QuestionView {
    for _ in 0..20 {
        let event = next_event(observation).await;
        if let SessionEventPayload::Questions(view) = event.payload {
            if want(&view) {
                return view;
            }
        }
    }
    panic!("expected question view was not delivered")
}

#[test]
fn session_observation_ac_01_interests_select_only_requested_facets() {
    assert_eq!(
        ObservationInterests::conversation(),
        ObservationInterests {
            conversation: true,
            questions: false
        }
    );
    assert_eq!(
        ObservationInterests::questions(),
        ObservationInterests {
            conversation: false,
            questions: true
        }
    );
    assert!(ObservationInterests::both().conversation);
    assert!(ObservationInterests::both().questions);
    assert!(ObservationInterests::both().is_empty().eq(&false));
}

#[test]
fn session_observation_ac_18_core_owned_types_need_no_wire_dto() {
    // Consumer code only names Core-owned values.
    let stamp = crate::api::QuestionStamp {
        observation_epoch: "epoch".into(),
        revision: 4,
    };
    let view = QuestionView::current(stamp.clone(), Vec::new());
    assert_eq!(view.freshness, QuestionFreshness::Current);
    assert_eq!(view.stamp, Some(stamp));
    assert!(view.batches.is_empty());
    let payload = SessionEventPayload::Questions(view);
    assert!(matches!(payload, SessionEventPayload::Questions(_)));
    let page = crate::api::QuestionPageInfo {
        batches: Vec::new(),
        next_cursor: String::new(),
        stamp: None,
        complete: true,
        encoded_bytes: 0,
    };
    assert!(page.stamp.is_none());
}

#[tokio::test]
async fn session_observation_ac_01_one_observation_carries_conversation_and_questions() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(
            &mut stream,
            preview_event(&peer_core, "session-1", "watch-1", "x"),
        )
        .await;
        let mut buffer = [0u8; 16];
        let _ = stream.read(&mut buffer).await;
    });
    let mut observation = core
        .observe_session(&key, ObservationInterests::both(), Cancellation::new())
        .await
        .unwrap();
    let observation_id = observation.observation_id().to_owned();
    let generation = observation.generation();
    let mut reset = false;
    let mut state = false;
    let mut preview = false;
    let mut questions = false;
    for _ in 0..20 {
        let event = next_event(&mut observation).await;
        assert_eq!(event.session, key);
        assert_eq!(event.observation_id, observation_id);
        assert_eq!(event.generation, generation);
        match event.payload {
            SessionEventPayload::Conversation(ViewUpdate::Reset) => reset = true,
            SessionEventPayload::Conversation(ViewUpdate::State(_)) => state = true,
            SessionEventPayload::Conversation(ViewUpdate::Preview { text, .. }) => {
                assert_eq!(text, "x");
                preview = true;
            }
            SessionEventPayload::Questions(view)
                if view.freshness == QuestionFreshness::Current =>
            {
                questions = true
            }
            _ => {}
        }
        if reset && state && preview && questions {
            break;
        }
    }
    assert!(
        reset && state && preview && questions,
        "all facets delivered"
    );
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_02_questions_only_never_reads_history() {
    let (_dir, core, access, listener) = setup(false).await;
    let key = core.session_key("session-1").await.unwrap();
    access
        .host
        .lock()
        .unwrap()
        .question_pages
        .push_back(proto_page(
            1,
            "",
            true,
            vec![proto_batch("session-1", "b1")],
        ));
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut observation = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap();
    let view = wait_question(&mut observation, |view| {
        view.freshness == QuestionFreshness::Current
    })
    .await;
    assert_eq!(view.batches.len(), 1);
    assert_eq!(view.batches[0].batch_id, "b1");
    assert_eq!(
        access.host.lock().unwrap().history_reads,
        0,
        "questions-only observation must not read history"
    );
    assert!(access.host.lock().unwrap().question_reads >= 1);
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_03_unsupported_questions_keep_conversation() {
    let (_dir, core, access, listener) = setup(true).await;
    access.host.lock().unwrap().question_descriptor = whalerein_proto::QuestionDescriptor {
        availability: whalerein_proto::QuestionAvailability::Unsupported as i32,
        unavailable_code: "questions_unsupported".into(),
        stamp: None,
    };
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(
            &mut stream,
            preview_event(&peer_core, "session-1", "watch-1", "still-live"),
        )
        .await;
        let mut buffer = [0u8; 16];
        let _ = stream.read(&mut buffer).await;
    });
    let mut observation = core
        .observe_session(&key, ObservationInterests::both(), Cancellation::new())
        .await
        .unwrap();
    let unsupported = wait_question(&mut observation, |view| {
        view.freshness == QuestionFreshness::Unsupported
    })
    .await;
    assert_eq!(unsupported.error.as_deref(), Some("questions_unsupported"));
    let mut preview = false;
    for _ in 0..20 {
        let event = next_event(&mut observation).await;
        if let SessionEventPayload::Conversation(ViewUpdate::Preview { text, .. }) = event.payload {
            assert_eq!(text, "still-live");
            preview = true;
            break;
        }
    }
    assert!(preview, "conversation continues without question support");
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_03_unavailable_questions_are_not_empty_current() {
    let (_dir, core, access, listener) = setup(true).await;
    access.host.lock().unwrap().question_error = Some(whalerein_proto::ErrorCode::Unavailable);
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut observation = core
        .observe_session(&key, ObservationInterests::both(), Cancellation::new())
        .await
        .unwrap();
    let view = wait_question(&mut observation, |view| {
        matches!(
            view.freshness,
            QuestionFreshness::Unavailable | QuestionFreshness::Stale
        )
    })
    .await;
    assert_ne!(view.freshness, QuestionFreshness::Current);
    assert!(view.batches.is_empty());
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_04_question_outside_history_is_discovered() {
    let (_dir, core, access, listener) = setup(false).await;
    access
        .host
        .lock()
        .unwrap()
        .question_pages
        .push_back(proto_page(1, "", true, Vec::new()));
    let key = core.session_key("session-1").await.unwrap();
    let (events, peer) = spawn_peer(listener);
    let mut observation = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap();
    let empty = wait_question(&mut observation, |view| {
        view.freshness == QuestionFreshness::Current
    })
    .await;
    assert!(empty.batches.is_empty(), "current-empty is a real result");
    assert_eq!(empty.stamp.as_ref().unwrap().revision, 1);
    {
        let mut host = access.host.lock().unwrap();
        host.question_descriptor = descriptor_with(2);
        host.question_pages.push_back(proto_page(
            2,
            "",
            true,
            vec![proto_batch("session-1", "b2")],
        ));
    }
    events
        .send(question_event(
            &core,
            "session-1",
            "watch-1",
            descriptor_with(2),
        ))
        .unwrap();
    let changed = wait_question(&mut observation, |view| {
        view.freshness == QuestionFreshness::Current && !view.batches.is_empty()
    })
    .await;
    assert_eq!(changed.batches[0].batch_id, "b2");
    assert_eq!(changed.stamp.unwrap().revision, 2);
    drop(events);
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_08_later_page_failure_keeps_the_previous_view_stale() {
    let (_dir, core, access, listener) = setup(false).await;
    access
        .host
        .lock()
        .unwrap()
        .question_pages
        .push_back(proto_page(
            1,
            "",
            true,
            vec![proto_batch("session-1", "b1")],
        ));
    let key = core.session_key("session-1").await.unwrap();
    let (events, peer) = spawn_peer(listener);
    let mut observation = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap();
    let first = wait_question(&mut observation, |view| {
        view.freshness == QuestionFreshness::Current
    })
    .await;
    assert_eq!(first.stamp.unwrap().revision, 1);
    {
        let mut host = access.host.lock().unwrap();
        host.question_descriptor = descriptor_with(2);
        // First r2 page is fine; the second page has no stamp and cannot be
        // committed, so the previous complete view must remain Stale.
        host.question_pages
            .push_back(proto_page(2, "cursor-2", false, Vec::new()));
        host.question_pages
            .push_back(whalerein_proto::QuestionPage {
                batches: vec![proto_batch("session-1", "b2")],
                next_cursor: String::new(),
                stamp: None,
                complete: true,
            });
    }
    events
        .send(question_event(
            &core,
            "session-1",
            "watch-1",
            descriptor_with(2),
        ))
        .unwrap();
    let stale = wait_question(&mut observation, |view| {
        view.freshness == QuestionFreshness::Stale
    })
    .await;
    assert_eq!(stale.stamp.unwrap().revision, 1);
    assert_eq!(stale.batches[0].batch_id, "b1");
    drop(events);
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_11_foreign_scope_never_enters_the_observation() {
    let (_dir, core, _access, listener) = setup(true).await;
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(
            &mut stream,
            preview_event(&peer_core, "other-session", "watch-1", "foreign"),
        )
        .await;
        publish(
            &mut stream,
            preview_event(&peer_core, "session-1", "old-watch", "old"),
        )
        .await;
        publish(
            &mut stream,
            preview_event(&peer_core, "session-1", "watch-1", "valid"),
        )
        .await;
        let mut buffer = [0u8; 16];
        let _ = stream.read(&mut buffer).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let mut texts = Vec::new();
    for _ in 0..20 {
        let event = next_event(&mut observation).await;
        assert_eq!(event.session, key);
        if let SessionEventPayload::Conversation(ViewUpdate::Preview { text, .. }) = event.payload {
            texts.push(text);
            break;
        }
    }
    assert_eq!(texts, vec!["valid".to_owned()]);
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_17_close_releases_the_profile_lease_and_reopens() {
    let (_dir, core, _access, listener) = setup(false).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let mut buffer = [0u8; 16];
        let _ = stream.read(&mut buffer).await;
    });
    let mut first = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap();
    let conflict = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap_err();
    assert_eq!(conflict.code(), "conflict");
    first.close().await;
    let second = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await;
    assert!(second.is_ok(), "a new observation proceeds after close");
    drop(second);
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_17_cancellation_stops_an_idle_observation() {
    let (_dir, core, _access, listener) = setup(false).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let mut buffer = [0u8; 16];
        let _ = stream.read(&mut buffer).await;
    });
    let cancellation = Cancellation::new();
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::questions(),
            cancellation.clone(),
        )
        .await
        .unwrap();
    let cancelled = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancelled.cancel();
    });
    // Drain the initial Loading view, then cancellation ends the stream.
    assert!(tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if observation.next_event().await.is_none() {
                return;
            }
        }
    })
    .await
    .is_ok());
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_27_answer_heartbeat_renews_the_original_request_only() {
    let (_dir, core, access, _listener) = setup(false).await;
    let key = core.session_key("session-1").await.unwrap();
    let page = core.read_questions(&key, "").await.unwrap();
    let batch = page.batches.into_iter().next().unwrap();
    access.host.lock().unwrap().pending_result_polls = 3;
    let handle = core
        .answer_question(
            &key,
            &batch,
            vec![QuestionAnswer {
                id: "q".into(),
                selected: vec!["A".into()],
                custom: None,
            }],
        )
        .await
        .unwrap();
    let heartbeats = access.host.lock().unwrap().heartbeats.clone();
    assert!(
        !heartbeats.is_empty(),
        "submission/acceptance and terminal waiting renew the original R"
    );
    assert!(
        heartbeats
            .iter()
            .all(|request| request == &handle.request_id),
        "renewal addresses the original answer request, not follow-up queries"
    );
    let settled = heartbeats.len();
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(
        access.host.lock().unwrap().heartbeats.len(),
        settled,
        "no detached heartbeat survives completion"
    );
}

#[test]
fn session_observation_ac_17_second_lease_for_the_same_profile_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let first = WatchLease::acquire(directory.path()).unwrap();
    let error = WatchLease::acquire(directory.path()).unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert!(error.to_string().contains("another watch is active"));
    drop(first);
    // The OS lock is released immediately; a later observation can attach.
    WatchLease::acquire(directory.path()).unwrap();
}

#[tokio::test]
async fn session_observation_ac_18_terminal_error_preserves_the_fatal_code() {
    let (_dir, core, access, listener) = setup(false).await;
    // The Host advertises every capability except events, which is a fatal
    // observation failure rather than a reconnectable transport problem.
    let mut advertised = crate::pairing::default_capabilities();
    advertised
        .capabilities
        .retain(|value| *value != whalerein_proto::Capability::Events as i32);
    access.host.lock().unwrap().channel_capabilities = Some(advertised);
    core.runtime.clear_channel("host-1");
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut observation = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap();
    // Drain the initial Loading view, then the observation ends fatally.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while observation.next_event().await.is_some() {}
    })
    .await
    .is_ok();
    assert!(ended, "fatal failure closes the stream");
    let error = observation
        .take_terminal_error()
        .expect("fatal code is available to the adapter");
    assert_eq!(error.code(), "unsupported_capability");
    assert!(observation.take_terminal_error().is_none(), "take once");
    peer.abort();
}

#[tokio::test]
async fn session_observation_ac_25_mqtt_loss_reports_reconnecting() {
    use crate::runtime::ConnectionState;
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        let stream = subscribe(&listener).await;
        let _ = drop_rx.await;
        drop(stream);
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        core.get_connection_state(&key.host_id),
        ConnectionState::Connected
    );
    drop_tx.send(()).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if core.get_connection_state(&key.host_id) != ConnectionState::Connected {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "MQTT loss never changed the connection state"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    observation.close().await;
    peer.abort();
}

#[test]
fn a_scope_denial_ends_a_watch_instead_of_reconnecting() {
    // Access answers 401/403 when the grant or enrollment is gone. Retrying
    // that answer cannot recover the watch, so it must become the terminal
    // result before the caller's deadline.
    assert!(watch_fatal(&CoreError::Api {
        code: Some(whalerein_proto::ErrorCode::Forbidden),
        message: "forbidden".into(),
    }));
    assert!(watch_fatal(&CoreError::Unauthorized));
    // A transport-shaped failure remains reconnectable.
    assert!(!watch_fatal(&CoreError::Unavailable("mqtt down".into())));
    assert!(!watch_fatal(&CoreError::Timeout));
}
