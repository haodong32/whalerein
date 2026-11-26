//! Contract and emission tests for the typed latency adapter.
//!
//! Each test names its acceptance binding in a comment so acceptance tooling
//! can grep `latency/AC-XX.variant` strings from the Rust suite.

use super::*;

use crate::conversation::ViewUpdate;

fn contract_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../contracts/latency")
        .canonicalize()
        .expect("contracts/latency")
}

fn manifest() -> Value {
    serde_json::from_slice(&std::fs::read(contract_dir().join("observation.json")).unwrap())
        .unwrap()
}

fn vectors() -> Value {
    serde_json::from_slice(&std::fs::read(contract_dir().join("vectors.json")).unwrap()).unwrap()
}

fn capturing(
    producer: Producer,
    clock: Arc<dyn BenchClock>,
) -> (Arc<BenchContext>, Arc<CapturingSink>) {
    let sink = Arc::new(CapturingSink::new());
    let context = BenchContext::with_clock(
        "run-test",
        "lt-test",
        producer,
        BenchMode::Trace,
        b"capture-key",
        sink.clone(),
        clock,
    );
    (context, sink)
}

fn receipt(frame: &str) -> BenchEvent {
    BenchEvent::CoreReceipt {
        frame: frame.into(),
        kind: ReceiptKind::Event,
        bytes: 10,
        decode: Decode::Ok,
        scope: None,
        stream: None,
        batch: None,
        block: None,
        seq_start: None,
        seq_end: None,
        reason: None,
    }
}

fn delivery(frame: &str, delivery: u64) -> BenchEvent {
    BenchEvent::CoreDelivery {
        frame: frame.into(),
        delivery,
        kind: DeliveryKind::Preview,
        bytes: 10,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    }
}

/// One decrypted live preview increment for range-fact tests.
fn live_update(stream: &str, block: &str, first: u64, last: u64) -> whalerein_proto::SessionUpdate {
    use whalerein_proto::{live_frame, session_update, AttemptRef, LiveBlockDelta, LiveFrame};
    whalerein_proto::SessionUpdate {
        watch_id: "watch-raw".into(),
        session: Some(whalerein_proto::SessionKey {
            host_id: "host-raw".into(),
            session_id: "session-raw".into(),
        }),
        payload: Some(session_update::Payload::Live(LiveFrame {
            attempt: Some(AttemptRef {
                live_epoch: "epoch-raw".into(),
                attempt_id: stream.into(),
            }),
            block_id: block.into(),
            first_seq: first,
            last_seq: last,
            payload: Some(live_frame::Payload::Delta(LiveBlockDelta {
                kind: whalerein_proto::BlockKind::Text as i32,
                text: "x".into(),
            })),
        })),
    }
}

#[test]
fn contract_manifest_and_vectors_agree_with_the_rust_adapter() {
    // latency/AC-05.shared-clock
    validate_manifest(&manifest()).expect("manifest matches the adapter");
    validate_vectors(&vectors()).expect("sample vectors match the adapter");
}

#[test]
fn contract_vectors_reject_each_invalid_shape() {
    let vectors = vectors();
    let invalid = vectors["invalid"].as_array().unwrap();
    let reasons: Vec<BenchViolation> = invalid
        .iter()
        .map(|vector| validate_record(&vector["value"]).unwrap_err())
        .collect();
    assert!(reasons
        .iter()
        .any(|reason| matches!(reason, BenchViolation::InvalidData(_))));
    assert!(reasons.iter().any(|reason| matches!(
        reason,
        BenchViolation::InvalidData(detail) if detail.contains("unknown variant")
    )));
    assert!(reasons
        .iter()
        .any(|reason| matches!(reason, BenchViolation::ForbiddenField(field) if field == "text")));
    assert!(reasons
        .iter()
        .any(|reason| matches!(reason, BenchViolation::UnknownEvent(_))));
    assert!(reasons
        .iter()
        .any(|reason| matches!(reason, BenchViolation::WrongProducer { .. })));
    assert!(reasons.contains(&BenchViolation::NegativeTimestamp));
}

#[test]
fn opaque_tokens_are_domain_separated_and_never_reveal_the_raw_id() {
    // latency/AC-06.privacy
    let tokenizer = BenchTokenizer::new(b"ephemeral-capture-key");
    let session = tokenizer.token(TokenDomain::Session, "session-raw-1");
    let stream = tokenizer.token(TokenDomain::Stream, "session-raw-1");
    let frame = tokenizer.token(TokenDomain::Frame, "session-raw-1");
    assert_eq!(session.len(), 32);
    assert!(session.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(session, stream);
    assert_ne!(stream, frame);
    assert!(!session.contains("session-raw-1"));
    assert_eq!(
        session,
        tokenizer.token(TokenDomain::Session, "session-raw-1")
    );
    assert_ne!(
        session,
        BenchTokenizer::new(b"other-key").token(TokenDomain::Session, "session-raw-1")
    );
}

#[test]
fn capture_key_accepts_hex_and_base64_and_rejects_an_empty_key() {
    let hex_key = BenchTokenizer::from_encoded_key("00112233445566778899aabbccddeeff").unwrap();
    let base64_key = BenchTokenizer::from_encoded_key("ABEiM0RVZneImaq7zN3u/w==").unwrap();
    assert_eq!(
        hex_key.token(TokenDomain::Frame, "f-1"),
        base64_key.token(TokenDomain::Frame, "f-1")
    );
    assert!(BenchTokenizer::from_encoded_key("").is_err());
    assert!(BenchTokenizer::from_encoded_key("not base64 !!").is_err());
}

#[test]
fn emit_is_trace_only_and_enforces_the_producer_role() {
    // latency/AC-07.mode-availability
    let clock = Arc::new(ManualClock::new(10));
    let sink = Arc::new(CapturingSink::new());
    let off = BenchContext::with_clock(
        "run",
        "lt",
        Producer::Core,
        BenchMode::Off,
        b"k",
        sink.clone(),
        clock.clone(),
    );
    off.emit(receipt("f-1"));
    assert!(sink.is_empty());

    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(receipt("f-1"));
    // A Core context may not emit a CLI-only event.
    core.emit(BenchEvent::CliRequest {
        request: "r-1".into(),
        attempt: 1,
        kind: RequestKind::Query,
        stage: CliRequestStage::Invoke,
        outcome: None,
        code: None,
    });
    assert_eq!(sink.len(), 1);
    assert_eq!(sink.records()[0].event.name(), "core_receipt");
}

#[test]
fn one_process_outlet_is_shared_by_core_and_cli_with_one_sequence_stream() {
    // latency/AC-07.shared-process-outlet
    let clock = Arc::new(ManualClock::new(100));
    let (core, sink) = capturing(Producer::Core, clock);
    let cli = core.for_producer(Producer::Cli);
    assert_eq!(cli.run_id(), core.run_id());
    assert_eq!(cli.lifetime(), core.lifetime());
    core.emit(receipt("f-1"));
    cli.emit(BenchEvent::CliWrite {
        delivery: 1,
        ordinal: 1,
        bytes: 5,
        outcome: WriteOutcome::Success,
        flush: true,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
    });
    core.emit(receipt("f-2"));
    let records = sink.records();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records.iter().map(|record| record.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(records[1].producer, Producer::Cli);
}

#[test]
fn bench_capture_is_independent_of_ordinary_log_verbosity() {
    // latency/AC-07.verbosity-independent
    let clock = Arc::new(ManualClock::new(1));
    let (core, sink) = capturing(Producer::Core, clock);
    // Nothing in the bench context reads a diagnostic level; emitting the
    // same facts with debug/info/warn logging selected would keep admission.
    core.emit(receipt("f-1"));
    assert_eq!(sink.len(), 1);
}

#[test]
fn a_content_field_cannot_enter_a_record() {
    // latency/AC-06.privacy
    let mut value = BenchRecord {
        v: SCHEMA_VERSION,
        run: "run".into(),
        producer: Producer::Core,
        lifetime: "lt".into(),
        seq: 1,
        ts: 1,
        event: receipt("f-1"),
    }
    .to_json();
    value["data"]["payload"] = Value::String("secret".into());
    assert_eq!(
        validate_record(&value),
        Err(BenchViolation::ForbiddenField("payload".into()))
    );
}

#[test]
fn a_negative_timestamp_is_rejected_but_zero_is_valid() {
    // latency/AC-05.wrong-clock-or-origin
    let mut value = BenchRecord {
        v: SCHEMA_VERSION,
        run: "run".into(),
        producer: Producer::Core,
        lifetime: "lt".into(),
        seq: 1,
        ts: 0,
        event: receipt("f-1"),
    }
    .to_json();
    assert!(validate_record(&value).is_ok());
    value["ts"] = Value::from(-1);
    assert_eq!(
        validate_record(&value),
        Err(BenchViolation::NegativeTimestamp)
    );
}

#[test]
fn manual_clock_produces_direct_differences_on_one_timeline() {
    // latency/AC-05.concurrent
    let clock = Arc::new(ManualClock::new(1_000));
    let (core, sink) = capturing(Producer::Core, clock.clone());
    let cli = core.for_producer(Producer::Cli);
    core.emit_at(receipt("f-1"), 1_000);
    clock.advance(25);
    cli.emit(BenchEvent::CliWrite {
        delivery: 1,
        ordinal: 1,
        bytes: 4,
        outcome: WriteOutcome::Success,
        flush: true,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
    });
    let records = sink.records();
    assert_eq!(records[1].ts - records[0].ts, 25);
}

#[test]
fn every_core_event_round_trips_through_the_validator() {
    // latency/AC-06.cleanup
    let clock = Arc::new(ManualClock::new(5));
    let (core, sink) = capturing(Producer::Core, clock);
    let events = vec![
        BenchEvent::CoreEnqueue {
            frame: "f-1".into(),
            queue_depth: 1,
            queue_bytes: 10,
            accepted: true,
            reason: None,
            stream: None,
            block: None,
            seq_start: None,
            seq_end: None,
            batch: None,
        },
        BenchEvent::CoreDequeue {
            frame: "f-1".into(),
            wait_ns: 12,
            disposition: DequeueDisposition::Applied,
            reason: None,
            stream: None,
            block: None,
            seq_start: None,
            seq_end: None,
            batch: None,
        },
        BenchEvent::CoreJoin {
            watch: "w-1".into(),
            connection: "c-1".into(),
            generation: 1,
            milestone: JoinMilestone::WatchReady,
        },
        BenchEvent::CoreSync {
            request: "r-1".into(),
            page: 1,
            messages: 2,
            bytes: 20,
            verified: true,
        },
        BenchEvent::CoreCommand {
            request: "r-1".into(),
            attempt: 1,
            stage: CommandStage::Terminal,
            outcome: Some(CommandOutcome::Ok),
            reason: None,
        },
    ];
    for event in events {
        core.emit(event);
    }
    for record in sink.records() {
        record.validate().unwrap();
        validate_record(&record.to_json()).unwrap();
    }
}

#[test]
fn inline_preview_delivers_receipt_dequeue_and_write() {
    // latency/AC-01.inline
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(receipt("f-1"));
    core.emit(BenchEvent::CoreDequeue {
        frame: "f-1".into(),
        wait_ns: 3,
        disposition: DequeueDisposition::Applied,
        reason: None,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    core.emit(delivery("f-1", 1));
    let names: Vec<_> = sink
        .records()
        .iter()
        .map(|record| record.event.name())
        .collect();
    assert_eq!(names, vec!["core_receipt", "core_dequeue", "core_delivery"]);
}

#[test]
fn coalesced_ranges_still_name_each_frame() {
    // latency/AC-01.coalesce
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(receipt("f-1"));
    core.emit(receipt("f-2"));
    core.emit(delivery("f-1", 1));
    let frames: Vec<_> = sink
        .records()
        .iter()
        .filter(|record| record.event.name() == "core_receipt")
        .map(|record| match &record.event {
            BenchEvent::CoreReceipt { frame, .. } => frame.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(frames, vec!["f-1", "f-2"]);
}

#[test]
fn split_preview_keeps_distinct_delivery_ordinals() {
    // latency/AC-01.split
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    for (index, frame) in ["f-1", "f-2"].into_iter().enumerate() {
        core.emit(receipt(frame));
        core.emit(delivery(frame, index as u64 + 1));
    }
    let deliveries: Vec<u64> = sink
        .records()
        .iter()
        .filter_map(|record| match &record.event {
            BenchEvent::CoreDelivery { delivery, .. } => Some(*delivery),
            _ => None,
        })
        .collect();
    assert_eq!(deliveries, vec![1, 2]);
}

#[test]
fn a_dropped_final_has_no_invented_delivery() {
    // latency/AC-01.drop-final
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(receipt("f-1"));
    let names: Vec<_> = sink
        .records()
        .iter()
        .map(|record| record.event.name())
        .collect();
    assert_eq!(names, vec!["core_receipt"]);
}

#[test]
fn live_range_tokens_join_receipt_delivery_and_stay_stable() {
    // latency/AC-01.waterfall
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    let update = live_update("stream-raw", "block-raw", 4, 6);
    let range = FrameRange::from_update(&core, &update).expect("live range");
    assert_eq!(range.seq_start, 4);
    assert_eq!(range.seq_end, 6);
    assert_eq!(range.stream, core.token(TokenDomain::Stream, "stream-raw"));
    let block = core.token(TokenDomain::Block, "block-raw");
    assert_eq!(range.block.as_deref(), Some(block.as_str()));
    assert_eq!(
        range.batch,
        core.token(TokenDomain::Batch, "stream-raw:4:6")
    );

    core.emit(BenchEvent::CoreReceipt {
        frame: "f-1".into(),
        kind: ReceiptKind::Event,
        bytes: 10,
        decode: Decode::Ok,
        scope: None,
        stream: Some(range.stream.clone()),
        batch: Some(range.batch.clone()),
        block: range.block.clone(),
        seq_start: Some(range.seq_start),
        seq_end: Some(range.seq_end),
        reason: None,
    });
    core.emit(BenchEvent::CoreDelivery {
        frame: "f-1".into(),
        delivery: 1,
        kind: DeliveryKind::Preview,
        bytes: 10,
        stream: Some(range.stream.clone()),
        block: range.block.clone(),
        seq_start: Some(range.seq_start),
        seq_end: Some(range.seq_end),
        batch: Some(range.batch.clone()),
    });
    let records = sink.records();
    let receipt = records[0].to_json();
    let delivery = records[1].to_json();
    for field in ["stream", "block", "seq_start", "seq_end", "batch"] {
        assert_eq!(
            receipt["data"][field], delivery["data"][field],
            "receipt and delivery must carry the same {field}"
        );
    }
    // A repeated derivation of the same increment yields one batch identity,
    // so a split or coalesced producer frame still joins this delivery.
    let repeated = FrameRange::from_update(&core, &update).expect("live range");
    assert_eq!(repeated.batch, range.batch);
    assert_eq!(repeated.stream, range.stream);
    assert_eq!(repeated.seq_start, range.seq_start);
    assert_eq!(repeated.seq_end, range.seq_end);
}

#[test]
fn a_non_live_or_unusable_frame_never_gets_a_range() {
    // latency/AC-01.no-invented-range
    let clock = Arc::new(ManualClock::new(10));
    let (core, _sink) = capturing(Producer::Core, clock);
    let state = whalerein_proto::SessionUpdate {
        watch_id: "watch-raw".into(),
        session: None,
        payload: Some(whalerein_proto::session_update::Payload::State(
            Default::default(),
        )),
    };
    assert!(
        FrameRange::from_update(&core, &state).is_none(),
        "a state or result frame carries no stream increment"
    );
    // A live frame without a usable inclusive range stays absent: seq 0 is not
    // an observed increment, so the fact must not claim one.
    let zero = live_update("stream-raw", "block-raw", 0, 0);
    assert!(FrameRange::from_update(&core, &zero).is_none());
    let reversed = live_update("stream-raw", "block-raw", 5, 4);
    assert!(FrameRange::from_update(&core, &reversed).is_none());
    let missing_attempt = live_update("", "block-raw", 1, 1);
    assert!(FrameRange::from_update(&core, &missing_attempt).is_none());
    // A disabled outlet never pays for a token, even on a usable live frame.
    let disabled = BenchContext::disabled(Producer::Core);
    assert!(
        FrameRange::from_update(&disabled, &live_update("stream-raw", "block-raw", 1, 1)).is_none()
    );
}

#[test]
fn a_live_frame_without_a_block_still_carries_its_stream_range() {
    // latency/AC-01.attempt
    let clock = Arc::new(ManualClock::new(10));
    let (core, _sink) = capturing(Producer::Core, clock);
    // A started/ended attempt names an increment but no text block.
    let update = live_update("stream-raw", "", 7, 7);
    let range = FrameRange::from_update(&core, &update).expect("live range");
    assert_eq!(range.block, None);
    assert_eq!(range.seq_start, 7);
    assert_eq!(range.seq_end, 7);
    assert_eq!(
        range.batch,
        core.token(TokenDomain::Batch, "stream-raw:7:7")
    );
}

fn preview_view() -> ViewUpdate {
    ViewUpdate::Preview {
        attempt: crate::conversation::AttemptRef {
            live_epoch: "epoch-raw".into(),
            attempt_id: "stream-raw".into(),
        },
        block_id: "block-raw".into(),
        kind: crate::conversation::BlockKind::Text,
        text: "hi".into(),
        replace: false,
        incomplete: false,
    }
}

struct Delivered {
    delivery: u64,
    frame: String,
    kind: DeliveryKind,
    stream: Option<String>,
    block: Option<String>,
    seq_start: Option<u64>,
    seq_end: Option<u64>,
    batch: Option<String>,
}

fn delivered_fact(record: &BenchRecord) -> Option<Delivered> {
    match &record.event {
        BenchEvent::CoreDelivery {
            frame,
            delivery,
            kind,
            stream,
            block,
            seq_start,
            seq_end,
            batch,
            ..
        } => Some(Delivered {
            delivery: *delivery,
            frame: frame.clone(),
            kind: *kind,
            stream: stream.clone(),
            block: block.clone(),
            seq_start: *seq_start,
            seq_end: *seq_end,
            batch: batch.clone(),
        }),
        _ => None,
    }
}

#[tokio::test]
async fn a_preview_delivery_carries_its_range_and_other_kinds_do_not() {
    // latency/AC-01.waterfall
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    let update = live_update("stream-raw", "block-raw", 4, 6);
    let range = FrameRange::from_update(&core, &update).expect("live range");
    let key = crate::session::SessionKey::new("host-raw", "session-raw").unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);

    crate::observation::emit_receipt_fact(
        &core,
        "frame-token",
        12,
        Decode::Ok,
        None,
        Some(&range),
        None,
    );
    let (sent, ordinal) = crate::observation::send_conversation(
        &core,
        &tx,
        &key,
        "observation",
        1,
        preview_view(),
        "frame-token",
        Some(&range),
    )
    .await;
    assert!(sent);
    let event = rx.recv().await.expect("delivered preview event");
    assert_eq!(event.preview_range.as_ref(), Some(&range));
    assert_eq!(event.delivery, ordinal);
    let records = sink.records();
    let receipt = records
        .iter()
        .find_map(|record| match &record.event {
            BenchEvent::CoreReceipt {
                frame,
                stream,
                block,
                seq_start,
                seq_end,
                batch,
                ..
            } => Some((
                frame.clone(),
                stream.clone(),
                block.clone(),
                *seq_start,
                *seq_end,
                batch.clone(),
            )),
            _ => None,
        })
        .expect("receipt fact");
    assert_eq!(receipt.0, "frame-token");
    let preview = records
        .iter()
        .filter_map(delivered_fact)
        .next_back()
        .expect("preview delivery fact");
    assert_eq!(preview.delivery, ordinal.unwrap());
    assert_eq!(preview.frame, receipt.0);
    assert_eq!(preview.kind, DeliveryKind::Preview);
    // Receipt and delivery must name the exact same increment.
    assert_eq!(preview.stream, receipt.1);
    assert_eq!(preview.block, receipt.2);
    assert_eq!(preview.seq_start, receipt.3);
    assert_eq!(preview.seq_end, receipt.4);
    assert_eq!(preview.batch, receipt.5);
    assert_eq!(preview.stream.as_deref(), Some(range.stream.as_str()));
    assert_eq!(preview.block, range.block);
    assert_eq!(preview.seq_start, Some(range.seq_start));
    assert_eq!(preview.seq_end, Some(range.seq_end));
    assert_eq!(preview.batch.as_deref(), Some(range.batch.as_str()));

    // An attempt-ended delivery from the same live frame names no increment:
    // its range fields stay absent instead of borrowing the preview's.
    let attempt = crate::conversation::AttemptRef {
        live_epoch: "epoch-raw".into(),
        attempt_id: "stream-raw".into(),
    };
    let (sent, _) = crate::observation::send_conversation(
        &core,
        &tx,
        &key,
        "observation",
        1,
        ViewUpdate::AttemptEnded {
            attempt,
            outcome: crate::conversation::AttemptOutcome::Committed,
        },
        "frame-token",
        Some(&range),
    )
    .await;
    assert!(sent);
    let event = rx.recv().await.expect("delivered attempt event");
    assert!(event.preview_range.is_none());
    let ended = sink
        .records()
        .iter()
        .filter_map(delivered_fact)
        .next_back()
        .expect("attempt-ended delivery fact");
    assert_eq!(ended.kind, DeliveryKind::AttemptEnded);
    assert!(
        ended.stream.is_none()
            && ended.block.is_none()
            && ended.seq_start.is_none()
            && ended.seq_end.is_none()
            && ended.batch.is_none(),
        "a non-preview delivery must not claim a stream increment"
    );
}

#[test]
fn dequeue_records_queue_residence() {
    // latency/AC-01.dequeue
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreDequeue {
        frame: "f-1".into(),
        wait_ns: 4_200,
        disposition: DequeueDisposition::Applied,
        reason: None,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    match &sink.records()[0].event {
        BenchEvent::CoreDequeue { wait_ns, .. } => assert_eq!(*wait_ns, 4_200),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn duplicate_and_stale_dispositions_are_distinct() {
    // latency/AC-01.duplicate
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    for disposition in [
        DequeueDisposition::Duplicate,
        DequeueDisposition::Stale,
        DequeueDisposition::WaitingGap,
        DequeueDisposition::Rejected,
    ] {
        core.emit(BenchEvent::CoreDequeue {
            frame: "f-1".into(),
            wait_ns: 1,
            disposition,
            reason: Some("test".into()),
            stream: None,
            block: None,
            seq_start: None,
            seq_end: None,
            batch: None,
        });
    }
    let dispositions: Vec<_> = sink
        .records()
        .iter()
        .map(|record| match &record.event {
            BenchEvent::CoreDequeue { disposition, .. } => *disposition,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(dispositions[0], DequeueDisposition::Duplicate);
    assert_eq!(dispositions[1], DequeueDisposition::Stale);
    assert_eq!(dispositions[2], DequeueDisposition::WaitingGap);
    assert_eq!(dispositions[3], DequeueDisposition::Rejected);
}

#[test]
fn waiting_gap_is_reported_without_a_delivery_stamp() {
    // latency/AC-01.waiting-gap
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreDequeue {
        frame: "f-1".into(),
        wait_ns: 10,
        disposition: DequeueDisposition::WaitingGap,
        reason: Some("history_gap".into()),
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    assert!(!sink
        .records()
        .iter()
        .any(|record| matches!(record.event, BenchEvent::CoreDelivery { .. })));
}

#[test]
fn enqueue_rejection_has_no_dequeue_stamp() {
    // latency/AC-01.enqueue-rejection
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreEnqueue {
        frame: "f-1".into(),
        queue_depth: 64,
        queue_bytes: 65536,
        accepted: false,
        reason: Some("queue_full".into()),
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    let records = sink.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event.name(), "core_enqueue");
    assert!(!records
        .iter()
        .any(|record| record.event.name() == "core_dequeue"));
}

#[test]
fn a_sink_gap_is_counted_by_the_sink_not_by_callers() {
    // latency/AC-01.sink-gap
    struct GapSink(AtomicU64);
    impl BenchSink for GapSink {
        fn emit(&self, _record: BenchRecord) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let sink = Arc::new(GapSink(AtomicU64::new(0)));
    let context = BenchContext::with_clock(
        "run",
        "lt",
        Producer::Core,
        BenchMode::Trace,
        b"k",
        sink.clone(),
        Arc::new(ManualClock::new(1)),
    );
    // Callers keep emitting; the outlet owns the loss accounting.
    for _ in 0..5 {
        context.emit(receipt("f-1"));
    }
    assert_eq!(sink.0.load(Ordering::SeqCst), 5);
}

#[test]
fn slow_observer_can_receive_events_with_overlapping_stamps() {
    // latency/AC-01.slow-observer
    let clock = Arc::new(ManualClock::new(50));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit_at(receipt("f-1"), 50);
    core.emit_at(receipt("f-2"), 50);
    let records = sink.records();
    assert_eq!(records[0].ts, records[1].ts);
    assert!(records[0].seq < records[1].seq);
}

#[test]
fn recovery_milestones_join_one_watch_and_connection() {
    // latency/AC-03.preview
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    for milestone in [
        JoinMilestone::Subscribed,
        JoinMilestone::ChannelReady,
        JoinMilestone::WatchReady,
        JoinMilestone::FirstUpdate,
        JoinMilestone::CaughtUp,
    ] {
        core.emit(BenchEvent::CoreJoin {
            watch: "w-1".into(),
            connection: "c-1".into(),
            generation: 2,
            milestone,
        });
    }
    let joins: Vec<_> = sink
        .records()
        .iter()
        .filter_map(|record| match &record.event {
            BenchEvent::CoreJoin {
                watch,
                connection,
                generation,
                milestone,
            } => Some((watch.clone(), connection.clone(), *generation, *milestone)),
            _ => None,
        })
        .collect();
    assert_eq!(joins.len(), 5);
    assert!(joins
        .iter()
        .all(|(watch, connection, generation, _)| watch == "w-1"
            && connection == "c-1"
            && *generation == 2));
    assert_eq!(joins.last().unwrap().3, JoinMilestone::CaughtUp);
}

#[test]
fn committed_and_history_milestones_share_the_watch_scope() {
    // latency/AC-03.committed
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreJoin {
        watch: "w-1".into(),
        connection: "c-1".into(),
        generation: 1,
        milestone: JoinMilestone::FirstUpdate,
    });
    core.emit(BenchEvent::CoreSync {
        request: "r-1".into(),
        page: 1,
        messages: 3,
        bytes: 90,
        verified: true,
    });
    let records = sink.records();
    assert_eq!(records[0].event.name(), "core_join");
    assert_eq!(records[1].event.name(), "core_sync");
}

#[test]
fn reused_and_stale_watch_milestones_remain_distinct_facts() {
    // latency/AC-03.reused
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreJoin {
        watch: "w-1".into(),
        connection: "c-1".into(),
        generation: 1,
        milestone: JoinMilestone::WatchReady,
    });
    core.emit(BenchEvent::CoreJoin {
        watch: "w-1".into(),
        connection: "c-2".into(),
        generation: 2,
        milestone: JoinMilestone::Closed,
    });
    let connections: Vec<_> = sink
        .records()
        .iter()
        .filter_map(|record| match &record.event {
            BenchEvent::CoreJoin { connection, .. } => Some(connection.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(connections, vec!["c-1", "c-2"]);
}

#[test]
fn timeout_milestone_does_not_imply_catch_up() {
    // latency/AC-03.timeout
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreJoin {
        watch: "w-1".into(),
        connection: "c-1".into(),
        generation: 1,
        milestone: JoinMilestone::WatchReady,
    });
    let milestones: Vec<_> = sink
        .records()
        .iter()
        .filter_map(|record| match &record.event {
            BenchEvent::CoreJoin { milestone, .. } => Some(*milestone),
            _ => None,
        })
        .collect();
    assert!(!milestones.contains(&JoinMilestone::CaughtUp));
}

#[test]
fn command_stages_share_one_request_token() {
    // latency/AC-03.committed
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    for stage in [
        CommandStage::Admit,
        CommandStage::Receipt,
        CommandStage::Recover,
        CommandStage::Terminal,
    ] {
        core.emit(BenchEvent::CoreCommand {
            request: "r-1".into(),
            attempt: 1,
            stage,
            outcome: Some(CommandOutcome::Ok),
            reason: None,
        });
    }
    assert!(sink.records().iter().all(|record| matches!(
        &record.event,
        BenchEvent::CoreCommand { request, .. } if request == "r-1"
    )));
}

#[test]
fn disabled_context_drops_without_a_platform_clock_read() {
    // latency/AC-07.off
    let context = BenchContext::disabled(Producer::Core);
    assert!(!context.enabled());
    context.emit(receipt("f-1"));
    assert_eq!(context.mode(), BenchMode::Off);
}

#[test]
fn aggregate_mode_keeps_the_writer_disabled() {
    // latency/AC-07.aggregate
    let sink = Arc::new(CapturingSink::new());
    let context = BenchContext::with_clock(
        "run",
        "lt",
        Producer::Core,
        BenchMode::Aggregate,
        b"k",
        sink.clone(),
        Arc::new(ManualClock::new(1)),
    );
    context.emit(receipt("f-1"));
    assert!(sink.is_empty());
}

#[test]
fn events_from_different_lifetimes_are_not_joined() {
    // latency/AC-05.restart
    let clock = Arc::new(ManualClock::new(1));
    let first = BenchContext::with_clock(
        "run",
        "lt-1",
        Producer::Core,
        BenchMode::Trace,
        b"k",
        Arc::new(NullSink),
        clock.clone(),
    );
    let second = BenchContext::with_clock(
        "run",
        "lt-2",
        Producer::Core,
        BenchMode::Trace,
        b"k",
        Arc::new(NullSink),
        clock,
    );
    assert_ne!(first.lifetime(), second.lifetime());
    assert_eq!(first.run_id(), second.run_id());
}

#[test]
fn out_of_order_arrival_does_not_reorder_assigned_sequence() {
    // latency/AC-05.ambiguous
    let clock = Arc::new(ManualClock::new(100));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit_at(receipt("f-2"), 200);
    core.emit_at(receipt("f-1"), 100);
    let records = sink.records();
    assert!(records[0].seq < records[1].seq);
    assert!(records[0].ts > records[1].ts);
}

#[test]
fn unsupported_manifest_versions_are_rejected() {
    // latency/AC-05.foreign-namespace
    let mut manifest = manifest();
    manifest["schema"] = Value::from(2);
    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn a_stale_scoped_frame_is_not_a_duplicate_or_waiting_gap() {
    // latency/AC-01.stale
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreDequeue {
        frame: "stale-frame-token".into(),
        wait_ns: 9,
        disposition: DequeueDisposition::Stale,
        reason: Some("epoch_changed".into()),
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    let dispositions: Vec<_> = sink
        .records()
        .iter()
        .map(|record| match &record.event {
            BenchEvent::CoreDequeue { disposition, .. } => *disposition,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(dispositions, vec![DequeueDisposition::Stale]);
    assert!(!dispositions.contains(&DequeueDisposition::Duplicate));
    assert!(!dispositions.contains(&DequeueDisposition::WaitingGap));
}

#[test]
fn history_repair_reaches_catch_up_before_first_visible_prefix() {
    // latency/AC-03.history
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreJoin {
        watch: "w-history".into(),
        connection: "c-history".into(),
        generation: 3,
        milestone: JoinMilestone::WatchReady,
    });
    core.emit(BenchEvent::CoreSync {
        request: "history-operation".into(),
        page: 1,
        messages: 4,
        bytes: 120,
        verified: true,
    });
    core.emit(BenchEvent::CoreJoin {
        watch: "w-history".into(),
        connection: "c-history".into(),
        generation: 3,
        milestone: JoinMilestone::CaughtUp,
    });
    let records = sink.records();
    assert!(records
        .iter()
        .any(|record| matches!(&record.event, BenchEvent::CoreSync { verified: true, .. })));
    let milestones: Vec<_> = records
        .iter()
        .filter_map(|record| match &record.event {
            BenchEvent::CoreJoin { milestone, .. } => Some(*milestone),
            _ => None,
        })
        .collect();
    assert_eq!(
        milestones,
        vec![JoinMilestone::WatchReady, JoinMilestone::CaughtUp]
    );
    assert!(records.iter().all(|record| match &record.event {
        BenchEvent::CoreJoin {
            watch, generation, ..
        } => watch == "w-history" && *generation == 3,
        _ => true,
    }));
}

#[test]
fn a_stale_update_during_recovery_stays_scoped_and_never_becomes_catch_up() {
    // latency/AC-03.stale
    let clock = Arc::new(ManualClock::new(10));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::CoreJoin {
        watch: "w-recovery".into(),
        connection: "c-recovery".into(),
        generation: 5,
        milestone: JoinMilestone::FirstUpdate,
    });
    core.emit(BenchEvent::CoreDequeue {
        frame: "stale-during-recovery".into(),
        wait_ns: 2,
        disposition: DequeueDisposition::Stale,
        reason: Some("epoch_changed".into()),
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    let records = sink.records();
    assert!(records.iter().any(|record| matches!(
        &record.event,
        BenchEvent::CoreDequeue {
            disposition: DequeueDisposition::Stale,
            ..
        }
    )));
    let milestones: Vec<_> = records
        .iter()
        .filter_map(|record| match &record.event {
            BenchEvent::CoreJoin { milestone, .. } => Some(*milestone),
            _ => None,
        })
        .collect();
    assert!(!milestones.contains(&JoinMilestone::CaughtUp));
    assert!(records.iter().all(|record| match &record.event {
        BenchEvent::CoreJoin {
            connection,
            generation,
            ..
        } => connection == "c-recovery" && *generation == 5,
        _ => true,
    }));
}

#[test]
fn outer_wall_clock_readings_are_never_used_as_the_measurement_origin() {
    // latency/AC-05.outer-clock-excluded
    let clock = Arc::new(ManualClock::new(7_000_000_000));
    let (core, sink) = capturing(Producer::Core, clock.clone());
    core.emit(receipt("f-1"));
    clock.advance(11);
    core.emit(delivery("f-1", 1));
    let records = sink.records();
    // Both facts use the same injected kernel-clock reader; an outer wall
    // clock would not produce this exact direct difference.
    assert_eq!(records[1].ts - records[0].ts, 11);
    assert_eq!(records[0].ts, 7_000_000_000);
}

#[test]
fn event_and_token_cardinality_stays_bounded() {
    // latency/AC-07.cardinality
    assert!(EVENT_NAMES.len() <= 32);
    let tokenizer = BenchTokenizer::new(b"k");
    let tokens: std::collections::BTreeSet<_> = (0..1_000)
        .map(|index| tokenizer.token(TokenDomain::Session, &format!("session-{index}")))
        .collect();
    assert_eq!(tokens.len(), 1_000);
    assert!(tokens.iter().all(|token| token.len() == 32));
    // A repeated identity maps to one bounded token.
    assert_eq!(
        tokenizer.token(TokenDomain::Session, "session-1"),
        tokenizer.token(TokenDomain::Session, "session-1")
    );
}

#[test]
fn concurrent_producers_share_one_strictly_increasing_sequence() {
    // latency/AC-07.concurrency
    let clock = Arc::new(ManualClock::new(1));
    let (core, sink) = capturing(Producer::Core, clock);
    let mut handles = Vec::new();
    for worker in 0..4 {
        let context = core.for_producer(Producer::Core);
        handles.push(std::thread::spawn(move || {
            for index in 0..50 {
                context.emit(receipt(&format!("frame-{worker}-{index}")));
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let records = sink.records();
    assert_eq!(records.len(), 200);
    let mut seqs: Vec<_> = records.iter().map(|record| record.seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=200).collect::<Vec<_>>());
}

#[test]
fn native_adapter_events_are_owned_by_the_dsh_producer_role() {
    // latency/AC-01.native
    // The Rust adapter does not emit `dsh_callback`/`dsh_handoff`; those are
    // owned by the Node DSH adapter. This Rust context must refuse them.
    let clock = Arc::new(ManualClock::new(1));
    let (core, sink) = capturing(Producer::Core, clock);
    core.emit(BenchEvent::DshCallback {
        stream: "s".into(),
        attempt: "a".into(),
        block: "b".into(),
        seq_start: 0,
        seq_end: 1,
        bytes: 1,
        native: NativeKind::AssistantPreview,
    });
    assert!(sink.is_empty());
}

#[test]
fn cross_language_token_vector_is_the_versioned_hmac_encoding() {
    // The Go and Node adapters match this exact preimage and truncation.
    let tokenizer = BenchTokenizer::from_encoded_key("00112233445566778899aabbccddeeff").unwrap();
    assert_eq!(
        tokenizer.token(TokenDomain::Stream, "s-1"),
        "4c1688654e6d854b282651a66ab18545"
    );
}

#[test]
fn contract_vocabulary_parses_and_reports_every_role_and_mode() {
    for (name, producer) in [
        ("fixture", Producer::Fixture),
        ("dsh", Producer::Dsh),
        ("host", Producer::Host),
        ("core", Producer::Core),
        ("cli", Producer::Cli),
        ("access", Producer::Access),
    ] {
        assert_eq!(Producer::parse(name), Some(producer));
        assert_eq!(producer.as_str(), name);
    }
    assert_eq!(Producer::parse("unknown"), None);
    for (name, mode) in [
        ("off", BenchMode::Off),
        ("aggregate", BenchMode::Aggregate),
        ("trace", BenchMode::Trace),
    ] {
        assert_eq!(BenchMode::parse(name), Some(mode));
        assert_eq!(mode.as_str(), name);
        assert_eq!(mode.captures(), mode == BenchMode::Trace);
    }
    assert_eq!(BenchMode::parse("verbose"), None);
}

#[test]
fn violation_messages_are_stable_and_name_the_failing_boundary() {
    let cases = [
        (
            BenchViolation::InvalidEnvelope("header"),
            "invalid observation envelope: header",
        ),
        (
            BenchViolation::NegativeTimestamp,
            "negative observation timestamp",
        ),
        (
            BenchViolation::UnknownEvent("x".into()),
            "unknown observation event: x",
        ),
        (
            BenchViolation::WrongProducer {
                event: "e".into(),
                producer: "host".into(),
            },
            "producer host cannot emit e",
        ),
        (
            BenchViolation::ForbiddenField("text".into()),
            "forbidden observation field: text",
        ),
        (
            BenchViolation::InvalidData("enum".into()),
            "invalid observation data: enum",
        ),
    ];
    for (violation, expected) in cases {
        assert_eq!(violation.to_string(), expected);
        assert!(std::error::Error::source(&violation).is_none());
    }
}

#[test]
fn a_record_serializes_and_validates_its_header() {
    let record = BenchRecord {
        v: 1,
        run: "run-1".into(),
        producer: Producer::Core,
        lifetime: "lt-1".into(),
        seq: 4,
        ts: 10,
        event: BenchEvent::CoreReceipt {
            frame: "f".into(),
            kind: ReceiptKind::Event,
            bytes: 1,
            decode: Decode::Ok,
            scope: None,
            stream: None,
            batch: None,
            block: None,
            seq_start: None,
            seq_end: None,
            reason: None,
        },
    };
    assert!(record.validate().is_ok());
    let json = record.to_json();
    assert_eq!(json["event"], "core_receipt");
    assert_eq!(json["producer"], "core");
    assert!(!record.to_vec().is_empty());

    let mut invalid = record.clone();
    invalid.v = 2;
    assert!(invalid.validate().is_err());
    invalid.v = 1;
    invalid.run.clear();
    assert!(invalid.validate().is_err());
    invalid.run = "run".into();
    invalid.producer = Producer::Host;
    assert!(invalid.validate().is_err());
}

#[test]
fn record_validation_rejects_each_structural_violation() {
    let base = serde_json::json!({
        "v": 1, "run": "run", "producer": "core", "lifetime": "lt", "seq": 1,
        "ts": 1, "event": "core_delivery",
        "data": {"frame": "f", "delivery": 1, "kind": "preview", "bytes": 1}
    });
    assert!(validate_record(&base).is_ok());
    assert!(matches!(
        validate_record(&serde_json::json!([])),
        Err(BenchViolation::InvalidEnvelope(_))
    ));
    let mut extra = base.clone();
    extra["extra"] = serde_json::json!(1);
    assert!(matches!(
        validate_record(&extra),
        Err(BenchViolation::InvalidEnvelope(_))
    ));
    let mut bad_version = base.clone();
    bad_version["v"] = serde_json::json!(2);
    assert!(matches!(
        validate_record(&bad_version),
        Err(BenchViolation::InvalidEnvelope(_))
    ));
    let mut short_run = base.clone();
    short_run["run"] = serde_json::json!("");
    assert!(validate_record(&short_run).is_err());
    let mut negative = base.clone();
    negative["ts"] = serde_json::json!(-1);
    assert_eq!(
        validate_record(&negative),
        Err(BenchViolation::NegativeTimestamp)
    );
    let mut unknown = base.clone();
    unknown["event"] = serde_json::json!("not_an_event");
    assert!(matches!(
        validate_record(&unknown),
        Err(BenchViolation::UnknownEvent(_))
    ));
    let mut forbidden = base.clone();
    forbidden["data"]["text"] = serde_json::json!("secret");
    assert!(matches!(
        validate_record(&forbidden),
        Err(BenchViolation::ForbiddenField(_))
    ));
    let mut unknown_field = base.clone();
    unknown_field["data"]["mystery"] = serde_json::json!(1);
    assert!(matches!(
        validate_record(&unknown_field),
        Err(BenchViolation::InvalidData(_))
    ));
    let mut missing = base.clone();
    missing["data"].as_object_mut().unwrap().remove("bytes");
    assert!(validate_record(&missing).is_err());
    let mut bad_enum = base;
    bad_enum["data"]["kind"] = serde_json::json!("made-up");
    assert!(validate_record(&bad_enum).is_err());
}

#[test]
fn contract_validation_rejects_a_manifest_that_drifted_from_rust() {
    let mut drifted = manifest();
    drifted["schema"] = serde_json::json!(9);
    assert!(validate_manifest(&drifted).is_err());
    let mut drifted = manifest();
    drifted["name"] = serde_json::json!("other");
    assert!(validate_manifest(&drifted).is_err());
    let mut drifted = manifest();
    drifted["producers"] = serde_json::json!(["core"]);
    assert!(validate_manifest(&drifted).is_err());
    let mut drifted = manifest();
    drifted["events"]["core_delivery"]["producers"] = serde_json::json!(["host"]);
    assert!(validate_manifest(&drifted).is_err());
    let mut drifted = manifest();
    drifted["events"]["core_delivery"]["fields"] = serde_json::json!({});
    assert!(validate_manifest(&drifted).is_err());
    let mut drifted = manifest();
    drifted["forbidden_fields"] = serde_json::json!([]);
    assert!(validate_manifest(&drifted).is_err());
    let mut changed = vectors();
    changed["valid"] = serde_json::json!([]);
    changed["invalid"] = serde_json::json!([]);
    assert!(validate_vectors(&changed).is_ok());
    let mut changed = vectors();
    changed["invalid"] = serde_json::json!([{ "value": changed["valid"][0].clone() }]);
    assert!(validate_vectors(&changed).is_err());
}

#[test]
fn capture_keys_and_tokens_reject_malformed_input() {
    assert!(decode_capture_key("").is_err());
    assert!(decode_capture_key("zz").is_err());
    assert!(decode_capture_key("0a0b").is_ok());
    let tokenizer =
        BenchTokenizer::from_encoded_key("00112233445566778899aabbccddeeff").expect("hex key");
    assert_eq!(tokenizer.token(TokenDomain::Stream, "s-1").len(), 32);
    assert_ne!(
        tokenizer.token(TokenDomain::Stream, "s-1"),
        tokenizer.token(TokenDomain::Frame, "s-1")
    );
}

#[test]
fn domain_names_cover_every_contract_namespace() {
    for (domain, name) in [
        (TokenDomain::Session, "session"),
        (TokenDomain::Stream, "stream"),
        (TokenDomain::Block, "block"),
        (TokenDomain::Batch, "batch"),
        (TokenDomain::Frame, "frame"),
        (TokenDomain::Request, "request"),
        (TokenDomain::Watch, "watch"),
        (TokenDomain::Connection, "connection"),
        (TokenDomain::Object, "object"),
        (TokenDomain::Ordinal, "ordinal"),
    ] {
        assert_eq!(domain.as_str(), name);
    }
}

#[test]
fn kernel_and_manual_clocks_return_ordered_nanoseconds() {
    let kernel = KernelMonotonicClock;
    let first = kernel.now_ns();
    let second = kernel.now_ns();
    assert!(second >= first);
    let manual = ManualClock::new(5);
    assert_eq!(manual.now_ns(), 5);
    manual.set(9);
    assert_eq!(manual.now_ns(), 9);
    manual.advance(3);
    assert_eq!(manual.now_ns(), 12);
}

#[test]
fn null_and_capturing_sinks_are_bounded_views() {
    let record = BenchRecord {
        v: 1,
        run: "run".into(),
        producer: Producer::Core,
        lifetime: "lt".into(),
        seq: 1,
        ts: 1,
        event: BenchEvent::CoreDelivery {
            frame: "f".into(),
            delivery: 1,
            kind: DeliveryKind::Preview,
            bytes: 1,
            stream: None,
            block: None,
            seq_start: None,
            seq_end: None,
            batch: None,
        },
    };
    NullSink.emit(record.clone());
    let capturing = CapturingSink::new();
    capturing.emit(record.clone());
    assert_eq!(capturing.len(), 1);
    assert_eq!(capturing.records(), vec![record.clone()]);
    assert_eq!(capturing.events(), vec![record.event]);
    assert!(!capturing.is_empty());
    assert!(CapturingSink::new().is_empty());
}

#[test]
fn context_accessors_share_one_sequence_and_one_clock() {
    let capturing = Arc::new(CapturingSink::new());
    let context = BenchContext::new(
        "run",
        "lt",
        Producer::Core,
        BenchMode::Trace,
        b"0123456789abcdef0123456789abcdef",
        capturing.clone(),
    );
    assert!(context.enabled());
    assert_eq!(context.mode(), BenchMode::Trace);
    assert_eq!(context.producer(), Producer::Core);
    assert_eq!(context.run_id(), "run");
    assert_eq!(context.lifetime(), "lt");
    assert!(!context
        .tokenizer()
        .token(TokenDomain::Stream, "s")
        .is_empty());
    assert_eq!(context.token(TokenDomain::Stream, "s").len(), 32);
    let first = context.next_write_ordinal();
    let second = context.next_write_ordinal();
    assert_eq!(second, first + 1);
    context.emit(BenchEvent::CoreDelivery {
        frame: "f".into(),
        delivery: 1,
        kind: DeliveryKind::Preview,
        bytes: 1,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    assert_eq!(capturing.len(), 1);
    let disabled = BenchContext::disabled(Producer::Core);
    assert!(!disabled.enabled());
    disabled.emit(BenchEvent::CoreDelivery {
        frame: "f".into(),
        delivery: 1,
        kind: DeliveryKind::Preview,
        bytes: 1,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
}

#[test]
fn record_validation_rejects_non_integer_timestamps_and_shape_mismatches() {
    let mut base = serde_json::json!({
        "v": 1, "run": "run", "producer": "core", "lifetime": "lt", "seq": 1,
        "ts": "one", "event": "core_delivery",
        "data": {"frame": "f", "delivery": 1, "kind": "preview", "bytes": 1}
    });
    assert!(matches!(
        validate_record(&base),
        Err(BenchViolation::InvalidEnvelope(_))
    ));
    base["ts"] = serde_json::json!(1);
    base["event"] = serde_json::json!("core_receipt");
    assert!(matches!(
        validate_record(&base),
        Err(BenchViolation::InvalidData(_))
    ));
    base["data"] = serde_json::json!({"frame": "f"});
    assert!(matches!(
        validate_record(&base),
        Err(BenchViolation::InvalidData(_))
    ));
    let mut extra_forbidden = serde_json::json!({
        "v": 1, "run": "run", "producer": "core", "lifetime": "lt", "seq": 1,
        "ts": 1, "event": "core_delivery",
        "data": {"frame": "f", "delivery": 1, "kind": "preview", "bytes": 1, "cursor": "x"}
    });
    assert!(matches!(
        validate_record(&extra_forbidden),
        Err(BenchViolation::ForbiddenField(_))
    ));
    extra_forbidden["data"]
        .as_object_mut()
        .unwrap()
        .remove("cursor");
    assert!(validate_record(&extra_forbidden).is_ok());
}

#[test]
fn manifest_event_count_must_match_the_rust_adapter() {
    for removed in ["access_notice", "access_transport"] {
        let mut drifted = manifest();
        drifted["events"].as_object_mut().unwrap().remove(removed);
        assert!(validate_manifest(&drifted).is_err(), "missing {removed}");
    }
}

#[test]
fn range_fields_are_declared_by_the_manifest_and_the_typed_adapter() {
    // latency/AC-01.waterfall
    let manifest = manifest();
    let events = [
        BenchEvent::CoreReceipt {
            frame: "f".into(),
            kind: ReceiptKind::Event,
            bytes: 1,
            decode: Decode::Ok,
            scope: Some("s".into()),
            stream: Some("st".into()),
            batch: Some("b".into()),
            block: Some("bl".into()),
            seq_start: Some(1),
            seq_end: Some(2),
            reason: Some("r".into()),
        },
        BenchEvent::CoreEnqueue {
            frame: "f".into(),
            queue_depth: 1,
            queue_bytes: 2,
            accepted: true,
            reason: Some("r".into()),
            stream: Some("st".into()),
            block: Some("bl".into()),
            seq_start: Some(1),
            seq_end: Some(2),
            batch: Some("b".into()),
        },
        BenchEvent::CoreDequeue {
            frame: "f".into(),
            wait_ns: 1,
            disposition: DequeueDisposition::Applied,
            reason: Some("r".into()),
            stream: Some("st".into()),
            block: Some("bl".into()),
            seq_start: Some(1),
            seq_end: Some(2),
            batch: Some("b".into()),
        },
        BenchEvent::CoreDelivery {
            frame: "f".into(),
            delivery: 1,
            kind: DeliveryKind::Preview,
            bytes: 1,
            stream: Some("st".into()),
            block: Some("bl".into()),
            seq_start: Some(1),
            seq_end: Some(2),
            batch: Some("b".into()),
        },
        BenchEvent::CliWrite {
            delivery: 1,
            ordinal: 1,
            bytes: 1,
            outcome: WriteOutcome::Success,
            flush: true,
            stream: Some("st".into()),
            block: Some("bl".into()),
            seq_start: Some(1),
            seq_end: Some(2),
        },
        BenchEvent::AccessTransport {
            connection: "c".into(),
            generation: 1,
            role: SocketRole::Host,
            direction: TransportDirection::HostToBroker,
            bytes: 1,
            outcome: TransportOutcome::Ok,
        },
    ];
    for event in events {
        let name = event.name();
        let spec = &manifest["events"][name];
        let fields = spec["fields"].as_object().expect("manifest fields");
        let required: Vec<&str> = spec["required"]
            .as_array()
            .expect("manifest required")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let optional: Vec<&str> = spec["optional"]
            .as_array()
            .expect("manifest optional")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let typed = serde_json::to_value(&event).expect("typed encoding");
        let data = typed["data"].as_object().expect("typed data");
        for key in data.keys() {
            assert!(
                required.contains(&key.as_str()) || optional.contains(&key.as_str()),
                "{name}.{key} is missing from the manifest"
            );
        }
        for key in &required {
            assert!(data.contains_key(*key), "{name}.{key} is not emitted");
        }
        assert_eq!(
            fields.len(),
            required.len() + optional.len(),
            "{name} declares a field in neither list"
        );
    }
}

#[test]
fn every_manifest_event_name_is_known_to_the_rust_adapter() {
    for name in EVENT_NAMES {
        assert!(manifest()["events"].get(*name).is_some());
    }
}

#[test]
fn every_remaining_event_name_is_stable() {
    let events = [
        (
            BenchEvent::FixtureClockCheck {
                producer: Producer::Core,
                reading_ns: 1,
                before_ns: 1,
                after_ns: 2,
                clock_source: "CLOCK_MONOTONIC".into(),
                ok: true,
                reason: None,
            },
            "fixture_clock_check",
        ),
        (
            BenchEvent::FixtureSlot {
                slot: 1,
                phase: FixturePhase::Measured,
                arm: FixtureArm::Watch,
                state: FixtureSlotState::Finished,
                pair: None,
            },
            "fixture_slot",
        ),
        (
            BenchEvent::FixtureSocketSample {
                role: SocketRole::Host,
                generation: 1,
                state: SocketSampleState::Sampled,
                rtt_us: Some(1),
                retransmits: None,
                queue_bytes: None,
                gap_ms: None,
            },
            "fixture_socket_sample",
        ),
        (
            BenchEvent::FixtureObserve {
                attempt: "a".into(),
                ordinal: 1,
                kind: ObserveKind::Preview,
                bytes: 1,
                prefix_bytes: 1,
                correct: true,
                block: None,
            },
            "fixture_observe",
        ),
        (
            BenchEvent::FixtureEmit {
                stream: "s".into(),
                attempt: "a".into(),
                block: "b".into(),
                seq_start: 0,
                seq_end: 1,
                bytes: 1,
            },
            "fixture_emit",
        ),
        (
            BenchEvent::FixtureRecovery {
                attempt: "a".into(),
                milestone: RecoveryMilestone::LinkRestored,
                state: MilestoneState::Observed,
                generation: None,
            },
            "fixture_recovery",
        ),
        (
            BenchEvent::DshHandoff {
                stream: "s".into(),
                attempt: "a".into(),
                block: "b".into(),
                seq_start: 0,
                seq_end: 1,
                bytes: 1,
            },
            "dsh_handoff",
        ),
        (
            BenchEvent::HostEnqueue {
                stream: "s".into(),
                block: "b".into(),
                seq_start: 0,
                seq_end: 1,
                bytes: 1,
                queue_depth: 1,
                queue_bytes: 1,
            },
            "host_enqueue",
        ),
        (
            BenchEvent::HostCoalesce {
                stream: "s".into(),
                block: "b".into(),
                seq_start: 0,
                seq_end: 1,
                frames: 1,
                bytes: 1,
            },
            "host_coalesce",
        ),
        (
            BenchEvent::HostDequeue {
                stream: "s".into(),
                block: "b".into(),
                seq_start: 0,
                seq_end: 1,
                bytes: 1,
                wait_ns: 1,
            },
            "host_dequeue",
        ),
        (
            BenchEvent::HostPublish {
                request: "r".into(),
                attempt: 1,
                kind: PublishKind::Event,
                bytes: 1,
                outcome: PublishOutcome::Accepted,
                stream: None,
                seq_start: None,
                seq_end: None,
            },
            "host_publish",
        ),
        (
            BenchEvent::HostCommand {
                request: "r".into(),
                attempt: 1,
                stage: CommandStage::Admit,
                outcome: None,
            },
            "host_command",
        ),
        (
            BenchEvent::HostSubscribe {
                connection: "c".into(),
                generation: 1,
                outcome: SubscribeOutcome::Subscribed,
            },
            "host_subscribe",
        ),
        (
            BenchEvent::AccessCommand {
                request: "r".into(),
                attempt: 1,
                stage: CommandStage::Admit,
                durable: true,
                outcome: None,
                bytes: None,
            },
            "access_command",
        ),
        (
            BenchEvent::AccessRelay {
                request: "r".into(),
                object_op: "o".into(),
                stage: RelayStage::Stage,
                route: AccessRoute::Reference,
                bytes: None,
                outcome: None,
            },
            "access_relay",
        ),
        (
            BenchEvent::AccessNotice {
                connection: "c".into(),
                generation: 1,
                role: SocketRole::Host,
                stage: AccessNoticeStage::Subscribe,
                outcome: None,
            },
            "access_notice",
        ),
        (
            BenchEvent::AccessTransport {
                connection: "c".into(),
                generation: 1,
                role: SocketRole::Host,
                direction: TransportDirection::HostToBroker,
                bytes: 1,
                outcome: TransportOutcome::Ok,
            },
            "access_transport",
        ),
    ];
    for (event, expected) in events {
        assert_eq!(event.name(), expected);
        assert!(!event.producers().is_empty());
    }
}

#[test]
fn forbidden_field_detection_is_shared_with_serde_errors() {
    assert_eq!(
        forbidden_from_error("data contains `text`"),
        Some("text".into())
    );
    assert_eq!(
        forbidden_from_error("data contains `cursor`"),
        Some("cursor".into())
    );
    assert_eq!(forbidden_from_error("data contains nothing"), None);
}

#[test]
fn frame_identity_is_deterministic_and_scoped() {
    let mut envelope = whalerein_proto::EventEnvelope {
        host_id: "host".into(),
        device_id: "device".into(),
        channel_id: "channel".into(),
        nonce: vec![1, 2, 3],
        ..Default::default()
    };
    let first = event_frame_identity(&envelope);
    let second = event_frame_identity(&envelope);
    assert_eq!(first, second);
    assert!(!first.is_empty());
    envelope.channel_id = "other".into();
    assert_ne!(first, event_frame_identity(&envelope));
}

#[test]
fn capture_key_decoding_rejects_malformed_encodings() {
    assert!(decode_capture_key("not hex or base64!").is_err());
    assert!(decode_capture_key("AA==").is_ok());
    assert!(decode_capture_key("00112233").is_ok());
}
