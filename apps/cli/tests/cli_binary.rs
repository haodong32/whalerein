//! Verify the executable's public startup and command contracts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

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

fn outcome(result: &Output, code: i32) -> Value {
    assert_eq!(result.status.code(), Some(code), "{result:?}");
    let stdout = std::str::from_utf8(&result.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{result:?}");
    let value: Value = serde_json::from_str(stdout).unwrap();
    assert_eq!(value["schema"], "whalerein/client/v1");
    value
}

#[test]
fn fresh_cloud_status_only_reads_selected_file() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "whalerein_cloud");
    let other = tempfile::tempdir().unwrap();
    let result = invocation(&config)
        .env("WHALEREIN_CONFIG", "/nonexistent.toml")
        .env("WHALEREIN_DATA_DIR", other.path())
        .env("WHALEREIN_ENVIRONMENT", "staging")
        .current_dir(other.path())
        .arg("status")
        .output()
        .unwrap();
    let value = outcome(&result, 0);
    assert_eq!(value["command"], "status");
    assert_eq!(value["data"]["service"], "whalerein_cloud");
    assert_eq!(value["data"]["account_state"], "signed_out");
    assert_eq!(value["data"]["device_access"], "sign_in_required");
    assert_eq!(value["data"]["remote_signout_pending"], false);
    assert!(value["data"]["testing_environment"].is_null());
    assert!(value["data"]["instance_id"].is_null());
    assert_eq!(
        Path::new(value["data"]["data_dir"].as_str().unwrap()),
        temp.path().canonicalize().unwrap().join("instance")
    );
    assert!(!temp.path().join("instance").exists());
    assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
}

#[test]
fn a_cloud_host_list_never_falls_back_to_a_standalone_directory() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "whalerein_cloud");
    let result = invocation(&config).arg("host").arg("list").output().unwrap();
    let value = outcome(&result, 4);
    assert_eq!(value["command"], "host.list");
    assert_eq!(value["error"]["code"], "unauthorized");
    assert!(value["data"].is_null(), "no Standalone host list is fabricated");
    assert!(!temp.path().join("instance").exists());
}

#[test]
fn default_path_uses_the_platform_home() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".whalerein");
    std::fs::create_dir(&directory).unwrap();
    let config = selected(&directory, "standalone");
    std::fs::rename(config, directory.join("config.toml")).unwrap();
    let result = binary()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .args(["--json", "status"])
        .output()
        .unwrap();
    let value = outcome(&result, 0);
    assert_eq!(value["data"]["account_state"], "not_applicable");
    assert!(!directory.join("instance").exists());
}

#[test]
fn selecting_a_missing_file_never_falls_back_or_initializes() {
    let temp = tempfile::tempdir().unwrap();
    let other = selected(temp.path(), "standalone");
    let missing = temp.path().join("missing.toml");
    let result = invocation(&missing)
        .env("WHALEREIN_CONFIG", other)
        .arg("status")
        .output()
        .unwrap();
    let value = outcome(&result, 2);
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("missing.toml"));
    assert!(!missing.exists());
    assert!(!temp.path().join("instance").exists());
}

#[test]
fn rejects_unknown_config_and_preserves_a_legacy_directory() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let dir = temp.path().join("instance");
    std::fs::create_dir(&dir).unwrap();
    let old_file = dir.join("config.json");
    std::fs::write(&old_file, b"legacy fixture").unwrap();
    let value = outcome(&invocation(&config).arg("status").output().unwrap(), 2);
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("fresh data_dir"));
    assert_eq!(std::fs::read(&old_file).unwrap(), b"legacy fixture");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

    std::fs::write(
        &config,
        "version=1\nservice='standalone'\ndata_dir='new'\nendpoint='https://example.test'\n",
    )
    .unwrap();
    outcome(&invocation(&config).arg("status").output().unwrap(), 2);
    assert!(!temp.path().join("new").exists());
}

#[test]
fn help_discovers_the_full_journey_and_leaf_host_placement() {
    let help = binary().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for fragment in [
        "login",
        "host list",
        "host connect",
        "session list",
        "session send",
        "--invite-stdin",
    ] {
        assert!(help.contains(fragment), "{fragment}: {help}");
    }
    let help = binary()
        .args(["--json", "session", "send", "--help"])
        .output()
        .unwrap();
    let value = outcome(&help, 0);
    let text = value["data"]["help"].as_str().unwrap();
    assert!(text.contains("--host"));
    assert!(text.contains("--detach"));
}

#[test]
fn removed_surfaces_are_usage_errors() {
    for args in [
        vec!["config", "show"],
        vec!["profile", "current"],
        vec!["account-status"],
        vec!["pair", "-"],
        vec!["hosts"],
        vec!["send", "--session", "s", "text"],
        vec!["--environment", "local", "status"],
        vec!["--data-dir", "legacy", "status"],
        vec!["--host", "h", "session", "list"],
        vec!["session", "--host", "h", "list"],
    ] {
        let result = binary().arg("--json").args(&args).output().unwrap();
        let value = outcome(&result, 2);
        assert_eq!(value["error"]["code"], "invalid_argument", "{args:?}");
    }
}

#[test]
fn invitation_stdin_is_exclusive_and_redacted() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let mut child = invocation(&config)
        .args(["--yes", "host", "connect", "--invite-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let fixture = b"not-a-valid-invitation-fixture";
    child.stdin.take().unwrap().write_all(fixture).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(!result
        .stdout
        .windows(fixture.len())
        .any(|part| part == fixture));
    assert!(!result
        .stderr
        .windows(fixture.len())
        .any(|part| part == fixture));
}

#[test]
fn machine_usage_errors_do_not_echo_positional_payloads() {
    let result = binary()
        .args([
            "--json",
            "session",
            "send",
            "s",
            "fixture-payload",
            "unexpected",
        ])
        .output()
        .unwrap();
    let value = outcome(&result, 2);
    assert_eq!(value["error"]["code"], "invalid_argument");
    assert!(!String::from_utf8_lossy(&result.stdout).contains("fixture-payload"));
}

#[test]
fn fresh_operation_list_does_not_initialize_state() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let result = invocation(&config)
        .args(["operation", "list"])
        .output()
        .unwrap();
    let value = outcome(&result, 0);
    assert_eq!(value["data"]["sends"], serde_json::json!([]));
    assert!(!temp.path().join("instance").exists());
}

#[test]
fn version_is_available_without_reading_configuration() {
    let config = tempfile::tempdir().unwrap().path().join("missing.toml");
    let formatted = invocation(&config).arg("--version").output().unwrap();
    let value = outcome(&formatted, 0);
    assert!(value["data"]["version"]
        .as_str()
        .unwrap()
        .starts_with("whalerein "));
    let plain = binary()
        .arg("--config")
        .arg(config)
        .arg("--version")
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert!(String::from_utf8(plain.stdout)
        .unwrap()
        .starts_with("whalerein "));
}

#[test]
fn plain_usage_diagnostics_use_stderr_and_redact_extra_arguments() {
    let result = binary()
        .args([
            "session",
            "send",
            "id",
            "fixture-private-marker",
            "unexpected",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains("--help"));
    assert!(!stderr.contains("fixture-private-marker"));
}
