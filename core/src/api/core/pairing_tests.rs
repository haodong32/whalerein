use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::PairingDecision;

#[tokio::test]
async fn pair_again_keeps_a_live_grant() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let error = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)));
    assert_eq!(
        access.host.lock().unwrap().device_id(),
        Some(outcome.device_id)
    );
    assert!(core.is_paired());
    assert_eq!(
        core.sessions("host-1").await.unwrap().sessions[0].session_id,
        "session-1"
    );
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
}

#[tokio::test]
async fn a_legacy_profile_without_a_host_pin_never_silently_trusts_a_fresh_channel() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.config
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .host_fingerprint = None;
    assert!(core.doctor().is_ok());
    assert!(matches!(
        core.sessions("host-1").await,
        Err(CoreError::Conflict(_))
    ));
    assert!(core.is_paired());
    assert!(access.host.lock().unwrap().request_traces.is_empty());
}

#[tokio::test]
async fn pair_replaces_a_pairing_the_host_already_revoked() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let first = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    let second = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(first.device_id, second.device_id);
    assert!(core.is_paired());
    let config = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    assert!(config.contains(&second.device_id));
    assert!(!secrets.is_empty());
    assert!(!core.runtime().is_revoked());
    let sessions = core.sessions("host-1").await.unwrap();
    assert_eq!(sessions.sessions[0].session_id, "session-1");
}

#[tokio::test]
async fn pair_keeps_local_material_when_access_cannot_be_checked() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.fail_hosts(true);
    let error = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap_err();
    assert!(!error.device_authorization_lost());
    assert!(core.is_paired());
    let config = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    assert!(config.contains(&outcome.device_id));
    assert!(!core.runtime().is_revoked());
    access.fail_hosts(false);
    assert_eq!(
        core.sessions("host-1").await.unwrap().sessions[0].session_id,
        "session-1"
    );
}

#[tokio::test]
async fn revoke_clears_local_material_after_the_host_revoked_first() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    let error = core.revoke().await.unwrap_err();
    assert!(matches!(
        error,
        CoreError::Revoked | CoreError::RevokedRetentionUnverified
    ));
    assert_ne!(outcome.device_id, "");
    assert!(!core.is_paired());
    assert!(!secrets.is_empty());
    assert!(!dir.path().join("config.json").exists());
    assert!(core.runtime().is_revoked());
}

#[tokio::test]
async fn rejected_device_token_ends_local_pairing_for_later_commands() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    let error = paired.hosts().await.unwrap_err();
    assert!(matches!(
        error,
        CoreError::Revoked | CoreError::RevokedRetentionUnverified
    ));
    assert!(!paired.is_paired());
    assert!(!secrets.is_empty());
    assert!(paired.runtime().is_revoked());
    assert!(matches!(paired.hosts().await, Err(CoreError::NotPaired)));
}

#[tokio::test]
async fn rejected_device_token_ends_pairing_on_a_host_command() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, _) = core(dir.path());
    paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    let error = paired.sessions("host-1").await.unwrap_err();
    assert!(matches!(
        error,
        CoreError::Revoked | CoreError::RevokedRetentionUnverified
    ));
    assert!(!paired.is_paired());
}

#[tokio::test]
async fn host_rejection_with_unavailable_access_confirmation_preserves_pairing_and_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    let outcome = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let before = std::fs::read(dir.path().join("config.json")).unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.query_error = Some(whalerein_proto::ErrorCode::Unauthorized);
        host.fail_hosts = true;
    }
    assert!(paired.sessions("host-1").await.is_err());
    assert!(paired.is_paired());
    assert!(!paired.runtime().is_revoked());
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        before
    );
    assert!(!secrets.is_empty());
    {
        let mut host = access.host.lock().unwrap();
        host.query_error = None;
        host.fail_hosts = false;
        assert_eq!(host.device_id(), Some(outcome.device_id));
    }
    assert_eq!(
        paired.sessions("host-1").await.unwrap().sessions[0].session_id,
        "session-1"
    );
}

#[tokio::test]
async fn revoke_before_pairing_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let error = core.revoke().await.unwrap_err();
    assert!(matches!(error, CoreError::NotPaired));
    assert!(secrets.is_empty());
    assert!(!dir.path().join("config.json").exists());
}

#[tokio::test]
async fn global_revoke_succeeds_with_existing_identity_locked() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    let outcome = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let other = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let _identity_lock = crate::security::device::ProfileLock::acquire(dir.path()).unwrap();
    let result = other.revoke().await;
    assert!(result.is_ok(), "revoke failed: {result:?}");
    assert!(secrets
        .load(&crate::security::keys::device_token(&outcome.device_id))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn global_cleanup_rejects_mismatched_existing_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, _) = core(dir.path());
    paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let error = paired.clear_local_pairing("different-device").unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_cleanup_does_not_erase_recreated_profile() {
    use crate::security::{MemorySecretStore, SecretError, SecretStore};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier,
    };
    struct PausingStore {
        inner: Arc<MemorySecretStore>,
        armed: AtomicBool,
        read: Barrier,
        resume: Barrier,
    }
    impl SecretStore for PausingStore {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
            let value = self.inner.load(key)?;
            if key.starts_with("whalerein:v2:identity:") && self.armed.swap(false, Ordering::SeqCst)
            {
                self.read.wait();
                self.resume.wait();
            }
            Ok(value)
        }
        fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
            self.inner.save(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.inner.delete(key)
        }
    }
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let paired = Core::new(CoreOptions {
        data_dir: dir.path().into(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let original = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let gate = Arc::new(PausingStore {
        inner: secrets.clone(),
        armed: AtomicBool::new(false),
        read: Barrier::new(2),
        resume: Barrier::new(2),
    });
    let stale = Core::new(CoreOptions {
        data_dir: dir.path().into(),
        secrets: gate.clone(),
    })
    .unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    let old_id = original.device_id.clone();
    let cleanup = tokio::task::spawn_blocking(move || stale.clear_local_pairing(&old_id));
    let wait = gate.clone();
    tokio::task::spawn_blocking(move || wait.read.wait())
        .await
        .unwrap();
    paired.forget_local_device().unwrap();
    let fresh = Core::new(CoreOptions {
        data_dir: dir.path().into(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let next = fresh
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_ne!(original.device_id, next.device_id);
    let release = gate.clone();
    tokio::task::spawn_blocking(move || release.resume.wait())
        .await
        .unwrap();
    let _ = cleanup.await.unwrap();
    assert!(
        dir.path().join("config.json").exists(),
        "stale cleanup erased the new profile"
    );
    assert!(secrets
        .load(&crate::security::keys::device_token(&next.device_id))
        .unwrap()
        .is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn old_async_cleanup_does_not_fence_recreated_identity() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (old, secrets) = core(dir.path());
    let first = old
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    old.forget_local_device().unwrap();
    let fresh = Core::new(CoreOptions {
        data_dir: dir.path().into(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let next = fresh
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_ne!(first.device_id, next.device_id);
    let marker = "pending:revoke:0123456789abcdef0123456789abcdef";
    let admission = crate::security::device::ProfileLock::acquire_admission(dir.path()).unwrap();
    revoked_commands::write_marker_in(admission.directory(), marker).unwrap();
    drop(admission);
    let result = old
        .clear_local_pairing_async_with_timeout(&first.device_id, marker, Duration::from_secs(1))
        .await;
    assert!(result.is_err());
    assert_eq!(
        revoked_commands::marker(dir.path()).unwrap().as_deref(),
        Some(marker)
    );
    assert!(dir.path().join("config.json").exists());
    assert!(secrets
        .load(&crate::security::keys::device_token(&next.device_id))
        .unwrap()
        .is_some());
}
