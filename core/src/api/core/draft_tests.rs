use super::test_support::core;
use super::*;
use crate::api::test_support::FakeAccess;
use crate::api::{MemorySecretStore, PairingDecision};
use std::sync::Arc;

async fn paired(dir: &std::path::Path) -> (Core, Arc<MemorySecretStore>, FakeAccess) {
    let access = FakeAccess::start().await;
    let (core, secrets) = core(dir);
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    (core, secrets, access)
}

#[tokio::test]
async fn drafts_roundtrip_overwrite_and_accept_unicode_and_the_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    assert_eq!(core.load_draft("host-1", "session-1").unwrap(), None);
    core.save_draft("host-1", "session-1", "first").unwrap();
    assert_eq!(
        core.load_draft("host-1", "session-1").unwrap().as_deref(),
        Some("first")
    );
    core.save_draft("host-1", "session-1", "replacement")
        .unwrap();
    assert_eq!(
        core.load_draft("host-1", "session-1").unwrap().as_deref(),
        Some("replacement")
    );

    let unicode = "draft with unicode: \u{1f40b} \u{4f60}\u{597d} \u{00e9}\nsecond line";
    core.save_draft("host-1", "unicode", unicode).unwrap();
    assert_eq!(
        core.load_draft("host-1", "unicode").unwrap().as_deref(),
        Some(unicode)
    );

    let largest = "x".repeat(crate::drafts::MAX_DRAFT_BYTES);
    core.save_draft("host-1", "large", &largest).unwrap();
    assert_eq!(
        core.load_draft("host-1", "large").unwrap().unwrap().len(),
        largest.len()
    );
}

#[tokio::test]
async fn draft_plaintext_is_absent_from_the_cache_file() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    let marker = "PLAINTEXT-DRAFT-MARKER-7f3c9e1a";
    core.save_draft("host-1", "session-1", marker).unwrap();
    let path = dir.path().join("cache.sqlite3");
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes
        .windows(marker.len())
        .any(|window| window == marker.as_bytes()));
    assert!(!String::from_utf8_lossy(&bytes).contains(marker));
}

#[tokio::test]
async fn drafts_are_isolated_by_host_and_session_and_clear_independently() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    core.save_draft("host-1", "session-1", "host one first")
        .unwrap();
    core.save_draft("host-1", "session-2", "host one second")
        .unwrap();
    core.save_draft("host-2", "session-1", "host two first")
        .unwrap();
    assert_eq!(
        core.load_draft("host-1", "session-1").unwrap().as_deref(),
        Some("host one first")
    );
    assert_eq!(
        core.load_draft("host-2", "session-1").unwrap().as_deref(),
        Some("host two first")
    );
    assert_eq!(core.load_draft("host-2", "session-2").unwrap(), None);

    core.clear_draft("host-1", "session-1").unwrap();
    assert_eq!(core.load_draft("host-1", "session-1").unwrap(), None);
    assert!(core.load_draft("host-1", "session-2").unwrap().is_some());
    assert!(core.load_draft("host-2", "session-1").unwrap().is_some());
    core.clear_draft("host-1", "missing").unwrap();

    core.clear_host_drafts("host-1").unwrap();
    assert_eq!(core.load_draft("host-1", "session-2").unwrap(), None);
    assert_eq!(
        core.load_draft("host-2", "session-1").unwrap().as_deref(),
        Some("host two first")
    );
    core.clear_host_drafts("missing").unwrap();
}

#[tokio::test]
async fn empty_text_deletes_a_draft() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    core.save_draft("host-1", "session-1", "to be deleted")
        .unwrap();
    core.save_draft("host-1", "session-1", "").unwrap();
    assert_eq!(core.load_draft("host-1", "session-1").unwrap(), None);
}

#[tokio::test]
async fn drafts_survive_a_core_restart_with_the_same_secret_store() {
    let dir = tempfile::tempdir().unwrap();
    let (core, secrets, _access) = paired(dir.path()).await;
    core.save_draft("host-1", "session-1", "persisted draft")
        .unwrap();
    drop(core);
    let reopened = Core::new(CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    assert_eq!(
        reopened
            .load_draft("host-1", "session-1")
            .unwrap()
            .as_deref(),
        Some("persisted draft")
    );
    assert_eq!(reopened.load_draft("host-1", "other").unwrap(), None);
}

#[tokio::test]
async fn confirmed_revocation_removes_all_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    core.save_draft("host-1", "session-1", "draft").unwrap();
    core.revoke().await.unwrap();
    assert!(!dir.path().join("cache.sqlite3").exists());
    let error = core.load_draft("host-1", "session-1").unwrap_err();
    assert!(matches!(
        error,
        CoreError::NotPaired | CoreError::Revoked | CoreError::RevokedRetentionUnverified
    ));
}

#[tokio::test]
async fn forget_device_removes_all_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    core.save_draft("host-1", "session-1", "draft").unwrap();
    // A confirmed remote revocation is represented by runtime state; forget
    // then removes the remaining local profile material without a network call.
    core.runtime().revoke();
    core.forget_device().unwrap();
    assert!(!dir.path().join("cache.sqlite3").exists());
    assert!(matches!(
        core.load_draft("host-1", "session-1"),
        Err(CoreError::NotPaired)
    ));
}

#[tokio::test]
async fn drafts_reject_oversized_text_and_empty_identities() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _, _access) = paired(dir.path()).await;
    let oversized = "x".repeat(crate::drafts::MAX_DRAFT_BYTES + 1);
    for error in [
        core.save_draft("host-1", "session-1", &oversized),
        core.save_draft("", "session-1", "text"),
        core.save_draft("   ", "session-1", "text"),
        core.save_draft("host-1", "", "text"),
        core.save_draft("host-1", " ", "text"),
        core.clear_draft("", "session-1"),
        core.clear_draft("host-1", " "),
        core.clear_host_drafts(""),
        core.clear_host_drafts("  "),
    ] {
        assert!(matches!(error, Err(CoreError::InvalidInput(_))));
    }
    assert!(matches!(
        core.load_draft("", "session-1"),
        Err(CoreError::InvalidInput(_))
    ));
    assert!(matches!(
        core.load_draft("host-1", ""),
        Err(CoreError::InvalidInput(_))
    ));
    assert_eq!(core.load_draft("host-1", "session-1").unwrap(), None);
}

#[tokio::test]
async fn drafts_require_a_configured_profile() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    assert!(matches!(
        core.save_draft("host-1", "session-1", "text"),
        Err(CoreError::NotPaired)
    ));
    assert!(matches!(
        core.load_draft("host-1", "session-1"),
        Err(CoreError::NotPaired)
    ));
    assert!(matches!(
        core.clear_draft("host-1", "session-1"),
        Err(CoreError::NotPaired)
    ));
    assert!(matches!(
        core.clear_host_drafts("host-1"),
        Err(CoreError::NotPaired)
    ));
}
