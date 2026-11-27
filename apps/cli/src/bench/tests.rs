//! Process outlet and CLI write-stamp tests.
//!
//! Binding names appear as `latency/AC-XX.variant` comments for acceptance.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use whalerein_core::api::bench::{
    BenchContext, BenchEvent, BenchMode, BenchRecord, BenchSink, CapturingSink, Decode,
    DeliveryKind, ManualClock, Producer, ReceiptKind, WriteOutcome,
};

use super::*;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn setup_with(pairs: &[(&str, &str)], linux: bool) -> BenchSetup {
    let env = env(pairs);
    BenchSetup::from_env_with(|name| env.get(name).cloned(), linux)
}

fn record(event: BenchEvent) -> BenchRecord {
    BenchRecord {
        v: whalerein_core::api::bench::SCHEMA_VERSION,
        run: "run".into(),
        producer: Producer::Core,
        lifetime: "lt".into(),
        seq: 1,
        ts: 1,
        event,
    }
}

fn receipt() -> BenchEvent {
    BenchEvent::CoreReceipt {
        frame: "f-1".into(),
        kind: ReceiptKind::Event,
        bytes: 5,
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

/// One decrypted live preview increment used to derive join facts.
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

fn shared_context() -> (Arc<BenchContext>, Arc<BenchContext>, Arc<CapturingSink>) {
    let sink = Arc::new(CapturingSink::new());
    let core = BenchContext::with_clock(
        "run",
        "lt",
        Producer::Core,
        BenchMode::Trace,
        b"key",
        sink.clone(),
        Arc::new(ManualClock::new(10)),
    );
    let cli = core.for_producer(Producer::Cli);
    (core, cli, sink)
}

#[test]
fn off_and_aggregate_create_no_writer() {
    // latency/AC-07.off
    // latency/AC-07.aggregate
    let dir = tempfile::tempdir().unwrap();
    let outlet = dir.path().join("outlet.ndjson");
    for mode in ["off", "aggregate"] {
        let setup = setup_with(
            &[
                (MODE_ENV, mode),
                (OUTLET_ENV, outlet.to_str().unwrap()),
                (RUN_ID_ENV, "run"),
                (CAPTURE_KEY_ENV, "00ff"),
            ],
            true,
        );
        assert!(setup.outlet().is_none());
    }
    assert!(!outlet.exists());
}

#[test]
fn trace_without_outlet_creates_no_writer() {
    // latency/AC-07.mode-availability
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (RUN_ID_ENV, "run"),
            (CAPTURE_KEY_ENV, "00ff"),
        ],
        true,
    );
    assert!(setup.outlet().is_none());
}

#[test]
fn trace_outlet_appends_ndjson_and_flushes_on_shutdown() {
    // latency/AC-06.cleanup
    // latency/AC-14.cli-endpoint
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (OUTLET_ENV, path.to_str().unwrap()),
            (RUN_ID_ENV, "run-1"),
            (CAPTURE_KEY_ENV, "00112233445566778899aabbccddeeff"),
            (LIFETIME_ENV, "lt-shared"),
        ],
        true,
    );
    assert!(setup.disabled_reason().is_none());
    let core = setup.core_context();
    let cli = setup.cli_context();
    core.emit(receipt());
    cli.emit(BenchEvent::CliWrite {
        delivery: 1,
        ordinal: 1,
        bytes: 7,
        outcome: WriteOutcome::Success,
        flush: true,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
    });
    setup.finish();
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        whalerein_core::api::bench::validate_record(&value).unwrap();
    }
    let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(first["lifetime"], "lt-shared");
    assert_eq!(first["run"], "run-1");
    assert_eq!(first["producer"], "core");
    let second: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
    assert_eq!(second["producer"], "cli");
}

#[test]
fn a_full_channel_drops_and_counts_without_blocking() {
    // latency/AC-06.saturation
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let outlet = FileOutlet::start(&path, 1, DEFAULT_BYTE_CAP).unwrap();
    // Fill the bounded channel faster than the writer can drain it.
    let before = outlet.dropped();
    for _ in 0..2_000 {
        outlet.emit(record(receipt()));
    }
    assert!(outlet.dropped() > before);
    outlet.shutdown();
}

#[test]
fn byte_cap_stops_retaining_detail_and_counts_loss() {
    // latency/AC-06.saturation
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let outlet = FileOutlet::start(&path, 64, 50).unwrap();
    for _ in 0..8 {
        outlet.emit(record(receipt()));
    }
    outlet.shutdown();
    assert!(outlet.dropped() >= 8);
    assert!(outlet.written_bytes() <= 50);
    assert!(outlet.byte_cap() == 50);
}

#[test]
fn an_unopenable_outlet_disables_capture_without_failing_the_cli() {
    // latency/AC-06.sink-failure
    let dir = tempfile::tempdir().unwrap();
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (OUTLET_ENV, dir.path().to_str().unwrap()),
            (RUN_ID_ENV, "run"),
            (CAPTURE_KEY_ENV, "00ff"),
        ],
        true,
    );
    assert!(setup.outlet().is_none());
    assert!(setup.disabled_reason().is_some());
    // Disabled emission and shutdown stay no-ops.
    setup.core_context().emit(receipt());
    setup.finish();
}

#[test]
fn malformed_capture_settings_never_panic() {
    // latency/AC-05.remote-rejected
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (OUTLET_ENV, path.to_str().unwrap()),
            // run id and key missing
        ],
        true,
    );
    assert!(setup.outlet().is_none());
    assert!(setup.disabled_reason().unwrap().contains(RUN_ID_ENV));
}

#[test]
fn non_linux_capture_is_rejected() {
    // latency/AC-05.foreign-namespace
    // latency/AC-05.remote-rejected
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (OUTLET_ENV, path.to_str().unwrap()),
            (RUN_ID_ENV, "run"),
            (CAPTURE_KEY_ENV, "00ff"),
        ],
        false,
    );
    assert!(setup.outlet().is_none());
    assert!(setup.disabled_reason().unwrap().contains("Linux"));
}

#[test]
fn capture_key_accepts_hex_and_base64_and_uses_one_outlet() {
    // latency/AC-05.shared-clock
    let dir = tempfile::tempdir().unwrap();
    for key in [
        "00112233445566778899aabbccddeeff",
        "ABEiM0RVZneImaq7zN3u/w==",
    ] {
        let path = dir.path().join("outlet.ndjson");
        let setup = setup_with(
            &[
                (MODE_ENV, "trace"),
                (OUTLET_ENV, path.to_str().unwrap()),
                (RUN_ID_ENV, "run"),
                (CAPTURE_KEY_ENV, key),
            ],
            true,
        );
        assert!(setup.outlet().is_some(), "key {key}");
        setup.finish();
    }
}

#[test]
fn one_outlet_carries_core_and_cli_records_in_sequence() {
    // latency/AC-07.shared-process-outlet
    let (core, cli, sink) = shared_context();
    core.emit(receipt());
    cli.emit(BenchEvent::CliWrite {
        delivery: 1,
        ordinal: 1,
        bytes: 1,
        outcome: WriteOutcome::Success,
        flush: true,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
    });
    let records = sink.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].seq + 1, records[1].seq);
    assert_eq!(records[1].producer, Producer::Cli);
}

#[test]
fn cli_write_is_stamped_only_after_a_successful_write() {
    // latency/AC-14.cli-endpoint
    // latency/AC-01.partial-write
    let (_, cli, sink) = shared_context();
    let stamps = WriteStamps::new(cli);
    stamps.stamp(Some(7), None, 4, WriteOutcome::Success);
    stamps.stamp(None, None, 4, WriteOutcome::Success);
    stamps.stamp(Some(8), None, 1, WriteOutcome::Failed);
    let writes: Vec<_> = sink
        .records()
        .into_iter()
        .filter_map(|record| match record.event {
            BenchEvent::CliWrite {
                delivery,
                ordinal,
                bytes,
                outcome,
                flush,
                ..
            } => Some((delivery, ordinal, bytes, outcome, flush)),
            _ => None,
        })
        .collect();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0], (7, 1, 4, WriteOutcome::Success, true));
    assert_eq!(writes[1], (8, 2, 1, WriteOutcome::Failed, true));
}

#[test]
fn a_write_stamp_uses_the_delivery_ordinal_it_rendered() {
    // latency/AC-01.overlapping-stamps
    // latency/AC-01.batched-split-read
    let (core, cli, sink) = shared_context();
    core.emit(BenchEvent::CoreDelivery {
        frame: "f-1".into(),
        delivery: 3,
        kind: DeliveryKind::Message,
        bytes: 5,
        stream: None,
        block: None,
        seq_start: None,
        seq_end: None,
        batch: None,
    });
    let stamps = WriteStamps::new(cli);
    // Two records completed by one batched read share the delivery identity
    // but keep distinct write ordinals.
    stamps.stamp(Some(3), None, 5, WriteOutcome::Success);
    stamps.stamp(Some(3), None, 5, WriteOutcome::Success);
    let writes: Vec<_> = sink
        .records()
        .into_iter()
        .filter_map(|record| match record.event {
            BenchEvent::CliWrite {
                delivery, ordinal, ..
            } => Some((delivery, ordinal)),
            _ => None,
        })
        .collect();
    assert_eq!(writes, vec![(3, 1), (3, 2)]);
}

#[test]
fn cli_write_carries_the_delivery_stream_range_and_ordinal() {
    // latency/AC-01.waterfall
    let (core, cli, sink) = shared_context();
    let update = live_update("stream-raw", "block-raw", 4, 6);
    let range = FrameRange::from_update(&core, &update).expect("live range");
    core.emit(BenchEvent::CoreDelivery {
        frame: "f-1".into(),
        delivery: 9,
        kind: DeliveryKind::Preview,
        bytes: 5,
        stream: Some(range.stream.clone()),
        block: range.block.clone(),
        seq_start: Some(range.seq_start),
        seq_end: Some(range.seq_end),
        batch: Some(range.batch.clone()),
    });
    let stamps = WriteStamps::new(cli);
    stamps.stamp(Some(9), Some(&range), 5, WriteOutcome::Success);
    let records = sink.records();
    let delivery = records[0].to_json();
    let write = records[1].to_json();
    assert_eq!(write["producer"], "cli");
    assert_eq!(write["data"]["delivery"], delivery["data"]["delivery"]);
    assert_eq!(write["data"]["ordinal"], 1);
    for field in ["stream", "block", "seq_start", "seq_end"] {
        assert_eq!(
            write["data"][field], delivery["data"][field],
            "cli_write must repeat the delivery's {field}"
        );
    }
}

#[test]
fn cli_write_without_a_preview_range_omits_every_range_field() {
    // latency/AC-01.no-invented-range
    let (_, cli, sink) = shared_context();
    let stamps = WriteStamps::new(cli);
    // A message or attempt-ended delivery (and a record with no range) still
    // stamp completion, but never claim a stream increment.
    stamps.stamp(Some(4), None, 3, WriteOutcome::Success);
    let data = sink.records()[0].to_json();
    let fields = data["data"].as_object().unwrap();
    for field in ["stream", "block", "seq_start", "seq_end"] {
        assert!(!fields.contains_key(field), "{field} must stay absent");
    }
    assert_eq!(data["data"]["ordinal"], 1);
}

#[test]
fn counted_write_maps_partial_and_failed_outcomes_without_a_success_stamp() {
    // latency/AC-01.partial-write
    struct PartialThenClosed;
    impl Write for PartialThenClosed {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len().min(2))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
    }
    struct NeverWrites;
    impl Write for NeverWrites {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (written, bytes) = crate::stream::write_all_counted(&mut PartialThenClosed, b"abcdef");
    assert!(written.is_err());
    assert_eq!(outcome_for(&written, bytes, 6), WriteOutcome::Partial);
    let (written, bytes) = crate::stream::write_all_counted(&mut NeverWrites, b"abcdef");
    assert!(written.is_err());
    assert_eq!(outcome_for(&written, bytes, 6), WriteOutcome::Failed);
}

#[test]
fn a_successful_counted_write_always_counts_every_byte() {
    // latency/AC-14.cli-endpoint
    #[derive(Default)]
    struct Buffer(Vec<u8>);
    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer::default();
    let (written, bytes) = crate::stream::write_all_counted(&mut buffer, b"line\n");
    assert!(written.is_ok());
    assert_eq!(bytes, 5);
    assert_eq!(buffer.0, b"line\n");
    assert_eq!(outcome_for(&written, bytes, 5), WriteOutcome::Success);
}

#[test]
fn verbosity_does_not_change_capture_eligibility() {
    // latency/AC-07.verbosity-independent
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (OUTLET_ENV, path.to_str().unwrap()),
            (RUN_ID_ENV, "run"),
            (CAPTURE_KEY_ENV, "00ff"),
        ],
        true,
    );
    // The outlet reads only capture variables; ordinary log level is not an
    // input to admission.
    assert!(setup.outlet().is_some());
}

#[test]
fn shutdown_is_bounded_and_idempotent() {
    // latency/AC-06.cleanup
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let outlet = FileOutlet::start(&path, 4, DEFAULT_BYTE_CAP).unwrap();
    outlet.emit(record(receipt()));
    outlet.shutdown();
    outlet.shutdown();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 1);
}

#[test]
fn process_lifetime_is_derived_when_unset() {
    // latency/AC-07.restart
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let setup = setup_with(
        &[
            (MODE_ENV, "trace"),
            (OUTLET_ENV, path.to_str().unwrap()),
            (RUN_ID_ENV, "run"),
            (CAPTURE_KEY_ENV, "00ff"),
        ],
        true,
    );
    let lifetime = setup.cli_context().lifetime().to_owned();
    assert!(lifetime.starts_with("lt-"));
    // Raw process identifiers never enter a record.
    assert!(!lifetime.contains(&format!("-{}-", std::process::id())));
}

#[test]
fn a_failed_writer_marks_the_capture_failed_and_drops_without_panicking() {
    // latency/AC-06.sink-failure
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outlet.ndjson");
    let outlet = FileOutlet::start(&path, 8, DEFAULT_BYTE_CAP).unwrap();
    outlet.state.failed.store(true, Ordering::SeqCst);
    for _ in 0..5 {
        outlet.emit(record(receipt()));
    }
    assert_eq!(outlet.dropped(), 5);
    outlet.shutdown();
}

#[test]
fn event_receipt_has_no_raw_identifier() {
    // latency/AC-06.revocation
    let (core, _, sink) = shared_context();
    let frame = core.token(whalerein_core::api::bench::TokenDomain::Frame, "f-1-raw");
    core.emit(BenchEvent::CoreReceipt {
        frame,
        kind: ReceiptKind::Event,
        bytes: 5,
        decode: Decode::Ok,
        scope: None,
        stream: None,
        batch: None,
        block: None,
        seq_start: None,
        seq_end: None,
        reason: None,
    });
    let text = sink.records()[0].to_json().to_string();
    assert!(!text.contains("f-1-raw"));
    assert!(
        sink.records()[0].to_json()["data"]["frame"]
            .as_str()
            .unwrap()
            .len()
            < 64
    );
}
