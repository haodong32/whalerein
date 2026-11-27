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
    // The fixture switches the transport after the initial HTTP bootstrap.
    // Stop the connection opened against the fake Access broker so the
    // observation attaches to this raw peer.
    core.disconnect_host("host-1");
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
    // The Device publishes its receive advertisement after CONNACK and then
    // subscribes to its control event and notice topics at QoS 1 and its live
    // preview topic at QoS 0. Acknowledge the QoS 1 publication and every
    // subscription so the event loop can progress.
    let mut subscriptions: Vec<(String, QoS)> = Vec::new();
    while subscriptions.len() < 4 {
        match read_packet(&mut stream, &mut buffer).await {
            Packet::Publish(publish) => {
                let [high, low] = publish.pkid.to_be_bytes();
                stream.write_all(&[0x40, 4, high, low, 0, 0]).await.unwrap();
            }
            Packet::Subscribe(subscription) => {
                let [high, low] = subscription.pkid.to_be_bytes();
                stream.write_all(&[0x90, 4, high, low, 0, 0]).await.unwrap();
                for filter in subscription.filters {
                    subscriptions.push((filter.path, filter.qos));
                }
            }
            other => panic!("unexpected packet before subscribe: {other:?}"),
        }
    }
    assert!(subscriptions[0].0.ends_with("/event"));
    assert!(subscriptions[1].0.ends_with("/notice"));
    assert!(subscriptions[2].0.ends_with("/reply"));
    assert!(subscriptions[3].0.ends_with("/live"));
    assert_eq!(
        subscriptions[0].1,
        QoS::AtLeastOnce,
        "control events subscribe at QoS 1"
    );
    assert_eq!(
        subscriptions[1].1,
        QoS::AtLeastOnce,
        "control notices subscribe at QoS 1"
    );
    assert_eq!(
        subscriptions[2].1,
        QoS::AtLeastOnce,
        "query replies subscribe at QoS 1"
    );
    assert_eq!(
        subscriptions[3].1,
        QoS::AtMostOnce,
        "live previews subscribe at QoS 0"
    );
    let prefix = subscriptions[3].0.trim_end_matches("/live").to_owned();
    assert_eq!(
        subscriptions[0].0,
        format!("{prefix}/event"),
        "live and control topics share the device scope"
    );
    assert_eq!(subscriptions[1].0, format!("{prefix}/notice"));
    assert_eq!(subscriptions[2].0, format!("{prefix}/reply"));
    stream
}

/// Publish a live-class preview envelope on the device live topic.
async fn publish(stream: &mut TcpStream, envelope: EventEnvelope) {
    let topic = crate::access::mqtt::device_live_topic(&envelope.host_id, &envelope.device_id);
    publish_on(stream, topic, envelope).await;
}

/// Publish a control-class envelope on the device control event topic.
async fn publish_control(stream: &mut TcpStream, envelope: EventEnvelope) {
    let topic = crate::access::device_event_topic(&envelope.host_id, &envelope.device_id);
    publish_on(stream, topic, envelope).await;
}

async fn publish_on(stream: &mut TcpStream, topic: String, envelope: EventEnvelope) {
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
        watch_closed: None,
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
        relay_version: 5,
        ..Default::default()
    }
}

fn preview_event(core: &Core, session: &str, watch: &str, text: &str) -> EventEnvelope {
    use whalerein_proto::{AttemptRef, LiveFrame};
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
                payload: Some(whalerein_proto::live_frame::Payload::Delta(
                    whalerein_proto::LiveBlockDelta {
                        kind: whalerein_proto::BlockKind::Text as i32,
                        text: text.into(),
                    },
                )),
            })),
        },
    )
}

fn live_frame_event(
    core: &Core,
    session: &str,
    watch: &str,
    seq: u64,
    payload: whalerein_proto::live_frame::Payload,
) -> EventEnvelope {
    use whalerein_proto::{AttemptRef, LiveFrame};
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
                first_seq: seq,
                last_seq: seq,
                payload: Some(payload),
            })),
        },
    )
}

fn delta_event(core: &Core, session: &str, watch: &str, seq: u64, text: &str) -> EventEnvelope {
    live_frame_event(
        core,
        session,
        watch,
        seq,
        whalerein_proto::live_frame::Payload::Delta(whalerein_proto::LiveBlockDelta {
            kind: whalerein_proto::BlockKind::Text as i32,
            text: text.into(),
        }),
    )
}

fn state_event(core: &Core, session: &str, watch: &str, revision: u64) -> EventEnvelope {
    state_event_with_projection(core, session, watch, revision, "projection-1")
}

fn state_event_with_projection(
    core: &Core,
    session: &str,
    watch: &str,
    revision: u64,
    projection: &str,
) -> EventEnvelope {
    seal(
        core,
        SessionUpdate {
            watch_id: watch.into(),
            session: Some(WireSessionKey {
                host_id: "host-1".into(),
                session_id: session.into(),
            }),
            payload: Some(session_update::Payload::State(
                whalerein_proto::SessionState {
                    session: Some(WireSessionKey {
                        host_id: "host-1".into(),
                        session_id: session.into(),
                    }),
                    live_epoch: "epoch".into(),
                    revision,
                    history_head: "bookmark:committed".into(),
                    run_phase: whalerein_proto::RunPhase::Idle as i32,
                    questions: None,
                    projection_id: projection.into(),
                },
            )),
        },
    )
}

fn history_event(
    core: &Core,
    session: &str,
    watch: &str,
    batch: whalerein_proto::HistoryBatch,
) -> EventEnvelope {
    seal(
        core,
        SessionUpdate {
            watch_id: watch.into(),
            session: Some(WireSessionKey {
                host_id: "host-1".into(),
                session_id: session.into(),
            }),
            payload: Some(session_update::Payload::History(batch)),
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
            // Question descriptors are control-class intent.
            publish_control(&mut stream, envelope).await;
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
            recent_page: None,
            questions: false
        }
    );
    assert_eq!(
        ObservationInterests::questions(),
        ObservationInterests {
            conversation: false,
            recent_page: None,
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
        // Outlive the lease tick: this case is about a deterministic refusal,
        // not about a transport loss competing with it.
        tokio::time::sleep(Duration::from_secs(15)).await;
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
        // Outlive the lease tick: this case is about a deterministic refusal,
        // not about a transport loss competing with it.
        tokio::time::sleep(Duration::from_secs(15)).await;
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
        // Outlive the lease tick: this case is about a deterministic refusal,
        // not about a transport loss competing with it.
        tokio::time::sleep(Duration::from_secs(15)).await;
    });
    let key = core.session_key("session-1").await.unwrap();
    let mut observation = core
        .observe_session(&key, ObservationInterests::questions(), Cancellation::new())
        .await
        .unwrap();
    // Drain the initial Loading view, then the observation ends fatally.
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
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
async fn a_host_connection_reports_connected_without_any_session() {
    // The app-level probe: no watch, no session, just the device transport.
    let (_dir, core, _access, listener) = setup(false).await;
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let snapshot = core.connect_host("host-1").await.unwrap();
    assert_eq!(snapshot.state, crate::runtime::ConnectionState::Connected);
    assert_eq!(snapshot.code, None);
    core.disconnect_host("host-1");
    assert_eq!(
        core.connection_snapshot("host-1")
            .map(|snapshot| snapshot.state),
        Some(crate::runtime::ConnectionState::Disconnected)
    );
    peer.abort();
}

#[tokio::test]
async fn a_deterministic_watch_failure_keeps_the_host_connection() {
    // A session-scoped refusal is not a transport outage: the Host connection
    // must still report Connected so the app can explain the session instead of
    // claiming the network is down.
    let (_dir, core, access, listener) = setup(false).await;
    access.host.lock().unwrap().version_check_error =
        Some(whalerein_proto::ErrorCode::PayloadTooLarge);
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
        while observation.next_event().await.is_some() {}
    })
    .await;
    assert_eq!(
        core.connection_snapshot(&key.host_id)
            .map(|snapshot| snapshot.state),
        Some(crate::runtime::ConnectionState::Connected),
        "the Host connection outlives a session-scoped watch failure"
    );
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
    // The watch attaches asynchronously; the connection reports Connected only
    // after the broker acknowledged its subscriptions.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while core
        .connection_snapshot(&key.host_id)
        .map(|snapshot| snapshot.state)
        .unwrap_or(ConnectionState::Disconnected)
        != ConnectionState::Connected
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the host connection never became usable"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop_tx.send(()).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if core
            .connection_snapshot(&key.host_id)
            .map(|snapshot| snapshot.state)
            .unwrap_or(ConnectionState::Disconnected)
            != ConnectionState::Connected
        {
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

async fn drain_terminal_error(observation: &mut SessionObservation) -> CoreError {
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
        while observation.next_event().await.is_some() {}
    })
    .await
    .is_ok();
    assert!(ended, "a protocol violation must end the observation");
    observation
        .take_terminal_error()
        .expect("terminal protocol/security error is available")
}

#[tokio::test]
async fn session_observation_live_preview_is_accepted_from_the_live_topic_without_ack() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let (no_ack_tx, no_ack_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(
            &mut stream,
            delta_event(&peer_core, "session-1", "watch-1", 1, "live"),
        )
        .await;
        // Nothing in Core acknowledges, retries or publishes for a live
        // preview. Any packet here would be an unexpected publication.
        let mut buffer = BytesMut::new();
        let packet = tokio::time::timeout(
            Duration::from_millis(300),
            read_packet(&mut stream, &mut buffer),
        )
        .await;
        let _ = no_ack_tx.send(packet.is_err());
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let mut preview = false;
    for _ in 0..20 {
        let event = next_event(&mut observation).await;
        if let SessionEventPayload::Conversation(ViewUpdate::Preview { text, .. }) = event.payload {
            assert_eq!(text, "live");
            preview = true;
            break;
        }
    }
    assert!(preview, "a live frame from the live topic is accepted");
    assert!(
        no_ack_rx.await.expect("peer observed the live frame"),
        "no ACK or retry is published for live delivery"
    );
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_rejects_a_live_frame_on_the_control_topic() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish_control(
            &mut stream,
            delta_event(&peer_core, "session-1", "watch-1", 1, "crossed"),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let error = drain_terminal_error(&mut observation).await;
    assert_eq!(error.code(), "security_error");
    assert!(
        error.to_string().contains("control event topic"),
        "the crossed live frame is rejected, not accepted through a fallback: {error}"
    );
    peer.abort();
}

#[tokio::test]
async fn session_observation_rejects_a_control_payload_on_the_live_topic() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        // A committed/control state update is control-class intent. Delivering
        // it on the live topic is a protocol violation, not a preview.
        publish(
            &mut stream,
            state_event(&peer_core, "session-1", "watch-1", 2),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let error = drain_terminal_error(&mut observation).await;
    assert_eq!(error.code(), "security_error");
    assert!(
        error.to_string().contains("control payload"),
        "a non-live payload on the live topic is rejected: {error}"
    );
    peer.abort();
}

#[tokio::test]
async fn session_observation_live_gap_marks_incomplete_without_failing_control() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(
            &mut stream,
            live_frame_event(
                &peer_core,
                "session-1",
                "watch-1",
                1,
                whalerein_proto::live_frame::Payload::Started(whalerein_proto::AttemptStarted {}),
            ),
        )
        .await;
        publish(
            &mut stream,
            delta_event(&peer_core, "session-1", "watch-1", 2, "a"),
        )
        .await;
        // Sequence 3 is lost; the next preview cannot be contiguous.
        publish(
            &mut stream,
            delta_event(&peer_core, "session-1", "watch-1", 4, "b"),
        )
        .await;
        // Control must still be delivered on the same connection after the
        // live gap: no reconnect, retry or block occurs.
        publish_control(
            &mut stream,
            state_event(&peer_core, "session-1", "watch-1", 2),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let mut first = false;
    let mut gap = false;
    let mut control_seen = false;
    for _ in 0..30 {
        let event = next_event(&mut observation).await;
        match event.payload {
            SessionEventPayload::Conversation(ViewUpdate::Preview {
                text, incomplete, ..
            }) if text == "a" && !incomplete => first = true,
            SessionEventPayload::Conversation(ViewUpdate::Preview {
                text, incomplete, ..
            }) if text == "b" && incomplete => gap = true,
            SessionEventPayload::Conversation(ViewUpdate::State(state)) => {
                control_seen = state.revision == 2
            }
            _ => {}
        }
        if first && gap && control_seen {
            break;
        }
    }
    assert!(first, "the contiguous preview is complete");
    assert!(gap, "a live gap marks only the preview incomplete");
    assert!(
        control_seen,
        "control delivery is unaffected by the live gap"
    );
    observation.close().await;
    peer.abort();
}

#[tokio::test]
async fn session_observation_committed_message_repairs_a_gapped_preview() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let cursor = core
        .sync_service()
        .unwrap()
        .cursor(&key)
        .unwrap()
        .expect("synced history cursor");
    let batch = whalerein_proto::HistoryBatch {
        from_cursor: cursor,
        to_cursor: "bookmark:repair".into(),
        batch_id: "batch-repair".into(),
        projection_id: "projection-1".into(),
        changes: vec![whalerein_proto::MessageUpsert {
            message: Some(crate::api::test_support::history_message(
                "message-repair",
                "committed",
            )),
            origin: None,
        }],
    };
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        publish(
            &mut stream,
            live_frame_event(
                &peer_core,
                "session-1",
                "watch-1",
                1,
                whalerein_proto::live_frame::Payload::Started(whalerein_proto::AttemptStarted {}),
            ),
        )
        .await;
        publish(
            &mut stream,
            delta_event(&peer_core, "session-1", "watch-1", 3, "partial"),
        )
        .await;
        // The committed control message supersedes and repairs the preview.
        publish_control(
            &mut stream,
            history_event(&peer_core, "session-1", "watch-1", batch),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let mut gapped = false;
    let mut repaired = false;
    for _ in 0..30 {
        let event = next_event(&mut observation).await;
        match event.payload {
            SessionEventPayload::Conversation(ViewUpdate::Preview { incomplete, .. })
                if incomplete =>
            {
                gapped = true
            }
            SessionEventPayload::Conversation(ViewUpdate::Message { message, .. })
                if message.message_id == "message-repair" =>
            {
                repaired = true
            }
            _ => {}
        }
        if gapped && repaired {
            break;
        }
    }
    assert!(gapped, "the preview is incomplete after the gap");
    assert!(repaired, "the committed control message repairs the view");
    observation.close().await;
    peer.abort();
}

#[tokio::test]
#[ignore = "pre-existing MQTT harness startup race; flaky on loaded hosts"]
async fn session_observation_rejects_an_epoch_three_event() {
    let (_dir, core, _access, listener) = setup(true).await;
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let mut event = delta_event(&peer_core, "session-1", "watch-1", 1, "old");
        event.relay_version = crate::access::transport::RELAY_EPOCH_V5 - 1;
        // The one-shot event can race the observer's subscription on a loaded
        // host. Repeating the same protocol violation keeps the fixture
        // deterministic; the first received copy still ends the observation.
        for _ in 0..20 {
            publish_control(&mut stream, event.clone()).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let error = drain_terminal_error(&mut observation).await;
    assert_eq!(error.code(), "protocol_version_incompatible");
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
    // An explicit authority close is terminal even when the transport is up.
    assert!(watch_fatal(&CoreError::AuthorityClosed(
        "host_removed".into()
    )));
    // A transport-shaped failure remains reconnectable.
    assert!(!watch_fatal(&CoreError::Unavailable("mqtt down".into())));
    assert!(!watch_fatal(&CoreError::Timeout));
}

#[tokio::test]
async fn a_deterministic_lease_failure_ends_the_watch_instead_of_reconnecting() {
    // An oversized or malformed renewal is guaranteed to fail again. The watch
    // must report the real cause once, not loop through reconnects that look
    // like a network problem.
    let (_dir, core, access, listener) = setup(false).await;
    access.host.lock().unwrap().version_check_error =
        Some(whalerein_proto::ErrorCode::PayloadTooLarge);
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        // Outlive the lease tick: this case is about a deterministic refusal,
        // not about a transport loss competing with it.
        tokio::time::sleep(Duration::from_secs(15)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let handshakes = access.host.lock().unwrap().channel_handshake_steps;
    // The first lease tick fires five seconds after the watch is ready.
    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        while observation.next_event().await.is_some() {}
    })
    .await
    .is_ok();
    assert!(
        ended,
        "a deterministic lease failure closes the observation"
    );
    let error = observation
        .take_terminal_error()
        .expect("the deterministic cause is available");
    assert_eq!(error.code(), "payload_too_large");
    assert_eq!(
        access.host.lock().unwrap().channel_handshake_steps,
        handshakes,
        "a deterministic lease failure must not rebuild the connection"
    );
    peer.abort();
}

#[tokio::test]
async fn a_transient_lease_failure_keeps_the_stream_open() {
    // A failed renewal changes nothing while the lease still has room: the
    // same connection retries on the next tick.
    let (_dir, core, access, listener) = setup(false).await;
    access.host.lock().unwrap().version_check_error = Some(whalerein_proto::ErrorCode::Unavailable);
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        tokio::time::sleep(Duration::from_secs(8)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let handshakes = access.host.lock().unwrap().channel_handshake_steps;
    // The first tick is a transient failure; the observation stays open and
    // the connection stays connected instead of reconnecting.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(
        core.connection_snapshot(&key.host_id)
            .map(|snapshot| snapshot.state)
            .unwrap_or(ConnectionState::Disconnected),
        crate::runtime::ConnectionState::Connected,
        "a transient renewal failure must not mark the host as reconnecting"
    );
    assert_eq!(
        access.host.lock().unwrap().channel_handshake_steps,
        handshakes,
        "a transient renewal failure must not rebuild the connection"
    );
    observation.close().await;
    peer.abort();
}

// ---------------------------------------------------------------------------
// Approvals reliability and lease-refusal classification (CORE-1 / CORE-2)
// ---------------------------------------------------------------------------

fn pending_approval(approval_id: &str) -> whalerein_proto::Approval {
    whalerein_proto::Approval {
        approval_id: approval_id.into(),
        session_id: "session-1".into(),
        tool_name: "shell".into(),
        call_id: "call-1".into(),
        reason: "run a command".into(),
        digest: vec![7u8; 32],
        version: 1,
        expires_at_unix_ms: 9_000_000_000_000,
        state: whalerein_proto::ApprovalState::Pending as i32,
        decision: whalerein_proto::ApprovalDecision::Unspecified as i32,
        ..Default::default()
    }
}

/// A transient failure of the per-state approvals query must not present an
/// empty pending set for a session whose approval the viewer already saw. The
/// state-event path keeps the last emitted approvals and retries the bounded
/// read on the next lease tick instead of publishing an empty list for a
/// revision the lease marker has already passed.
#[tokio::test]
async fn a_transient_approvals_read_failure_keeps_visible_pending_approvals() {
    let (_dir, core, access, listener) = setup(false).await;
    access.host.lock().unwrap().approvals = vec![pending_approval("approval-1")];
    let key = core.session_key("session-1").await.unwrap();
    let peer_core = core.clone();
    let (go_tx, go_rx) = tokio::sync::oneshot::channel();
    let mut go_tx = Some(go_tx);
    let peer = tokio::spawn(async move {
        let mut stream = subscribe(&listener).await;
        let _ = go_rx.await;
        // A normal transition event with a newer revision: Core reads the
        // pending approvals through ApprovalList when it applies it.
        publish_control(
            &mut stream,
            state_event(&peer_core, "session-1", "watch-1", 2),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let mut baseline = None;
    for _ in 0..10 {
        if let SessionEventPayload::Conversation(ViewUpdate::State(state)) =
            next_event(&mut observation).await.payload
        {
            baseline = Some(state.approvals.len());
            break;
        }
    }
    assert_eq!(
        baseline,
        Some(1),
        "the watch baseline exposes the pending approval read from the Host"
    );
    // The next bounded approvals read fails transiently (a network blip), while
    // the Host delivers a newer state revision.
    access.host.lock().unwrap().query_error_once = Some(whalerein_proto::ErrorCode::Unavailable);
    let _ = go_tx.take().expect("go sender").send(());
    let mut delivered = None;
    for _ in 0..10 {
        if let SessionEventPayload::Conversation(ViewUpdate::State(state)) =
            next_event(&mut observation).await.payload
        {
            if state.revision == 2 {
                delivered = Some(state.approvals.len());
                break;
            }
        }
    }
    assert_eq!(
        delivered,
        Some(1),
        "a failed approvals read must not erase the pending approval the viewer must answer"
    );
    observation.close().await;
    peer.abort();
}

/// WIRE.md says a deterministic refusal ends the observation with its own code
/// instead of reconnecting. A Host that does not know the renewal payload
/// rejects it with `invalid_argument` ("RequestPayload carries no known payload
/// variant"), which is a `fix_input` code and therefore terminal. An earlier
/// classifier matched the non-existent code string `invalid_input`, so this
/// refusal was retried until the caller's deadline hid the real cause.
#[tokio::test]
async fn a_malformed_lease_refusal_ends_the_watch_with_its_real_code() {
    let (_dir, core, access, listener) = setup(false).await;
    access.host.lock().unwrap().version_check_error =
        Some(whalerein_proto::ErrorCode::InvalidArgument);
    let key = core.session_key("session-1").await.unwrap();
    let peer = tokio::spawn(async move {
        let _stream = subscribe(&listener).await;
        // Hold the subscription so the retry loop stays on a live connection.
        tokio::time::sleep(Duration::from_secs(25)).await;
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    // The first lease tick fires five seconds after attach, so three ticks fit
    // in this window when the refusal is (wrongly) treated as transient.
    let ended = tokio::time::timeout(Duration::from_secs(12), async {
        while observation.next_event().await.is_some() {}
    })
    .await
    .is_ok();
    assert!(
        ended,
        "a deterministic invalid_argument refusal must end the observation with its real code"
    );
    assert_eq!(
        observation
            .take_terminal_error()
            .expect("the deterministic cause is available")
            .code(),
        "invalid_argument"
    );
    peer.abort();
}

fn history_rows(count: usize) -> Vec<whalerein_proto::ConversationMessage> {
    (1..=count)
        .map(|index| crate::api::test_support::history_message(&format!("row-{index}"), "fixture"))
        .collect()
}

fn history_position(index: usize) -> String {
    format!("position:{index}")
}

fn set_source_history(access: &FakeAccess, count: usize) {
    let mut host = access.host.lock().unwrap();
    host.history_messages = history_rows(count);
    host.history_positions = (0..=count)
        .map(|index| (history_position(index), index))
        .collect();
    host.history_cut = history_position(count);
}

async fn setup_paged_observation() -> (tempfile::TempDir, Core, FakeAccess, SessionKey) {
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
    // The FakeBroker gates the reply lane while still delivering control
    // notifications and other query replies.
    core.use_mqtt_queries();
    core.sessions("host-1").await.unwrap();
    let key = SessionKey::new("host-1", "session-1").unwrap();
    (dir, core, access, key)
}

fn publish_committed(core: &Core, access: &FakeAccess, index: usize) {
    let batch = whalerein_proto::HistoryBatch {
        from_cursor: history_position(index - 1),
        to_cursor: history_position(index),
        batch_id: format!("batch-{index}"),
        projection_id: "projection-1".into(),
        changes: vec![whalerein_proto::MessageUpsert {
            message: Some(crate::api::test_support::history_message(
                &format!("row-{index}"),
                "fixture",
            )),
            origin: None,
        }],
    };
    access
        .broker
        .publish_event(&history_event(core, "session-1", "watch-1", batch));
}

async fn next_history_snapshot(
    observation: &mut SessionObservation,
) -> crate::api::HistorySnapshot {
    loop {
        match next_event(observation).await.payload {
            SessionEventPayload::HistorySnapshot(snapshot) => return snapshot,
            SessionEventPayload::HistoryUnavailable { code } => {
                panic!("history unavailable: {code}")
            }
            _ => {}
        }
    }
}

fn snapshot_ids(snapshot: &crate::api::HistorySnapshot) -> Vec<&str> {
    snapshot
        .page
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect()
}

async fn wait_history_request(started: tokio::sync::oneshot::Receiver<()>, access: &FakeAccess) {
    match tokio::time::timeout(Duration::from_secs(5), started).await {
        Ok(Ok(())) => {}
        outcome => {
            let host = access.host.lock().unwrap();
            panic!(
                "pinned read did not enter the gate: {outcome:?}; reads: {}; gate pending: {}",
                host.history_reads,
                host.history_reverse_gate.is_some()
            );
        }
    }
}

#[tokio::test]
async fn paged_observation_cold_cut_keeps_questions_live_and_does_not_certify_full_history() {
    use crate::api::test_support::HistoryReadGate;
    use crate::history::{HistoryPageRequest, HistorySource};
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 2);
    let (entered, request_entered) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    access.host.lock().unwrap().history_reverse_gate = Some(HistoryReadGate {
        entered,
        release: held,
    });
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(1, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();
    wait_history_request(request_entered, &access).await;
    let mut running_state = false;
    let mut question_ready = false;
    while !running_state || !question_ready {
        match next_event(&mut observation).await.payload {
            SessionEventPayload::HistorySnapshot(_)
            | SessionEventPayload::HistoryUnavailable { .. } => {
                panic!("cold history was not pending while holding its response");
            }
            SessionEventPayload::Conversation(ViewUpdate::State(_)) => running_state = true,
            SessionEventPayload::Questions(view)
                if view.freshness == QuestionFreshness::Current =>
            {
                question_ready = true;
            }
            _ => {}
        }
    }
    // Queue the pre-cut replay and a post-cut commit ahead of the Questions
    // marker. Receipt of that facet confirms the watch processed both events
    // while the reverse reply was still held.
    set_source_history(&access, 3);
    publish_committed(&core, &access, 2);
    publish_committed(&core, &access, 3);

    let newer = descriptor_with(2);
    {
        let mut host = access.host.lock().unwrap();
        host.question_descriptor = newer.clone();
        host.question_pages.push_back(proto_page(
            2,
            "",
            true,
            vec![proto_batch("session-1", "revision-2")],
        ));
    }
    access
        .broker
        .publish_event(&question_event(&core, "session-1", "watch-1", newer));
    let questions = loop {
        match next_event(&mut observation).await.payload {
            SessionEventPayload::Questions(view)
                if view.stamp.as_ref().is_some_and(|stamp| stamp.revision == 2)
                    && view.freshness == QuestionFreshness::Current =>
            {
                break view
            }
            SessionEventPayload::HistorySnapshot(_)
            | SessionEventPayload::HistoryUnavailable { .. }
            | SessionEventPayload::Conversation(ViewUpdate::Message { .. }) => {
                panic!("history escaped the held baseline");
            }
            _ => {}
        }
    };
    assert_eq!(questions.batches[0].batch_id, "revision-2");

    release.send(()).unwrap();

    let first = next_history_snapshot(&mut observation).await;
    assert_eq!(first.page.source, HistorySource::Online);
    assert_eq!(snapshot_ids(&first), ["row-2"]);
    let continuation = first.page.continuation.clone().expect("older cut");
    loop {
        match next_event(&mut observation).await.payload {
            SessionEventPayload::Conversation(ViewUpdate::Message { message, .. }) => {
                assert_eq!(message.message_id, "row-3");
                break;
            }
            SessionEventPayload::HistorySnapshot(_) => panic!("unexpected replacement"),
            _ => {}
        }
    }
    let older = core
        .history_page(&HistoryPageRequest::new(key.clone()).continuation(continuation))
        .await
        .unwrap();
    assert_eq!(
        older
            .messages
            .iter()
            .map(|m| m.message_id.as_str())
            .collect::<Vec<_>>(),
        ["row-1"]
    );
    assert!(older.complete);

    // Deliver an actual reconnect after a committed event is lost. Recovery
    // uses an incremental read, leaving the established reverse cut alone.
    set_source_history(&access, 4);
    let reads_before_reconnect = access.host.lock().unwrap().history_reads;
    core.disconnect_host(&key.host_id);
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match observation
                .next_event()
                .await
                .expect("observation ended during reconnect")
                .payload
            {
                SessionEventPayload::Conversation(ViewUpdate::Message { message, .. }) => {
                    assert_eq!(message.message_id, "row-4");
                    break;
                }
                SessionEventPayload::HistorySnapshot(_) => {
                    panic!("continuous reconnect replaced the window")
                }
                SessionEventPayload::HistoryUnavailable { code } => {
                    panic!("connection interruption invalidated readable history: {code}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("idle observation did not recover before its lease check");
    assert_eq!(
        access.host.lock().unwrap().history_reads,
        reads_before_reconnect + 1,
        "continuous reconnect reads only the missing forward range"
    );
    observation.close().await;

    let cache = core.cache().unwrap();
    assert!(cache.history_view(&key).unwrap().is_none());
    let recent = cache.recent_history(&key).unwrap().unwrap();
    assert_eq!(recent.cut_cursor, history_position(4));
    assert_eq!(
        recent.batch.unwrap().changes[0]
            .message
            .as_ref()
            .unwrap()
            .message_id,
        "row-4"
    );

    // Exercise the normal cache-matching sync path on the same Core. With
    // only recent coverage it must build the complete prefix from scratch.
    core.sync_conversation(
        &key,
        &mut Conversation::default(),
        false,
        false,
        "projection-1",
        &mut crate::api::core::watch::AcceptAll,
    )
    .await
    .unwrap();
    assert_eq!(
        cache.history_message_ids(&key).unwrap(),
        ["row-1", "row-2", "row-3", "row-4"]
    );
}

#[tokio::test]
async fn paged_observation_retains_disjoint_cache_until_forward_proof_or_explicit_replacement() {
    use crate::api::{test_support::HistoryReadGate, HistoryContinuity};
    use crate::history::HistorySource;
    for bridge in [true, false] {
        let (_dir, core, access, key) = setup_paged_observation().await;
        set_source_history(&access, 2);
        core.sync_session(&key).await.unwrap();
        let cache = core.cache().unwrap();
        let full_view = cache.history_view(&key).unwrap();
        set_source_history(&access, 6);

        let (entered, request_entered) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        {
            let mut host = access.host.lock().unwrap();
            host.history_reverse_gate = Some(HistoryReadGate {
                entered,
                release: held,
            });
            host.history_forward_error =
                (!bridge).then_some(whalerein_proto::ErrorCode::SnapshotRequired);
        }
        let mut observation = core
            .observe_session(
                &key,
                ObservationInterests::paged_conversation(2, 256 * 1024),
                Cancellation::new(),
            )
            .await
            .unwrap();
        let warm = next_history_snapshot(&mut observation).await;
        assert_eq!(warm.page.source, HistorySource::Cache);
        assert_eq!(snapshot_ids(&warm), ["row-1", "row-2"]);
        wait_history_request(request_entered, &access).await;

        set_source_history(&access, 7);
        publish_committed(&core, &access, 7);
        release.send(()).unwrap();
        let mut merged = Vec::new();
        loop {
            match next_event(&mut observation).await.payload {
                SessionEventPayload::Conversation(ViewUpdate::Message { message, .. }) => {
                    merged.push(message.message_id)
                }
                SessionEventPayload::HistorySnapshot(snapshot) => {
                    assert_eq!(snapshot_ids(&snapshot), ["row-5", "row-6"]);
                    assert_eq!(
                        snapshot.continuity,
                        if bridge {
                            HistoryContinuity::Continuous
                        } else {
                            HistoryContinuity::Replacement
                        }
                    );
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(
            merged,
            if bridge {
                vec!["row-3", "row-4", "row-5", "row-6"]
            } else {
                Vec::new()
            }
        );
        loop {
            if let SessionEventPayload::Conversation(ViewUpdate::Message { message, .. }) =
                next_event(&mut observation).await.payload
            {
                assert_eq!(message.message_id, "row-7");
                break;
            }
        }
        // Installing the recent window and processing further live changes
        // must neither rewrite nor advance independently certified coverage.
        assert_eq!(cache.history_view(&key).unwrap(), full_view);
        assert_eq!(cache.history_message_ids(&key).unwrap(), ["row-1", "row-2"]);
        observation.close().await;
    }
}

#[tokio::test]
async fn paged_observation_explicit_read_failure_retains_cache_and_questions() {
    use crate::history::HistorySource;
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 1);
    core.sync_session(&key).await.unwrap();
    access.host.lock().unwrap().history_reverse_error =
        Some(whalerein_proto::ErrorCode::Unavailable);
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(1, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let warm = next_history_snapshot(&mut observation).await;
    assert_eq!(warm.page.source, HistorySource::Cache);
    assert_eq!(snapshot_ids(&warm), ["row-1"]);
    let mut unavailable = false;
    let mut questions = false;
    while !unavailable || !questions {
        match next_event(&mut observation).await.payload {
            SessionEventPayload::HistoryUnavailable { code } => {
                assert_eq!(code, "unavailable");
                unavailable = true;
            }
            SessionEventPayload::Questions(view)
                if view.freshness == QuestionFreshness::Current =>
            {
                questions = true
            }
            SessionEventPayload::HistorySnapshot(_) => {
                panic!("failed read replaced retained history")
            }
            _ => {}
        }
    }
    observation.close().await;
}

#[tokio::test]
async fn paged_observation_recent_only_offline_cache_is_bounded_and_incomplete() {
    use crate::history::HistorySource;
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 4);
    let mut online = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(3, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let page = next_history_snapshot(&mut online).await;
    assert_eq!(page.page.source, HistorySource::Online);
    assert_eq!(snapshot_ids(&page), ["row-2", "row-3", "row-4"]);
    assert!(page.page.continuation.is_some());
    online.close().await;

    access.host.lock().unwrap().host_online = false;
    let mut offline = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(2, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let page = next_history_snapshot(&mut offline).await;
    assert_eq!(page.page.source, HistorySource::Cache);
    assert_eq!(snapshot_ids(&page), ["row-3", "row-4"]);
    assert!(!page.page.complete);
    assert!(page.page.continuation.is_none());
    assert!(core.cache().unwrap().history_view(&key).unwrap().is_none());
    offline.close().await;
}

#[tokio::test]
async fn paged_observation_source_complete_empty_is_explicit() {
    use crate::history::HistorySource;
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 0);
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(5, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let page = next_history_snapshot(&mut observation).await;
    assert_eq!(page.page.source, HistorySource::Online);
    assert!(page.page.messages.is_empty());
    assert!(page.page.complete);
    assert!(page.page.continuation.is_none());
    observation.close().await;
}

#[tokio::test]
async fn paged_observation_empty_source_page_preserves_a_progressing_older_cut() {
    use crate::history::{HistoryPageRequest, HistorySource};
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 2);
    access.host.lock().unwrap().history_empty_first_page = true;
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(1, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();

    let first = next_history_snapshot(&mut observation).await;
    assert_eq!(first.page.source, HistorySource::Online);
    assert!(first.page.messages.is_empty());
    assert!(!first.page.complete);
    let continuation = first
        .page
        .continuation
        .expect("incomplete window has an older cut");
    let older = core
        .history_page(&HistoryPageRequest::new(key).continuation(continuation))
        .await
        .unwrap();
    assert_eq!(
        older
            .messages
            .iter()
            .map(|row| row.message_id.as_str())
            .collect::<Vec<_>>(),
        ["row-1"]
    );
    assert!(older.complete);
    observation.close().await;
}

#[tokio::test]
async fn paged_observation_cancellation_fences_the_held_reply_on_reopen() {
    use crate::api::test_support::HistoryReadGate;
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 1);
    let (entered, request_entered) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    access.host.lock().unwrap().history_reverse_gate = Some(HistoryReadGate {
        entered,
        release: held,
    });

    let cancellation = Cancellation::new();
    let interests = ObservationInterests::paged_conversation(1, 256 * 1024);
    let mut old = core
        .observe_session(&key, interests, cancellation.clone())
        .await
        .unwrap();
    wait_history_request(request_entered, &access).await;
    let old_generation = old.generation();
    cancellation.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), old.next_event())
            .await
            .unwrap()
            .is_none()
    );
    assert!(core
        .cache()
        .unwrap()
        .recent_history(&key)
        .unwrap()
        .is_none());

    set_source_history(&access, 2);
    let mut replacement = core
        .observe_session(&key, interests, Cancellation::new())
        .await
        .unwrap();
    assert_ne!(replacement.generation(), old_generation);
    release.send(()).unwrap();
    let page = next_history_snapshot(&mut replacement).await;
    assert_eq!(snapshot_ids(&page), ["row-2"]);
    assert!(old.next_event().await.is_none());
    replacement.close().await;
}

#[tokio::test]
async fn paged_observation_active_projection_rotation_replaces_and_fences_old_pages() {
    use crate::api::{test_support::HistoryReadGate, HistoryContinuity};
    use crate::history::HistoryPageRequest;
    let (_dir, core, access, key) = setup_paged_observation().await;
    set_source_history(&access, 2);
    let mut observation = core
        .observe_session(
            &key,
            ObservationInterests::paged_conversation(1, 256 * 1024),
            Cancellation::new(),
        )
        .await
        .unwrap();
    let initial = next_history_snapshot(&mut observation).await;
    assert_eq!(initial.page.projection, "projection-1");
    assert_eq!(snapshot_ids(&initial), ["row-2"]);
    let old_continuation = initial.page.continuation.unwrap();

    let (entered, request_entered) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    set_source_history(&access, 3);
    {
        let mut host = access.host.lock().unwrap();
        host.history_projection = "projection-2".into();
        host.history_watch_projection = Some("projection-2".into());
        host.history_reverse_gate = Some(HistoryReadGate {
            entered,
            release: held,
        });
    }
    access.broker.publish_event(&state_event_with_projection(
        &core,
        "session-1",
        "watch-1",
        2,
        "projection-2",
    ));
    wait_history_request(request_entered, &access).await;
    let retained = core.cache().unwrap().recent_history(&key).unwrap().unwrap();
    assert_eq!(retained.projection_id, "projection-1");
    assert_eq!(
        core.history_page(&HistoryPageRequest::new(key).continuation(old_continuation))
            .await
            .unwrap_err()
            .code(),
        "snapshot_required"
    );
    release.send(()).unwrap();
    let replacement = next_history_snapshot(&mut observation).await;
    assert_eq!(replacement.page.projection, "projection-2");
    assert_eq!(replacement.continuity, HistoryContinuity::Replacement);
    assert_eq!(snapshot_ids(&replacement), ["row-3"]);
    observation.close().await;
}
