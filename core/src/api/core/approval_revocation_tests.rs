use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::PairingDecision;

#[tokio::test]
async fn fake_host_accepts_approval_submissions_then_self_revocation_clears_local_pairing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let approved = core.approve("approval-1", &[0xabu8; 32], 3).await.unwrap();
    assert_eq!(approved.status, OnlineStatus::HostAccepted);
    let rejected = core.reject("approval-2", &[0xcdu8; 32], 4).await.unwrap();
    assert_eq!(rejected.status, OnlineStatus::HostAccepted);
    assert_eq!(access.host.lock().unwrap().commands_seen, 2);
    assert_eq!(core.cache().unwrap().command_counts().unwrap(), (2, 0));
    let revoked = core.revoke().await.unwrap();
    assert_eq!(revoked.device_id, outcome.device_id);
    assert!(!core.is_paired());
    assert!(matches!(core.hosts().await, Err(CoreError::NotPaired)));
    assert!(!dir.path().join("config.json").exists());
    assert!(!dir.path().join("cache.sqlite3").exists());
}
