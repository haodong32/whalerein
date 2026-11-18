//! Behavioral tests for the exported `MobileCore` contract.
//!
//! The pairing-backed cases use Core's deterministic loopback fake Access and
//! Host fixtures. They exercise the real binding, Core facade and cache without
//! external services.

use std::time::{Duration, Instant};

use bytes::BytesMut;
use rumqttc::v5::mqttbytes::v5::Packet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use whalerein_core::api::test_support::{history_message, FakeAccess};
use whalerein_mobile::{
    write_client_config, ConversationEvent, DeliveryChoice, MemorySecretStore, MessageView,
    MobileClientService, MobileCore,
};

fn write_config(dir: &std::path::Path) -> String {
    let config = dir.join("config.toml").to_string_lossy().into_owned();
    write_client_config(
        config.clone(),
        MobileClientService::Standalone,
        "instance".into(),
        None,
        None,
    )
    .expect("write client config");
    config
}

fn new_core(data_dir: &std::path::Path) -> std::sync::Arc<MobileCore> {
    MobileCore::new(
        write_config(data_dir),
        MemorySecretStore::new(),
        "test-app".to_owned(),
    )
    .expect("create MobileCore")
}

fn wait_for_pairing(core: &MobileCore) -> (Option<String>, Option<String>) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for event in core.poll_events(500) {
            if let ConversationEvent::PairingFinished { host_id, code } = event {
                return (host_id, code);
            }
        }
        assert!(
            Instant::now() < deadline,
            "pairing did not finish before the deadline"
        );
    }
}

#[test]
fn unpaired_hosts_is_a_typed_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let error = core.hosts().unwrap_err();
    assert_eq!(error.code(), "not_paired");
    assert!(!error.message().is_empty());
    // It stays a typed failure on repetition and cached_sessions also fails.
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
    assert_eq!(
        core.cached_sessions("host-1".into()).unwrap_err().code(),
        "not_paired"
    );
    assert_eq!(
        core.capabilities("host-1".into(), "session-1".into())
            .unwrap_err()
            .code(),
        "not_paired"
    );
    assert_eq!(
        core.cached_messages("host-1".into(), "session-1".into())
            .unwrap_err()
            .code(),
        "not_paired"
    );
    assert_eq!(core.connection_state("host-1".into()), "disconnected");
}

#[test]
fn poll_events_timeout_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let started = Instant::now();
    let events = core.poll_events(50);
    assert!(events.is_empty());
    assert!(started.elapsed() < Duration::from_secs(2));
    // A zero timeout never blocks.
    assert!(core.poll_events(0).is_empty());
}

#[test]
fn validation_failures_are_typed_before_any_network_work() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    assert_eq!(
        core.send(
            "host-1".into(),
            "session-1".into(),
            "   ".into(),
            DeliveryChoice::Send
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.send(
            "host-1".into(),
            "session-1".into(),
            "x".repeat(300 * 1024),
            DeliveryChoice::Steer
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.resolve_approval(
            "host-1".into(),
            "session-1".into(),
            "approval-1".into(),
            1,
            "not-hex".into(),
            true,
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.resolve_approval(
            "host-1".into(),
            "session-1".into(),
            "approval-1".into(),
            0,
            "ab".repeat(32),
            true,
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    assert_eq!(
        core.save_draft("host-1".into(), "session-1".into(), "x".repeat(300 * 1024))
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}

#[test]
fn unpaired_diagnostics_preview_is_redacted_and_local() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let report = core.diagnostics_preview().unwrap();
    let value: serde_json::Value = serde_json::from_str(&report.json).unwrap();
    assert_eq!(report.app_version, "test-app");
    assert_eq!(report.core_version, env!("CARGO_PKG_VERSION"));
    assert!(!report.redaction_fields.is_empty());
    assert!(value.get("redactions").is_some());
    assert!(value.get("fields").is_some());
    assert!(report.last_failure_code.is_none());
}

#[test]
fn pairing_enables_hosts_drafts_and_redacted_export() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    access.host.lock().unwrap().host_id = "host-fixture-9f3".to_owned();
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());

    let prompt = core.pairing_start(access.pairing_uri()).unwrap();
    assert_eq!(prompt.fingerprint, access.fingerprint());
    assert!(!prompt.fingerprint_display.is_empty());
    core.pairing_decide(true, Some(access.fingerprint()))
        .unwrap();

    let (finished_host, code) = wait_for_pairing(&core);
    assert_eq!(code, None);
    assert_eq!(finished_host.as_deref(), Some(prompt.host_id.as_str()));

    let hosts = core.hosts().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].host_id, prompt.host_id);
    assert_eq!(hosts[0].fingerprint, access.fingerprint());

    let host_id = prompt.host_id.clone();
    let session_id = "session-1".to_owned();
    // An empty local cache is an empty list, not an error.
    assert!(core
        .cached_messages(host_id.clone(), session_id.clone())
        .unwrap()
        .is_empty());
    assert_eq!(
        core.load_draft(host_id.clone(), session_id.clone())
            .unwrap(),
        None
    );
    core.save_draft(
        host_id.clone(),
        session_id.clone(),
        "unsent draft text".into(),
    )
    .unwrap();
    assert_eq!(
        core.load_draft(host_id.clone(), session_id.clone())
            .unwrap()
            .as_deref(),
        Some("unsent draft text")
    );
    // Empty text clears the draft, matching an emptied editor.
    core.save_draft(host_id.clone(), session_id.clone(), String::new())
        .unwrap();
    assert_eq!(
        core.load_draft(host_id.clone(), session_id.clone())
            .unwrap(),
        None
    );
    // Explicit clear is idempotent.
    core.clear_draft(host_id.clone(), session_id).unwrap();

    let preview = core.diagnostics_preview().unwrap();
    assert!(!preview.json.contains("unsent draft"));
    assert!(!preview.json.contains(&host_id));
    assert!(preview
        .redaction_fields
        .iter()
        .any(|field| field == "message content"));

    let path = dir.path().join("diagnostics.json");
    let export = core
        .export_diagnostics(path.to_string_lossy().into_owned())
        .unwrap();
    assert!(path.exists());
    assert!(!export.json.contains("unsent draft"));
    assert!(!export.json.contains(&host_id));
    assert!(export
        .redaction_fields
        .iter()
        .any(|field| field == "pairing tokens"));
}

#[test]
fn pairing_cancel_emits_cancelled_and_allows_a_new_attempt() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());

    core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_cancel();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events = core.poll_events(500);
        if events.iter().any(|event| {
            matches!(
                event,
                ConversationEvent::PairingFinished {
                    code: Some(code),
                    ..
                } if code == "cancelled"
            )
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "cancel event missing");
    }
    // A cancelled attempt is not running who knows what; hosts stays unpaired.
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
}

fn pair(core: &MobileCore, access: &FakeAccess) -> String {
    let prompt = core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_decide(true, Some(access.fingerprint()))
        .unwrap();
    let (host_id, code) = wait_for_pairing(core);
    assert_eq!(code, None);
    assert_eq!(host_id.as_deref(), Some(prompt.host_id.as_str()));
    prompt.host_id
}

#[test]
fn memory_constructor_and_revision_accessors_work() {
    let dir = tempfile::tempdir().unwrap();
    let core =
        MobileCore::new_with_memory_secret_store(write_config(dir.path()), "t".into()).unwrap();
    assert_eq!(core.core_revision(), whalerein_mobile::core_revision());
    assert!(!core.core_revision().is_empty());
}

#[test]
fn pairing_rejection_and_repeat_start_are_typed() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());

    core.pairing_start(access.pairing_uri()).unwrap();
    // A second start while one is pending is a conflict.
    assert_eq!(
        core.pairing_start(access.pairing_uri()).unwrap_err().code(),
        "conflict"
    );
    core.pairing_decide(false, None).unwrap();
    let (host_id, code) = wait_for_pairing(&core);
    assert_eq!(host_id, None);
    assert_eq!(code.as_deref(), Some("cancelled"));
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");

    // After completion a fresh attempt is allowed.
    core.pairing_start(access.pairing_uri()).unwrap();
    core.pairing_cancel();
}

#[test]
fn sessions_send_stop_and_approvals_reach_the_fixture_host() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);

    let sessions = core.sessions(host_id.clone()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "session-1");
    assert!(!sessions[0].title.is_empty());
    assert!(sessions[0].running);
    assert!(sessions[0].capabilities.is_some());

    let capabilities = core
        .capabilities(host_id.clone(), "session-1".into())
        .unwrap();
    // The fixture Source reports its capability set explicitly; the binding
    // must reflect it rather than inventing delivery support.
    assert!(!capabilities.interrupt);
    assert!(capabilities.delivery_modes.is_empty());
    assert!(capabilities.model_selection);
    assert_eq!(
        core.capabilities(host_id.clone(), "missing-session".into())
            .unwrap_err()
            .code(),
        "not_found"
    );
    // The session list was cached by the successful query.
    assert_eq!(core.cached_sessions(host_id.clone()).unwrap().len(), 1);

    let sent = core
        .send(
            host_id.clone(),
            "session-1".into(),
            "hello from the binding".into(),
            DeliveryChoice::Send,
        )
        .unwrap();
    assert!(sent.status == "succeeded" || sent.status == "host_accepted");
    assert!(!sent.request_id.is_empty());
    assert!(sent.message_id.is_some());
    let recovered = core.recover_send(sent.request_id.clone(), true).unwrap();
    assert_eq!(recovered.operation_id, sent.request_id);
    assert_eq!(recovered.host_id, host_id);
    assert_eq!(recovered.session_id, "session-1");
    assert!(matches!(
        recovered.reply,
        Some(whalerein_mobile::MessageReplyView::Committed { ref attempt_id, .. })
            if attempt_id == "fixture-attempt"
    ));
    // The same result is queued for observers.
    assert!(core.poll_events(0).iter().any(|event| matches!(
        event,
        ConversationEvent::CommandStatus { command } if command.request_id == sent.request_id
    )));

    let stopped = core.stop(host_id.clone(), "session-1".into()).unwrap();
    assert!(!stopped.command_id.is_empty());

    // The fixture Host does not implement approval listing; the binding must
    // surface a typed failure rather than panicking.
    match core.approvals(host_id.clone(), "session-1".into()) {
        Ok(approvals) => assert!(approvals.is_empty()),
        Err(error) => assert!(
            matches!(error.code(), "unavailable" | "internal"),
            "unexpected approval listing error: {error:?}"
        ),
    }
    let resolved = core
        .resolve_approval(
            host_id.clone(),
            "session-1".into(),
            "approval-1".into(),
            1,
            "ab".repeat(32),
            true,
        )
        .unwrap();
    assert!(!resolved.request_id.is_empty());
}

#[test]
fn watch_start_and_stop_are_prompt_and_idempotent() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);

    core.start_watch(host_id.clone(), "session-1".into())
        .unwrap();
    // Replacing an active watch is allowed.
    core.start_watch(host_id.clone(), "session-1".into())
        .unwrap();
    core.stop_watch();
    core.stop_watch();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ended = 0;
    while Instant::now() < deadline && ended == 0 {
        ended += core
            .poll_events(200)
            .iter()
            .filter(|event| matches!(event, ConversationEvent::WatchEnded))
            .count();
    }
    assert!(ended >= 1, "stopWatch must emit watchEnded");
}

#[test]
fn forget_device_removes_pairing_and_revoke_is_terminal() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair(&core, &access);
    core.save_draft(host_id.clone(), "session-1".into(), "draft".into())
        .unwrap();
    // Core requires explicit revocation before forgetting a paired identity.
    assert_eq!(core.forget_device().unwrap_err().code(), "conflict");
    core.revoke().unwrap();
    core.forget_device().unwrap();
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");

    let access = runtime.block_on(FakeAccess::start());
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let _host_id = pair(&core, &access);
    core.revoke().unwrap();
    assert_eq!(core.hosts().unwrap_err().code(), "not_paired");
}

#[test]
fn export_diagnostics_rejects_an_unusable_path() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"file").unwrap();
    let unusable = blocker.join("nested").join("diagnostics.json");
    let error = core
        .export_diagnostics(unusable.to_string_lossy().into_owned())
        .unwrap_err();
    assert_eq!(error.code(), "internal");
}

// ---------------------------------------------------------------------------
// G10: queued events belong to the watch that produced them.
// ---------------------------------------------------------------------------

/// Accept MQTT CONNECT/SUBSCRIBE from Core's watchers and hold the streams open.
///
/// The fixture Host only publishes through Watch/Refresh responses; the test
/// peer is enough to let a real watch attach so its events enter the binding
/// queue through the production path.
fn spawn_mqtt_peer(runtime: &tokio::runtime::Runtime, listener: TcpListener) {
    runtime.spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut stream = stream;
                let mut buffer = BytesMut::new();
                if !matches!(
                    read_mqtt_packet(&mut stream, &mut buffer).await,
                    Packet::Connect(..)
                ) {
                    return;
                }
                if stream.write_all(&[0x20, 3, 0, 0, 0]).await.is_err() {
                    return;
                }
                let Packet::Subscribe(subscription) =
                    read_mqtt_packet(&mut stream, &mut buffer).await
                else {
                    return;
                };
                let [high, low] = subscription.pkid.to_be_bytes();
                if stream.write_all(&[0x90, 4, high, low, 0, 0]).await.is_err() {
                    return;
                }
                // Keep the subscription alive until Core drops the watch.
                let mut sink = [0u8; 64];
                while matches!(stream.read(&mut sink).await, Ok(size) if size > 0) {}
            });
        }
    });
}

async fn read_mqtt_packet(stream: &mut TcpStream, buffer: &mut BytesMut) -> Packet {
    loop {
        if let Ok(packet) = Packet::read(buffer, None) {
            return packet;
        }
        assert_ne!(
            stream.read_buf(buffer).await.unwrap(),
            0,
            "MQTT peer closed before the subscription"
        );
    }
}

fn pair_with_mqtt(core: &MobileCore, access: &FakeAccess, mqtt_url: &str) -> String {
    // The invitation's optional `mqtt=` override keeps the deterministic
    // loopback broker per profile instead of mutating process environment.
    let invitation = format!("{}&mqtt={mqtt_url}", access.pairing_uri());
    let prompt = core.pairing_start(invitation).unwrap();
    core.pairing_decide(true, Some(access.fingerprint()))
        .unwrap();
    let (host_id, code) = wait_for_pairing(core);
    assert_eq!(code, None);
    assert_eq!(host_id.as_deref(), Some(prompt.host_id.as_str()));
    prompt.host_id
}

fn wait_for_cached_message(core: &MobileCore, host_id: &str, session_id: &str, message_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let messages = core
            .cached_messages(host_id.to_owned(), session_id.to_owned())
            .unwrap();
        if messages
            .iter()
            .any(|message| message.message_id == message_id)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "message {message_id} did not reach the {host_id}/{session_id} cache"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn drain_events(core: &MobileCore) -> Vec<ConversationEvent> {
    let mut events = Vec::new();
    loop {
        let batch = core.poll_events(150);
        if batch.is_empty() {
            return events;
        }
        events.extend(batch);
    }
}

fn event_text(message: &MessageView) -> String {
    message
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_message(events: &[ConversationEvent], text: &str) -> bool {
    events.iter().any(|event| match event {
        ConversationEvent::Message { message } => event_text(message) == text,
        _ => false,
    })
}

#[test]
fn queued_old_watch_events_never_become_the_next_sessions_events() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let mqtt_url = format!("mqtt://{}", listener.local_addr().unwrap());
    spawn_mqtt_peer(&runtime, listener);
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair_with_mqtt(&core, &access, &mqtt_url);

    // Warm the committed cursor for both sessions. A later watch on a warm
    // cursor treats the next history interval as new output instead of a cold
    // snapshot, which is what makes the queued old-watch message observable.
    for session_id in ["session-1", "session-2"] {
        core.start_watch(host_id.clone(), session_id.into())
            .unwrap();
        wait_for_cached_message(&core, &host_id, session_id, "message-1");
        core.stop_watch();
        drain_events(&core);
        std::thread::sleep(Duration::from_millis(50));
    }

    // The old watch starts with a distinct committed message already waiting,
    // then the consumer is paused while that message enters the real queue.
    {
        let mut host = access.host.lock().unwrap();
        host.history_advance = Some((
            vec![history_message("old-message", "OLD-WATCH-CONTENT")],
            "bookmark:old".into(),
        ));
        assert_eq!(host.history_cut, "bookmark:committed");
    }
    core.start_watch(host_id.clone(), "session-1".into())
        .unwrap();
    wait_for_cached_message(&core, &host_id, "session-1", "old-message");
    // The cache is committed immediately before the watch callback queues the
    // Message; give that callback its moment while the consumer stays paused.
    std::thread::sleep(Duration::from_millis(200));

    // An independent non-watch event (a command receipt) is queued while the
    // old watch's output is still buffered. It must survive replacement.
    let sent = core
        .send(
            host_id.clone(),
            "session-1".into(),
            "command while paused".into(),
            DeliveryChoice::Send,
        )
        .unwrap();

    // Stop the old watch: its watchEnded enters the real queue and must not
    // terminate the replacement watch after the consumer resumes.
    core.stop_watch();

    // The replacement watch is a different session on the same Host and has a
    // genuine committed message waiting at its next cursor interval.
    {
        let mut host = access.host.lock().unwrap();
        host.history_cut = "bookmark:committed".into();
        host.history_advance = Some((
            vec![history_message("new-message", "NEW-SESSION-CONTENT")],
            "bookmark:new".into(),
        ));
    }
    core.start_watch(host_id.clone(), "session-2".into())
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut emitted = Vec::new();
    let mut saw_new_message = false;
    while Instant::now() < deadline {
        let batch = core.poll_events(200);
        emitted.extend(batch);
        if has_message(&emitted, "NEW-SESSION-CONTENT") {
            saw_new_message = true;
            break;
        }
        assert!(
            !emitted
                .iter()
                .any(|event| matches!(event, ConversationEvent::WatchEnded)),
            "the replacement watch ended before delivering its genuine message: {emitted:?}"
        );
    }
    assert!(
        saw_new_message,
        "the replacement watch never delivered its new message: {emitted:?}"
    );
    assert!(
        !has_message(&emitted, "OLD-WATCH-CONTENT"),
        "an old-watch message was emitted as the new session's event: {emitted:?}"
    );
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, ConversationEvent::WatchEnded)),
        "the old watch's watchEnded terminated the replacement watch: {emitted:?}"
    );
    assert!(
        emitted.iter().any(|event| matches!(
            event,
            ConversationEvent::CommandStatus { command } if command.request_id == sent.request_id
        )),
        "a non-watch command event was dropped with the old watch: {emitted:?}"
    );

    // The replacement watch still ends normally when the app stops it.
    core.stop_watch();
    let mut tail = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline
        && !tail
            .iter()
            .any(|event| matches!(event, ConversationEvent::WatchEnded))
    {
        tail.extend(core.poll_events(200));
    }
    assert_eq!(
        tail.iter()
            .filter(|event| matches!(event, ConversationEvent::WatchEnded))
            .count(),
        1,
        "the new watch's normal watchEnded did not arrive exactly once: {tail:?}"
    );
    assert!(
        !has_message(&tail, "OLD-WATCH-CONTENT"),
        "old content surfaced after the new watch ended: {tail:?}"
    );
}

#[test]
fn identical_session_ids_on_two_authorized_hosts_keep_their_event_scopes() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let mqtt_url = format!("mqtt://{}", listener.local_addr().unwrap());
    spawn_mqtt_peer(&runtime, listener);
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_a = pair_with_mqtt(&core, &access, &mqtt_url);

    // Pair the second Host identity on the same Access profile, then put the
    // fixture back to Host A for the old watch. Both Hosts stay authorized.
    access.host.lock().unwrap().host_id = "host-2".into();
    let host_b = pair_with_mqtt(&core, &access, &mqtt_url);
    assert_eq!(host_a, "host-1");
    assert_eq!(host_b, "host-2");
    access.host.lock().unwrap().host_id = host_a.clone();
    drain_events(&core);

    // Warm Host A's committed cursor so the next watch reports new output.
    core.start_watch(host_a.clone(), "session-1".into())
        .unwrap();
    wait_for_cached_message(&core, &host_a, "session-1", "message-1");
    core.stop_watch();
    drain_events(&core);
    std::thread::sleep(Duration::from_millis(50));

    {
        let mut host = access.host.lock().unwrap();
        host.history_advance = Some((
            vec![history_message("host-a-message", "HOST-A-ONLY")],
            "bookmark:a".into(),
        ));
    }
    core.start_watch(host_a.clone(), "session-1".into())
        .unwrap();
    wait_for_cached_message(&core, &host_a, "session-1", "host-a-message");
    std::thread::sleep(Duration::from_millis(200));
    core.stop_watch();

    // Same session id on Host B. The fixture's WatchReady is bound to Host A,
    // so this replacement watch records its own typed terminal rather than a
    // live message; that is enough to prove Host A's queued output and
    // watchEnded were discarded instead of relabelled.
    access.host.lock().unwrap().host_id = host_b.clone();
    core.start_watch(host_b.clone(), "session-1".into())
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut emitted = Vec::new();
    while Instant::now() < deadline {
        emitted.extend(core.poll_events(200));
    }
    assert!(
        !has_message(&emitted, "HOST-A-ONLY"),
        "Host A's queued message was emitted as Host B's session event: {emitted:?}"
    );
    assert!(
        !emitted.iter().any(|event| matches!(
            event,
            ConversationEvent::Connection { host_id, .. } if host_id == &host_a
        )),
        "Host A's queued connection event surfaced under the Host B watch: {emitted:?}"
    );
    assert!(emitted.iter().any(|event| matches!(
        event,
        ConversationEvent::Connection { host_id, .. } if host_id == &host_b
    )));
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, ConversationEvent::WatchEnded)),
        "the old Host's watchEnded terminated the replacement watch: {emitted:?}"
    );

    // The replacement watch is still alive: stopping it produces a terminal
    // event of its own, and the old Host's content never appears.
    core.stop_watch();
    let mut tail = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline
        && !tail
            .iter()
            .any(|event| matches!(event, ConversationEvent::WatchEnded))
    {
        tail.extend(core.poll_events(200));
    }
    assert_eq!(
        tail.iter()
            .filter(|event| matches!(event, ConversationEvent::WatchEnded))
            .count(),
        1,
        "the replacement watch's own terminal event did not arrive exactly once: {tail:?}"
    );
    assert!(!has_message(&tail, "HOST-A-ONLY"));
}

// ---------------------------------------------------------------------------
// Session observation: Core-owned question view, guard and answer flow.
// ---------------------------------------------------------------------------

fn wait_for_current_questions(
    core: &MobileCore,
    session_id: &str,
) -> whalerein_mobile::QuestionsView {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        for event in core.poll_events(250) {
            if let ConversationEvent::Questions { view } = event {
                if view.freshness == "current"
                    && view
                        .batches
                        .iter()
                        .any(|batch| batch.session_id == session_id)
                {
                    return view;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "no current question view for {session_id} arrived"
        );
    }
}

#[test]
fn question_observation_and_guard_flow_use_core_owned_state() {
    use whalerein_mobile::QuestionAnswerView;

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let mqtt_url = format!("mqtt://{}", listener.local_addr().unwrap());
    spawn_mqtt_peer(&runtime, listener);
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair_with_mqtt(&core, &access, &mqtt_url);
    let session_id = "session-1".to_owned();

    core.start_watch(host_id.clone(), session_id.clone())
        .unwrap();
    let view = wait_for_current_questions(&core, &session_id);
    // Emitted observation state, not just a one-shot read.
    assert_eq!(view.freshness, "current");
    assert!(view.error.is_none());
    let stamp = view.stamp.clone().expect("stamp on a current view");
    assert_eq!(stamp.observation_epoch, "question-epoch-1");
    assert_eq!(stamp.revision, 1);
    let emitted = view
        .batches
        .iter()
        .find(|batch| batch.batch_id == "batch-1")
        .expect("batch-1");
    assert_eq!(emitted.session_id, session_id);
    assert_eq!(emitted.phase, "QUESTION_CONTINUED");
    assert!(emitted.answerable);
    assert!(!emitted.content_unavailable);
    assert_eq!(emitted.digest_hex, "07".repeat(32));
    assert_eq!(emitted.questions.len(), 1);
    assert_eq!(emitted.questions[0].question, "Synthetic question");
    assert_eq!(emitted.questions[0].options[0].label, "A");
    assert_eq!(
        emitted.questions[0].options[0].description.as_deref(),
        Some("Option")
    );

    // One-shot reads share the same authoritative reader and stamp.
    let page = core
        .list_questions(host_id.clone(), session_id.clone(), String::new())
        .unwrap();
    assert!(page.complete);
    assert_eq!(page.next_cursor, "");
    assert_eq!(page.stamp, Some(stamp.clone()));
    assert_eq!(page.batches.len(), 1);
    assert_eq!(page.batches[0].batch_id, "batch-1");

    let detail = core
        .get_question(host_id.clone(), session_id.clone(), "batch-1".into())
        .unwrap();
    assert_eq!(detail.batch_id, "batch-1");
    assert_eq!(detail.digest_hex, emitted.digest_hex);
    assert_eq!(
        core.get_question(host_id.clone(), session_id.clone(), "missing".into())
            .unwrap_err()
            .code(),
        "not_found"
    );

    // A guard that is not a usable 32-byte identity is rejected before any
    // native submission. A well-formed but stale digest stays a native guard:
    // the Host, not the client, revalidates it.
    let mut malformed = emitted.clone();
    malformed.digest_hex = "zz".into();
    assert_eq!(
        core.answer_question(
            host_id.clone(),
            session_id.clone(),
            malformed,
            vec![QuestionAnswerView {
                id: "q".into(),
                selected: vec!["A".into()],
                custom: None,
            }],
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    let mut settled = emitted.clone();
    settled.phase = "QUESTION_SETTLED".into();
    assert_eq!(
        core.answer_question(
            host_id.clone(),
            session_id.clone(),
            settled,
            vec![QuestionAnswerView {
                id: "q".into(),
                selected: vec!["A".into()],
                custom: None,
            }],
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );
    let mut foreign = emitted.clone();
    foreign.session_id = "session-2".into();
    assert_eq!(
        core.answer_question(
            host_id.clone(),
            session_id.clone(),
            foreign,
            vec![QuestionAnswerView {
                id: "q".into(),
                selected: vec!["A".into()],
                custom: None,
            }],
        )
        .unwrap_err()
        .code(),
        "invalid_input"
    );

    // The exact reviewed guard is accepted and queued as a command receipt.
    let answered = core
        .answer_question(
            host_id.clone(),
            session_id.clone(),
            emitted.clone(),
            vec![QuestionAnswerView {
                id: "q".into(),
                selected: vec!["A".into()],
                custom: Some("custom".into()),
            }],
        )
        .unwrap();
    assert!(!answered.command_id.is_empty());
    assert_eq!(answered.status, "succeeded");
    assert!(core.poll_events(0).iter().any(|event| matches!(
        event,
        ConversationEvent::CommandStatus { command } if command.command_id == answered.command_id
    )));
    assert!(core
        .cancel_question_answer(host_id, session_id, answered.command_id)
        .unwrap());

    core.stop_watch();
}

#[test]
fn queued_question_state_from_a_replaced_session_is_not_relabelled() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let mqtt_url = format!("mqtt://{}", listener.local_addr().unwrap());
    spawn_mqtt_peer(&runtime, listener);
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let host_id = pair_with_mqtt(&core, &access, &mqtt_url);
    drain_events(&core);

    // Let the old watch queue both its conversation baseline and a current
    // question view while the consumer stays paused.
    core.start_watch(host_id.clone(), "session-1".into())
        .unwrap();
    std::thread::sleep(Duration::from_millis(900));
    core.stop_watch();
    std::thread::sleep(Duration::from_millis(100));

    // Independent events survive replacement and remain unscoped.
    let sent = core
        .send(
            host_id.clone(),
            "session-1".into(),
            "command while paused".into(),
            DeliveryChoice::Send,
        )
        .unwrap();

    core.start_watch(host_id.clone(), "session-2".into())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut emitted = Vec::new();
    let mut saw_current = false;
    while Instant::now() < deadline {
        emitted.extend(core.poll_events(250));
        saw_current = emitted.iter().any(|event| {
            matches!(
                event,
                ConversationEvent::Questions { view }
                    if view.freshness == "current"
                        && view.batches.iter().any(|batch| batch.session_id == "session-2")
            )
        });
        if saw_current {
            break;
        }
    }
    assert!(
        saw_current,
        "new session never delivered its question view: {emitted:?}"
    );
    assert!(
        !emitted.iter().any(|event| matches!(
            event,
            ConversationEvent::Questions { view }
                if view.batches.iter().any(|batch| batch.session_id == "session-1")
        )),
        "old session's queued question state leaked into the replacement: {emitted:?}"
    );
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, ConversationEvent::WatchEnded)),
        "the replaced watch's terminal event ended the replacement: {emitted:?}"
    );
    assert!(
        emitted.iter().any(|event| matches!(
            event,
            ConversationEvent::CommandStatus { command } if command.request_id == sent.request_id
        )),
        "an independent command event was lost with the replaced watch: {emitted:?}"
    );
    core.stop_watch();
}
