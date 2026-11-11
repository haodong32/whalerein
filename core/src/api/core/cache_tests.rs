use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::OnlineStatus;
use crate::api::PairingDecision;

#[tokio::test]
async fn returns_runtime_state_and_populates_host_session_and_event_cache() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    assert_eq!(core.data_dir(), dir.path());
    assert_eq!(
        core.get_connection_state("host-1"),
        ConnectionState::Disconnected
    );
    assert!(core.metrics().is_empty());
    assert!(format!("{:?}", core.options).contains("CoreOptions"));
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(core.access_address(), Some(access.url().to_string()));

    // get_host before hosts() exercises the network fallback.
    let host = core.get_host("host-1").await.unwrap();
    assert_eq!(host.host_id, "host-1");
    assert!(matches!(
        core.get_host("missing").await,
        Err(CoreError::NotFound(_))
    ));
    assert!(core.is_paired());

    let hosts = core.list_hosts().await.unwrap();
    assert_eq!(hosts.len(), 1);
    let sessions = core.list_sessions("host-1").await.unwrap();
    assert_eq!(sessions.sessions.len(), 1);
    assert!(!core.runtime().metrics().is_empty());
    assert_eq!(core.cached_sessions("host-1").unwrap().len(), 1);
    let emitted = core.sync_session("session-1").await.unwrap();
    assert_eq!(emitted, 1);
    assert_eq!(
        core.cached_messages(&core.session_key("session-1").unwrap())
            .unwrap()[0]
            .message_id,
        "message-1"
    );
    assert_eq!(
        core.approval_digest_hex("a", "s", "Write", "c", "r", 1, 2)
            .len(),
        64
    );
    let config = core.current_config().unwrap();
    let settings = core.mqtt_settings(&config).unwrap();
    assert_eq!(settings.port, 1883);
    assert!(!settings.tls);
    let session = core.get_session("session-1").await.unwrap();
    assert_eq!(session.title, "test session");
}

#[tokio::test]
async fn sessions_fall_back_to_the_cache_when_the_host_is_offline() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let _ = core.sessions("host-1").await.unwrap();
    access.host.lock().unwrap().host_online = false;
    let sessions = core.sessions("host-1").await.unwrap();
    assert_eq!(sessions.source, "cache");
    assert!(!sessions.complete);
    assert_eq!(sessions.sessions[0].session_id, "session-1");
}

#[tokio::test]
async fn send_without_a_cached_session_uses_the_paired_host() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let handle = core.send_message("session-1", "hello").await.unwrap();
    assert_eq!(handle.status, OnlineStatus::Succeeded);
    let report = core.doctor().unwrap();
    assert_eq!(report.commands.non_terminal, 0);
}

#[tokio::test]
async fn pairing_initializes_one_cache_key_for_concurrent_core_instances() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (first, secrets) = core(dir.path());
    first
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let key_name = keys::cache_key(&first.require_config().unwrap().device_id);
    let before = secrets.load(&key_name).unwrap().unwrap();
    assert_eq!(before.len(), 32);
    let second = Core::new(first.options.clone()).unwrap();
    let options = first.options.clone();
    let a = std::thread::spawn(move || {
        first.sync_service().unwrap().store_sessions(
            "host-a",
            &[SessionSummary {
                session_id: "s".into(),
                ..Default::default()
            }],
            "remote",
            true,
        )
    });
    let b = std::thread::spawn(move || {
        second.sync_service().unwrap().store_sessions(
            "host-b",
            &[SessionSummary {
                session_id: "s".into(),
                ..Default::default()
            }],
            "remote",
            true,
        )
    });
    assert!(a.join().unwrap().is_ok());
    assert!(b.join().unwrap().is_ok());
    assert_eq!(secrets.load(&key_name).unwrap().unwrap(), before);
    let reopened = Core::new(options).unwrap();
    assert_eq!(reopened.cached_sessions("host-a").unwrap().len(), 1);
    assert_eq!(reopened.cached_sessions("host-b").unwrap().len(), 1);
}

#[tokio::test]
async fn missing_legacy_cache_key_cannot_be_created_without_the_profile_lock() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (client, secrets) = core(dir.path());
    client
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let name = keys::cache_key(&client.require_config().unwrap().device_id);
    secrets.delete(&name).unwrap();
    let guard = client.identity_lock().unwrap();
    assert!(matches!(
        client.cached_sessions("host-1"),
        Err(CoreError::Conflict(_))
    ));
    assert!(secrets.load(&name).unwrap().is_none());
    drop(guard);
    assert!(client.cached_sessions("host-1").is_ok());
}

#[tokio::test]
async fn send_waits_through_an_accepted_result_until_the_host_settles() {
    let access = FakeAccess::start().await;
    access.host.lock().unwrap().pending_result_polls = 1;
    let dir = tempfile::tempdir().unwrap();
    let (client, _) = core(dir.path());
    client
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let result = client.send_message("session-1", "").await.unwrap();
    assert_eq!(result.status, OnlineStatus::Succeeded);
    assert!(result.message_id.is_some());
    assert_eq!(access.host.lock().unwrap().pending_result_polls, 0);
}

#[tokio::test]
async fn an_accepted_send_returns_the_later_failure_instead_of_success() {
    let access = FakeAccess::start().await;
    {
        let mut host = access.host.lock().unwrap();
        host.pending_result_polls = 1;
        host.settled_command_status = whalerein_proto::OnlineStatus::Failed;
    }
    let dir = tempfile::tempdir().unwrap();
    let (client, _) = core(dir.path());
    client
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(client.send_message("session-1", "").await.is_err());
    assert_eq!(access.host.lock().unwrap().pending_result_polls, 0);
}
