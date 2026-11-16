//! Write a profile fixture with the post-cutover revision for the stopped-client
//! rollback exercise required by task 6.2 of `refactor-core-architecture`.
//!
//! The fixture is only written when `WHALEREIN_ROLLBACK_OUT` is set; normal test
//! runs skip it. The pre-cutover reader lives in the detached worktree at the
//! baseline revision, so the two halves never share the changed Rust API.
//!
//!   WHALEREIN_ROLLBACK_OUT=<dir> cargo test -p whalerein-core \
//!     --features test-support --test rollback_writer -- --nocapture

use std::path::{Path, PathBuf};
use std::sync::Arc;

use whalerein_core::api::{FileSecretStore, PairingDecision};
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
async fn write_post_cutover_rollback_fixture() {
    let Some(out) = std::env::var_os("WHALEREIN_ROLLBACK_OUT").map(PathBuf::from) else {
        eprintln!("skip: WHALEREIN_ROLLBACK_OUT is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("profile");
    std::fs::create_dir_all(&data).unwrap();
    let secrets: Arc<FileSecretStore> = Arc::new(FileSecretStore::new(data.join("file-secrets")));
    let access = whalerein_core::api::test_support::FakeAccess::start().await;
    let core = Core::new(CoreOptions {
        data_dir: data.clone(),
        secrets,
    })
    .unwrap();
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("config.json")).unwrap()).unwrap();
    let host_id = config["host_id"].as_str().unwrap().to_owned();
    let device_id = config["device_id"].as_str().unwrap().to_owned();
    let fingerprint = config["host_fingerprint"].as_str().unwrap_or("").to_owned();
    core.save_draft(&host_id, "session-1", "post-cutover draft")
        .unwrap();
    access.host.lock().unwrap().drop_command_receipts = true;
    assert!(core
        .send_message(
            &whalerein_core::api::SessionKey::new(&host_id, "session-1").unwrap(),
            "post-cutover pending",
        )
        .await
        .is_err());
    let pending = core.pending_commands().unwrap();
    assert!(!pending.is_empty());
    drop(core);

    let target = out.join("profile");
    if target.exists() {
        std::fs::remove_dir_all(&target).unwrap();
    }
    copy_tree(&data, &target).unwrap();
    let record = serde_json::json!({
        "writer": "post-cutover",
        "writer_revision": std::env::var("WHALEREIN_ROLLBACK_REVISION").unwrap_or_default(),
        "device_id": device_id,
        "host_id": host_id,
        "host_fingerprint": fingerprint,
        "draft": {"host_id": host_id, "session_id": "session-1", "text": "post-cutover draft"},
        "pending_request_ids": pending.iter().map(|item| item.request_id.clone()).collect::<Vec<_>>(),
    });
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
}
