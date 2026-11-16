//! Reopen a synthetic profile written by the pre-cutover writer.
//!
//! This is the AC-04.baseline evidence procedure for
//! `refactor-core-architecture`. The fixture is generated from a detached
//! worktree at the pre-cutover revision and lives in the ignored acceptance
//! directory; there is no checked-in binary fixture and no real credential.
//!
//! Run:
//!   WHALEREIN_BASELINE_DIR=.local-docs/acceptance/refactor-core-architecture/baseline \
//!     cargo test -p whalerein-core --test baseline_profile -- --nocapture
//!
//! Without the environment variable the case reports a skip and does nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use whalerein_core::api::{FileSecretStore, SessionKey};
use whalerein_core::{Core, CoreOptions};

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn pre_cutover_profile_reopens_with_identity_pins_drafts_and_pending_metadata() {
    let Some(root) = std::env::var_os("WHALEREIN_BASELINE_DIR").map(PathBuf::from) else {
        eprintln!("skip: WHALEREIN_BASELINE_DIR is not set (AC-04.baseline fixture absent)");
        return;
    };
    let source = root.join("profile");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let data = scratch.path().join("profile");
    copy_tree(&source, &data).unwrap();

    let secrets: Arc<FileSecretStore> = Arc::new(FileSecretStore::new(data.join("file-secrets")));
    let core = Core::new(CoreOptions {
        data_dir: data.clone(),
        secrets,
    })
    .expect("the pre-cutover profile must reopen");

    assert!(core.is_paired(), "pairing must survive the cutover");
    assert_eq!(core.data_dir(), data.as_path());
    assert!(
        core.cache_recovery_reason().unwrap().is_none(),
        "the versioned cache must open without quarantine"
    );

    // Identity and pins are unchanged.
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["device_id"], manifest["device_id"]);
    assert_eq!(config["host_id"], manifest["host_id"]);
    assert_eq!(config["host_fingerprint"], manifest["host_fingerprint"]);

    // Draft text is still sealed under the same profile cache key.
    let draft_host = manifest["draft"]["host_id"].as_str().unwrap();
    let draft_session = manifest["draft"]["session_id"].as_str().unwrap();
    assert_eq!(
        core.load_draft(draft_host, draft_session)
            .unwrap()
            .as_deref(),
        manifest["draft"]["text"].as_str()
    );

    // The uncertain command intent recorded by the old writer still identifies
    // one recoverable request for the same device, Host and pin.
    let pending = core.pending_commands().unwrap();
    let expected: Vec<&str> = manifest["pending_request_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(
        pending
            .iter()
            .map(|item| item.request_id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    let item = &pending[0];
    assert_eq!(item.device_id, manifest["device_id"].as_str().unwrap());
    assert_eq!(item.host_id, manifest["host_id"].as_str().unwrap());
    assert_eq!(
        item.host_fingerprint,
        manifest["host_fingerprint"].as_str().unwrap()
    );

    // Cached session rows decrypt through the same cache provenance.
    let key = SessionKey::new(draft_host, draft_session).unwrap();
    assert!(core.cached_messages(&key).is_ok());
    assert!(core.cached_sessions(draft_host).is_ok());
}
