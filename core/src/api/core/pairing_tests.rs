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
async fn rejected_device_token_preserves_local_trust_for_later_calls() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    let outcome = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let config_path = dir.path().join("config.json");
    let config_before = std::fs::read(&config_path).unwrap();
    let token = secrets
        .load(&crate::security::keys::device_token(&outcome.device_id))
        .unwrap()
        .unwrap();
    let cache_key = secrets
        .load(&crate::security::keys::cache_key(&outcome.device_id))
        .unwrap()
        .unwrap();
    paired
        .save_draft("host-1", "session-1", "retained draft")
        .unwrap();
    assert_eq!(paired.sessions("host-1").await.unwrap().source, "remote");

    // Access rejects the stored device token: service admission is lost, but
    // no Host confirmed a revocation for this enrollment.
    access.revoke();
    let error = paired.hosts().await.unwrap_err();
    assert!(matches!(error, CoreError::Revoked), "{error:?}");
    assert!(paired.is_paired());
    assert!(!paired.runtime().is_revoked());
    assert_eq!(
        paired.local_revocation_state().unwrap(),
        Some("revocation_pending")
    );
    assert!(paired.authorization_verification_pending().unwrap());
    assert_eq!(std::fs::read(&config_path).unwrap(), config_before);
    assert_eq!(
        secrets
            .load(&crate::security::keys::device_token(&outcome.device_id))
            .unwrap()
            .unwrap(),
        token
    );
    assert_eq!(
        secrets
            .load(&crate::security::keys::cache_key(&outcome.device_id))
            .unwrap()
            .unwrap(),
        cache_key
    );
    assert!(dir.path().join("cache.sqlite3").exists());
    assert_eq!(
        paired.load_draft("host-1", "session-1").unwrap().as_deref(),
        Some("retained draft")
    );

    // Repeated denials neither escalate to a confirmed revocation nor drop
    // the retained identity and cache.
    assert!(matches!(paired.hosts().await, Err(CoreError::Revoked)));
    assert!(matches!(
        paired.sessions("host-1").await,
        Err(CoreError::Revoked)
    ));
    assert!(paired.is_paired());
    assert_eq!(
        paired.local_revocation_state().unwrap(),
        Some("revocation_pending")
    );
    assert!(dir.path().join("cache.sqlite3").exists());

    // A later authorized call reconciles the pending observation without any
    // manual cleanup.
    access.host.lock().unwrap().revoked = false;
    assert_eq!(paired.hosts().await.unwrap().len(), 1);
    assert_eq!(paired.local_revocation_state().unwrap(), None);
    assert!(!paired.authorization_verification_pending().unwrap());
    assert_eq!(paired.sessions("host-1").await.unwrap().source, "remote");
}

#[tokio::test]
async fn forbidden_directory_read_retains_local_trust() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    let outcome = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(paired.sessions("host-1").await.unwrap().source, "remote");
    {
        let mut host = access.host.lock().unwrap();
        host.forbid_hosts = true;
        host.query_error = Some(whalerein_proto::ErrorCode::Forbidden);
    }
    let error = paired.sessions("host-1").await.unwrap_err();
    assert!(matches!(error, CoreError::Revoked), "{error:?}");
    assert!(paired.is_paired());
    assert!(!paired.runtime().is_revoked());
    assert_eq!(
        paired.local_revocation_state().unwrap(),
        Some("revocation_pending")
    );
    assert!(secrets
        .load(&crate::security::keys::device_token(&outcome.device_id))
        .unwrap()
        .is_some());
    assert!(dir.path().join("cache.sqlite3").exists());

    {
        let mut host = access.host.lock().unwrap();
        host.forbid_hosts = false;
        host.query_error = None;
    }
    assert_eq!(paired.hosts().await.unwrap().len(), 1);
    assert_eq!(paired.local_revocation_state().unwrap(), None);
    assert_eq!(
        paired.sessions("host-1").await.unwrap().sessions[0].session_id,
        "session-1"
    );
}

#[tokio::test]
async fn rejected_device_token_on_a_host_command_preserves_pairing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, _) = core(dir.path());
    paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    let error = paired.sessions("host-1").await.unwrap_err();
    assert!(matches!(error, CoreError::Revoked), "{error:?}");
    assert!(paired.is_paired());
    assert!(!paired.runtime().is_revoked());
    assert_eq!(
        paired.local_revocation_state().unwrap(),
        Some("revocation_pending")
    );
    access.host.lock().unwrap().revoked = false;
    assert_eq!(paired.hosts().await.unwrap().len(), 1);
    assert_eq!(paired.local_revocation_state().unwrap(), None);
    assert_eq!(
        paired.sessions("host-1").await.unwrap().sessions[0].session_id,
        "session-1"
    );
}

#[tokio::test]
async fn explicit_revoke_after_service_denial_still_fully_cleans() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    let outcome = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    assert!(matches!(paired.hosts().await, Err(CoreError::Revoked)));
    assert!(paired.is_paired());

    // The user explicitly revokes after the denial. Unlike an ordinary call,
    // this must finish the local cleanup even though Access already rejects
    // the stored token.
    let error = paired.revoke().await.unwrap_err();
    assert!(
        matches!(
            error,
            CoreError::Revoked | CoreError::RevokedRetentionUnverified
        ),
        "{error:?}"
    );
    assert!(!paired.is_paired());
    assert!(paired.runtime().is_revoked());
    assert!(!dir.path().join("config.json").exists());
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(secrets
        .load(&crate::security::keys::device_token(&outcome.device_id))
        .unwrap()
        .is_none());
    assert!(secrets
        .load(&crate::security::keys::cache_key(&outcome.device_id))
        .unwrap()
        .is_none());
    assert_eq!(paired.local_revocation_state().unwrap(), Some("revoked"));

    // The explicit cleanup is restart-safe and a fresh pairing still works.
    let reopened = Core::new(paired.options.clone()).unwrap();
    assert!(!reopened.is_paired());
    reopened
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(reopened.is_paired());
    assert_eq!(reopened.local_revocation_state().unwrap(), None);
}

#[tokio::test]
async fn explicit_forget_after_service_denial_clears_only_local_material() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (paired, secrets) = core(dir.path());
    let outcome = paired
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    access.revoke();
    assert!(matches!(paired.hosts().await, Err(CoreError::Revoked)));
    assert!(paired.authorization_verification_pending().unwrap());

    paired.forget_local_device().unwrap();
    assert!(!paired.is_paired());
    assert!(!dir.path().join("config.json").exists());
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(secrets
        .load(&crate::security::keys::device_token(&outcome.device_id))
        .unwrap()
        .is_none());
    // The unconfirmed observation belonged to the forgotten local profile and
    // must not block pairing again or masquerade as a confirmed revocation.
    assert!(!paired.authorization_verification_pending().unwrap());
    assert_eq!(paired.local_revocation_state().unwrap(), None);

    let reopened = Core::new(paired.options.clone()).unwrap();
    assert!(!reopened.is_paired());
    reopened
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(reopened.is_paired());
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

#[tokio::test]
async fn cancelling_at_the_fingerprint_prompt_then_retrying_creates_one_grant() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let uri = access.pairing_uri();
    let error = core
        .pair(&uri, |_| PairingDecision::reject())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::PairingCancelled));
    {
        let host = access.host.lock().unwrap();
        assert_eq!(host.pair_hello_calls, 1);
        assert_eq!(host.pair_finish_calls, 0, "refusal must not send FINISH");
    }
    assert!(!core.is_paired());
    assert!(core
        .load_pending_pairing(&core.device_identity().unwrap().device_id)
        .unwrap()
        .is_none());
    // The same invitation is still unused, so the retry is the only attempt
    // that reaches the Host FINISH stage.
    let outcome = core
        .pair(&uri, |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(core.is_paired());
    assert_eq!(access.host.lock().unwrap().pair_finish_calls, 1);
    assert_eq!(
        access.host.lock().unwrap().device_id().as_deref(),
        Some(outcome.device_id.as_str())
    );
    assert_eq!(core.sessions("host-1").await.unwrap().sessions.len(), 1);
}

#[tokio::test]
async fn pending_finish_save_failure_blocks_finish_and_leaves_invitation_reusable() {
    use crate::security::{MemorySecretStore, SecretError, SecretStore};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FailingPending {
        inner: MemorySecretStore,
        fail: AtomicBool,
    }
    impl SecretStore for FailingPending {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, SecretError> {
            self.inner.load(key)
        }
        fn save(&self, key: &str, value: &[u8]) -> Result<(), SecretError> {
            if key.starts_with("whalerein:v1:pending-pairing:") && self.fail.load(Ordering::SeqCst)
            {
                return Err(SecretError::Failed);
            }
            self.inner.save(key, value)
        }
        fn delete(&self, key: &str) -> Result<(), SecretError> {
            self.inner.delete(key)
        }
    }
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(FailingPending {
        inner: MemorySecretStore::new(),
        fail: AtomicBool::new(true),
    });
    let core = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let uri = access.pairing_uri();
    let error = core
        .pair(&uri, |_| PairingDecision::approve())
        .await
        .unwrap_err();
    assert!(
        matches!(error, CoreError::Security(_)),
        "pending save failure = {error:?}"
    );
    {
        let host = access.host.lock().unwrap();
        assert_eq!(host.pair_hello_calls, 1);
        assert_eq!(
            host.pair_finish_calls, 0,
            "FINISH was sent without a saved pending intent"
        );
    }
    // The invitation was not consumed: the same URI succeeds once the secret
    // backend recovers, and exactly one FINISH reaches the Host.
    secrets.fail.store(false, Ordering::SeqCst);
    let outcome = core
        .pair(&uri, |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(core.is_paired());
    assert_eq!(access.host.lock().unwrap().pair_finish_calls, 1);
    assert_eq!(
        access.host.lock().unwrap().device_id().as_deref(),
        Some(outcome.device_id.as_str())
    );
}

#[tokio::test]
async fn expired_finish_closes_pending_and_resume_explains_the_next_step() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let primary = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.host_id = "host-2".into();
        host.identity = crate::security::StaticIdentity::generate();
        host.drop_pair_finish_once = true;
    }
    assert!(core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .is_err());
    let pending = core
        .load_pending_pairing(&primary.device_id)
        .unwrap()
        .expect("lost FINISH must retain the pending intent");
    let expired = CoreError::Api {
        code: Some(whalerein_proto::ErrorCode::Expired),
        message: "pairing token expired".into(),
    };
    core.settle_pairing_terminal(&pending, &expired).unwrap();
    assert!(
        core.load_pending_pairing(&primary.device_id)
            .unwrap()
            .is_none(),
        "an expired invitation can never be resumed"
    );
    let error = core.resume_pairing().await.unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert!(error.to_string().contains("already paired"));
}

#[tokio::test]
async fn resume_without_pending_guides_to_pair_and_already_paired_guides_to_revoke() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let error = core.resume_pairing().await.unwrap_err();
    assert_eq!(error.code(), "not_found");
    assert!(error.to_string().contains("whalerein pair"));
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let error = core.resume_pairing().await.unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert!(error.to_string().contains("already paired"));
    assert!(error.to_string().contains("revoke"));
}

#[tokio::test]
async fn pairing_history_is_bounded_and_keeps_the_newest_records() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let primary = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let mut records = Vec::new();
    for index in 0..40 {
        records.push(crate::api::PairingHistoryRecord {
            claim_id: format!("record-{index:02}"),
            host_id: "host-1".into(),
            fingerprint: "f".repeat(64),
            status: "committed_unavailable".into(),
        });
    }
    secrets
        .save(
            &keys::pairing_history(&primary.device_id),
            &serde_json::to_vec(&records).unwrap(),
        )
        .unwrap();
    let bounded = core.pairing_history().unwrap();
    assert_eq!(bounded.len(), 32);
    assert_eq!(bounded.first().unwrap().claim_id, "record-08");
    assert_eq!(bounded.last().unwrap().claim_id, "record-39");
}

#[tokio::test]
async fn new_pairing_drops_resolved_history_for_the_same_host_but_keeps_unresolved_evidence() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let primary = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let resolved = crate::api::PairingHistoryRecord {
        claim_id: "resolved".into(),
        host_id: "host-2".into(),
        fingerprint: "a".repeat(64),
        status: "confirmed".into(),
    };
    let unresolved = crate::api::PairingHistoryRecord {
        claim_id: "unresolved".into(),
        host_id: "host-2".into(),
        fingerprint: "b".repeat(64),
        status: "committed_unavailable".into(),
    };
    secrets
        .save(
            &keys::pairing_history(&primary.device_id),
            &serde_json::to_vec(&vec![resolved, unresolved]).unwrap(),
        )
        .unwrap();
    {
        let mut host = access.host.lock().unwrap();
        host.host_id = "host-2".into();
        host.identity = crate::security::StaticIdentity::generate();
        host.drop_pair_finish_once = true;
    }
    assert!(core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .is_err());
    let history = core.pairing_history().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].claim_id, "unresolved");
}

#[tokio::test]
async fn pairing_history_handles_empty_and_corrupt_state_without_leaking_credentials() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    assert!(core.pairing_history().unwrap().is_empty());
    let primary = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(core.pairing_history().unwrap().is_empty());
    {
        let mut host = access.host.lock().unwrap();
        host.host_id = "host-2".into();
        host.identity = crate::security::StaticIdentity::generate();
        host.unconfirmed_pair_finish_status = Some(409);
    }
    assert_eq!(
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap_err()
            .code(),
        "pairing_committed_unconfirmed"
    );
    let records = core.pairing_history().unwrap();
    assert_eq!(records.len(), 1);
    let pending = core
        .load_pending_pairing(&primary.device_id)
        .unwrap()
        .unwrap();
    let token = pending.finish.device_token.clone();
    assert!(!serde_json::to_string(&records).unwrap().contains(&token));
    let raw = secrets
        .load(&keys::pairing_history(&primary.device_id))
        .unwrap()
        .unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains(&token));
    secrets
        .save(&keys::pairing_history(&primary.device_id), b"not json")
        .unwrap();
    let error = core.pairing_history().unwrap_err();
    assert_eq!(error.code(), "security_error");
}

// AC-38.grant-binding: Core keeps the Host-assigned grant identity.
#[tokio::test]
async fn pairing_persists_the_host_owned_grant_and_sends_it_on_the_next_channel() {
    let access = FakeAccess::start().await;
    access.host.lock().unwrap().pair_grant_id = "grant_01HZX:alpha-1".into();
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(
        outcome.host_grant_id.as_deref(),
        Some("grant_01HZX:alpha-1")
    );
    let config = core.current_config().unwrap();
    assert_eq!(config.host_grant_id.as_deref(), Some("grant_01HZX:alpha-1"));
    assert_eq!(
        config.host_grant_id_for("host-1"),
        Some("grant_01HZX:alpha-1")
    );
    core.ensure_channel("host-1", true).await.unwrap();
    assert_eq!(
        access.host.lock().unwrap().last_hello_grant_id,
        "grant_01HZX:alpha-1"
    );
    // Diagnostics never carry the identifier.
    assert!(!core
        .diagnostics_preview()
        .unwrap()
        .contains("grant_01HZX:alpha-1"));

    // A restart preserves the identity and the next authenticated hello still
    // sends exactly the persisted value.
    drop(core);
    let reopened = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    assert_eq!(
        reopened
            .current_config()
            .unwrap()
            .host_grant_id_for("host-1"),
        Some("grant_01HZX:alpha-1")
    );
    reopened.ensure_channel("host-1", false).await.unwrap();
    assert_eq!(
        access.host.lock().unwrap().last_hello_grant_id,
        "grant_01HZX:alpha-1"
    );

    // Core sends what the profile holds; a stale value is not silently repaired
    // or replaced by a derived label, so the Host can reject it.
    {
        let mut config = reopened.current_config().unwrap();
        config.host_grant_id = Some("stale-grant".into());
        reopened.config_store().save(&config).unwrap();
        *reopened.config.write().unwrap() = Some(config);
    }
    reopened.runtime().clear_channel("host-1");
    reopened.ensure_channel("host-1", true).await.unwrap();
    assert_eq!(
        access.host.lock().unwrap().last_hello_grant_id,
        "stale-grant"
    );
}

#[tokio::test]
async fn a_channel_ready_grant_echo_must_match_the_authenticated_hello() {
    let access = FakeAccess::start().await;
    access.host.lock().unwrap().pair_grant_id = "grant_echo:alpha-1".into();
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    core.ensure_channel("host-1", true).await.unwrap();

    // The Host echoes a different grant than the one the Device authenticated.
    access.host.lock().unwrap().ready_grant_override = Some("grant_echo:other".into());
    core.runtime().clear_channel("host-1");
    let error = core.ensure_channel("host-1", true).await.unwrap_err();
    assert!(matches!(error, CoreError::Security(_)));
    assert!(core.runtime().channel("host-1").is_none());

    // A legacy profile without a grant id is not upgraded by the echo and still
    // completes; the Host stays the authority for what it installed.
    access.host.lock().unwrap().ready_grant_override = None;
}

#[tokio::test]
async fn a_legacy_empty_host_grant_leaves_the_profile_unset_and_sends_nothing() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(outcome.host_grant_id.is_none());
    let config = core.current_config().unwrap();
    assert!(config.host_grant_id.is_none());
    assert_eq!(config.host_grant_id_for("host-1"), None);
    let encoded = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    assert!(!encoded.contains("host_grant"));
    core.ensure_channel("host-1", true).await.unwrap();
    assert!(access.host.lock().unwrap().last_hello_grant_id.is_empty());
}

#[tokio::test]
async fn a_lost_finish_reply_recovers_the_same_host_grant_identity() {
    let access = FakeAccess::start().await;
    {
        let mut host = access.host.lock().unwrap();
        host.pair_grant_id = "grant-stable".into();
        host.drop_pair_finish_once = true;
    }
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets) = core(dir.path());
    assert!(core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .is_err());
    let (operation_id, _) = core.pending_standalone_pairing().unwrap().unwrap();
    // An unconfirmed FINISH persists no profile and therefore no grant id.
    assert!(core.current_config().is_none());
    assert!(!dir.path().join("config.json").exists());
    drop(core);
    let reopened = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    access.host.lock().unwrap().drop_pair_finish_once = false;
    let recovered = reopened
        .recover_standalone_pairing(&operation_id)
        .await
        .unwrap();
    assert_eq!(recovered.host_grant_id.as_deref(), Some("grant-stable"));
    assert_eq!(
        reopened
            .current_config()
            .unwrap()
            .host_grant_id_for("host-1"),
        Some("grant-stable")
    );
    reopened.ensure_channel("host-1", true).await.unwrap();
    assert_eq!(
        access.host.lock().unwrap().last_hello_grant_id,
        "grant-stable"
    );
}

#[tokio::test]
async fn pairing_prompt_carries_only_the_advisory_directory_preview() {
    const DISPLAY_MARKER: &str = "PREPAIR-DISPLAY-MARKER";
    const OS_MARKER: &str = "PREPAIR-OS-MARKER";
    let access = FakeAccess::start().await;
    {
        let mut host = access.host.lock().unwrap();
        host.directory_display_name = DISPLAY_MARKER.into();
        host.directory_os_family = OS_MARKER.into();
    }
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    let seen = Arc::new(Mutex::new(None));
    let captured = seen.clone();
    let outcome = core
        .pair(&access.pairing_uri(), move |prompt| {
            *captured.lock().unwrap() = Some(prompt.clone());
            PairingDecision::approve()
        })
        .await
        .unwrap();
    let prompt = seen.lock().unwrap().clone().expect("prompt");
    assert_eq!(prompt.display_name, DISPLAY_MARKER);
    assert_eq!(prompt.os_family, OS_MARKER);

    // The preview decorates the prompt only. It must not become configuration,
    // pairing state, diagnostics or the pair outcome.
    let config = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    assert!(!config.contains(DISPLAY_MARKER));
    assert!(!config.contains(OS_MARKER));
    let outcome_json = serde_json::to_string(&outcome).unwrap();
    assert!(!outcome_json.contains(DISPLAY_MARKER));
    assert!(!outcome_json.contains(OS_MARKER));
    let snapshot = core.diagnostics_snapshot().unwrap();
    let snapshot = String::from_utf8_lossy(snapshot.bytes()).to_string();
    assert!(!snapshot.contains(DISPLAY_MARKER));
    assert!(!snapshot.contains(OS_MARKER));
    assert!(!directory_tree_contains(dir.path(), DISPLAY_MARKER));
    assert!(!directory_tree_contains(dir.path(), OS_MARKER));
    assert!(core.is_paired());
}

fn directory_tree_contains(root: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if directory_tree_contains(&path, needle) {
                return true;
            }
        } else if let Ok(bytes) = std::fs::read(&path) {
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return true;
            }
        }
    }
    false
}

#[tokio::test]
async fn advisory_directory_failures_keep_pairing_with_empty_preview() {
    #[derive(Clone, Copy)]
    enum Failure {
        Unavailable,
        Malformed,
        Timeout,
    }

    for failure in [Failure::Unavailable, Failure::Malformed, Failure::Timeout] {
        let access = FakeAccess::start().await;
        {
            let mut host = access.host.lock().unwrap();
            // Any preview would be visible in the captured prompt, so a
            // timeout must not wait for a late response that could populate it.
            host.directory_display_name = "late-display".into();
            host.directory_os_family = "late-os".into();
            match failure {
                Failure::Unavailable => host.directory_status = Some(404),
                Failure::Malformed => {
                    host.directory_body = Some(b"{\"hostId\":\"host-1\",\"displayName\"".to_vec())
                }
                Failure::Timeout => host.directory_delay_ms = 500,
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = core(dir.path());
        if matches!(failure, Failure::Timeout) {
            core.set_directory_timeout(Duration::from_millis(50));
        }
        let seen = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let outcome = core
            .pair(&access.pairing_uri(), move |prompt| {
                *captured.lock().unwrap() = Some(prompt.clone());
                PairingDecision::approve()
            })
            .await
            .unwrap_or_else(|error| panic!("pairing must survive directory failure: {error:?}"));
        let prompt = seen.lock().unwrap().clone().expect("prompt");
        assert_eq!(prompt.display_name, "");
        assert_eq!(prompt.os_family, "");
        assert!(core.is_paired());
        assert_eq!(outcome.host_id, "host-1");
    }
}
