//! Regression coverage for user-visible Host/CLI defects.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_whalerein"))
}

fn selected(dir: &Path, service: &str) -> PathBuf {
    let config = dir.join("selected.toml");
    std::fs::write(
        &config,
        format!("version = 1\nservice = \"{service}\"\ndata_dir = \"instance\"\n"),
    )
    .unwrap();
    config
}

fn invocation(config: &Path) -> Command {
    let mut command = binary();
    command.arg("--config").arg(config).arg("--json");
    command
}

fn paired_binary() -> (
    tokio::runtime::Runtime,
    tempfile::TempDir,
    PathBuf,
    whalerein_core::api::test_support::FakeAccess,
) {
    use whalerein_core::api::test_support::FakeAccess;

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let access = runtime.block_on(FakeAccess::start());
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let mut pairing = invocation(&config)
        .args(["--yes", "host", "connect", "--invite-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    pairing
        .stdin
        .take()
        .unwrap()
        .write_all(access.pairing_uri().as_bytes())
        .unwrap();
    let paired = pairing.wait_with_output().unwrap();
    assert_eq!(paired.status.code(), Some(0), "{paired:?}");
    (runtime, temp, config, access)
}

/// HOST-4: `device revoke --host` must be rejected on Standalone, where the
/// flag cannot name a Cloud Host scope.
#[test]
fn standalone_device_revoke_rejects_the_named_host() {
    let (_runtime, _temp, config, access) = paired_binary();

    let revoked = invocation(&config)
        .args(["--yes", "device", "revoke", "--host", "host-does-not-exist"])
        .output()
        .unwrap();

    assert_eq!(
        revoked.status.code(),
        Some(2),
        "revoking an unknown Host is a usage error: {revoked:?}"
    );
    let envelope: serde_json::Value = serde_json::from_slice(&revoked.stdout).unwrap();
    assert_eq!(
        envelope["error"]["code"], "invalid_argument",
        "the ignored --host must be reported: {envelope}"
    );
    assert!(
        envelope["data"].is_null(),
        "no revocation is claimed: {envelope}"
    );
    assert!(
        !access.host.lock().unwrap().revoked,
        "the paired Host grant was revoked even though --host named another Host"
    );
}
