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
fn verbose_diagnostics_preserve_json_output_and_stay_on_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let quiet = invocation(&config).arg("status").output().unwrap();
    let expected = outcome(&quiet, 0);
    assert!(quiet.stderr.is_empty());

    for args in [["--verbose", "status"], ["status", "-v"]] {
        let result = invocation(&config).args(args).output().unwrap();
        assert_eq!(outcome(&result, 0), expected);
        let stderr = String::from_utf8(result.stderr).unwrap();
        assert!(!stderr.contains("selected.toml"));
        let events: Vec<Value> = stderr
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events[0]["event"], "command_started");
        assert_eq!(events[0]["configuration_source"], "explicit");
        assert_eq!(events[1]["event"], "configuration_loaded");
        assert_eq!(events[1]["level"], "info");
        let trace_id = events[1]["trace_id"].as_str().unwrap();
        assert_eq!(trace_id.len(), 32);
        assert!(events.iter().all(|event| event["trace_id"] == trace_id));
        assert_eq!(events[2]["event"], "command_completed");
        assert_eq!(events[2]["exit_code"], 0);
        assert_eq!(events[2]["outcome_exit_code"], 0);
    }
}

#[test]
fn verbose_diagnostics_cover_configuration_failure_and_text_output() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("private-fixture-name.toml");
    let result = invocation(&missing)
        .args(["--verbose", "status"])
        .output()
        .unwrap();
    let value = outcome(&result, 2);
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(!stderr.contains("private-fixture-name"));
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[1]["event"], "configuration_failed");
    assert_eq!(events[1]["code"], value["error"]["code"]);
    assert_eq!(events[2]["exit_code"], 2);
    let trace_id = events[1]["trace_id"].as_str().unwrap();
    assert!(events.iter().all(|event| event["trace_id"] == trace_id));

    let config = selected(temp.path(), "standalone");
    let result = binary()
        .arg("--config")
        .arg(config)
        .args(["status", "--verbose"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8(result.stderr)
        .unwrap()
        .contains("INFO configuration_loaded"));
    assert!(!String::from_utf8(result.stdout)
        .unwrap()
        .contains("configuration_loaded"));
}

#[test]
fn verbose_diagnostics_correlate_missing_home_before_core_opens() {
    let result = binary()
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .args(["--verbose", "--json", "status"])
        .output()
        .unwrap();
    let value = outcome(&result, 2);
    let stderr = String::from_utf8(result.stderr).unwrap();
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["event"], "command_started");
    assert_eq!(events[1]["event"], "configuration_failed");
    assert_eq!(events[1]["code"], value["error"]["code"]);
    assert_eq!(events[2]["event"], "command_completed");
    let trace_id = events[0]["trace_id"].as_str().unwrap();
    assert!(events.iter().all(|event| event["trace_id"] == trace_id));
}

#[cfg(unix)]
#[test]
fn verbose_diagnostics_report_the_process_exit_after_a_closed_stdout() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let temp = tempfile::tempdir().unwrap();
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let result = invocation(&temp.path().join("missing.toml"))
        .args(["--verbose", "status"])
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(0));
    let stderr = String::from_utf8(result.stderr).unwrap();
    let completion: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
    assert_eq!(completion["event"], "command_completed");
    assert_eq!(completion["exit_code"], 0);
    assert_eq!(completion["outcome_exit_code"], 2);
}

#[cfg(unix)]
#[test]
fn verbose_diagnostics_exit_even_with_a_blocked_stderr() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    for bytes in [&[0; 4096][..], &[0][..]] {
        loop {
            match writer.write(bytes) {
                Ok(count) => assert_ne!(count, 0),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("could not fill stderr fixture: {error}"),
            }
        }
    }
    writer.set_nonblocking(false).unwrap();

    let mut child = invocation(&config)
        .args(["--verbose", "status"])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(OwnedFd::from(writer)))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("command did not exit with stderr blocked");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = child.wait_with_output().unwrap();
    drop(reader);
    assert_eq!(outcome(&result, 0)["command"], "status");
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
    let result = invocation(&config)
        .arg("host")
        .arg("list")
        .output()
        .unwrap();
    let value = outcome(&result, 4);
    assert_eq!(value["command"], "host.list");
    assert_eq!(value["error"]["code"], "unauthorized");
    assert!(
        value["data"].is_null(),
        "no Standalone host list is fabricated"
    );
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
        "--verbose",
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

#[test]
fn help_discovers_reasoning_history_and_download_surface() {
    let help = binary().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("--show-reasoning"), "{help}");
    let session_help = binary()
        .args(["--json", "session", "--help"])
        .output()
        .unwrap();
    let session_help = outcome(&session_help, 0)["data"]["help"]
        .as_str()
        .unwrap()
        .to_owned();
    for fragment in ["history", "download"] {
        assert!(
            session_help.contains(fragment),
            "{fragment}: {session_help}"
        );
    }
    let history = binary()
        .args(["--json", "session", "history", "--help"])
        .output()
        .unwrap();
    let value = outcome(&history, 0);
    let text = value["data"]["help"].as_str().unwrap();
    for fragment in ["--cache", "--continuation", "--message", "--max-messages"] {
        assert!(text.contains(fragment), "{fragment}: {text}");
    }
    let download = binary()
        .args(["--json", "session", "download", "--help"])
        .output()
        .unwrap();
    let value = outcome(&download, 0);
    let text = value["data"]["help"].as_str().unwrap();
    assert!(text.contains("--to"), "{text}");
    assert!(text.contains("--session"), "{text}");
    assert!(text.contains("--force"), "{text}");
    assert!(text.contains("--timeout"), "{text}");
}

fn attachment_fixture(
    destination: &Path,
) -> (
    tokio::runtime::Runtime,
    tempfile::TempDir,
    PathBuf,
    whalerein_core::api::test_support::FakeAccess,
) {
    use whalerein_core::api::test_support::FakeAttachmentObject;

    let fixture = paired_binary();
    std::fs::create_dir(destination).unwrap();
    fixture
        .3
        .host
        .lock()
        .unwrap()
        .put_attachment(FakeAttachmentObject::new(
            "session-1",
            "attachment-1",
            "note.txt",
            b"verified bytes".to_vec(),
        ));
    fixture
}

fn download_args(destination: &Path) -> Vec<String> {
    [
        "session",
        "download",
        "attachment-1",
        "--host",
        "host-1",
        "--session",
        "session-1",
        "--to",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(destination.to_str().unwrap().to_owned()))
    .collect()
}

#[test]
fn download_saves_verified_bytes_with_one_envelope_and_progress_on_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("downloads");
    let (_runtime, _fixture, config, access) = attachment_fixture(&destination);
    let args = download_args(&destination);
    let result = invocation(&config).args(&args).output().unwrap();
    let value = outcome(&result, 0);
    assert_eq!(value["command"], "session.download");
    assert_eq!(value["data"]["reference"], "attachment-1");
    assert_eq!(value["data"]["name"], "note.txt");
    assert_eq!(value["data"]["bytes"], 14);
    assert_eq!(value["data"]["verified"], true);
    assert_eq!(value["data"]["media_type"], "application/octet-stream");
    assert_eq!(
        Path::new(value["data"]["path"].as_str().unwrap()),
        destination.join("note.txt")
    );
    assert_eq!(
        std::fs::read(destination.join("note.txt")).unwrap(),
        b"verified bytes"
    );
    assert_eq!(
        value["data"]["sha256"],
        "186287b2d987891f027b4bc8baaf621a3e5a4a73ec78e04b0f65dc309b1ccc03"
    );
    assert_eq!(access.host.lock().unwrap().attachment_open_calls, 1);

    // Progress is structured stderr, never mixed into the single stdout envelope.
    let stderr = String::from_utf8(result.stderr).unwrap();
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.len() >= 2, "{events:?}");
    assert!(events
        .iter()
        .all(|event| event["event"] == "download_progress"));
    assert_eq!(events[0]["received"], 0);
    assert_eq!(events[0]["total"], 14);
    assert_eq!(events.last().unwrap()["received"], 14);
    assert!(!stderr.contains("note.txt"));
    assert!(!stderr.contains(destination.to_str().unwrap()));
    assert!(!String::from_utf8(result.stdout)
        .unwrap()
        .contains("download_progress"));
}

#[test]
fn human_download_keeps_progress_on_stderr_and_success_on_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("downloads");
    let (_runtime, _fixture, config, _access) = attachment_fixture(&destination);
    let args = download_args(&destination);
    let result = binary()
        .arg("--config")
        .arg(config)
        .args(&args)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("Saved: "), "{stdout}");
    assert!(stdout.contains("Verified: yes"), "{stdout}");
    assert!(stdout.contains("SHA-256: "), "{stdout}");
    assert!(!stdout.contains("whalerein/client/v1"), "{stdout}");
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 2, "{stderr}");
    assert!(stderr.lines().all(|line| line.starts_with("Progress: ")));
    assert!(stderr.ends_with("(100%)\n"), "{stderr}");
}

#[test]
fn download_failure_is_one_envelope_that_touches_no_file() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("downloads");
    let (_runtime, _fixture, config, access) = attachment_fixture(&destination);
    access.host.lock().unwrap().attachment_unsupported = true;
    let args = download_args(&destination);
    let result = invocation(&config).args(&args).output().unwrap();
    let value = outcome(&result, 9);
    assert_eq!(value["command"], "session.download");
    assert_eq!(value["error"]["code"], "unsupported_capability");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("No bytes were read"), "{message}");
    assert!(message.contains("session history session-1"), "{message}");
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    // The failed stage is a bounded warning event; no progress or path leaks.
    let stderr = String::from_utf8(result.stderr).unwrap();
    let event: Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(event["event"], "attachment_transfer");
    assert_eq!(event["stage"], "download_open");
    assert_eq!(event["outcome"], "failed");
    assert!(!stderr.contains("note.txt"));
    assert!(!stderr.contains(destination.to_str().unwrap()));
}

#[test]
fn verbose_download_reports_core_stages_without_changing_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("downloads");
    let (_runtime, _fixture, config, _access) = attachment_fixture(&destination);
    let args = download_args(&destination);
    let result = invocation(&config)
        .args(["--verbose"])
        .args(&args)
        .output()
        .unwrap();
    let value = outcome(&result, 0);
    assert_eq!(value["data"]["verified"], true);
    let stderr = String::from_utf8(result.stderr).unwrap();
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let stages: Vec<&str> = events
        .iter()
        .filter(|event| event["event"] == "operation_diagnostic")
        .filter_map(|event| event["stage"].as_str())
        .collect();
    for stage in [
        "download_open",
        "download_admission",
        "download_part",
        "download_verify",
        "download_commit",
    ] {
        assert!(stages.contains(&stage), "{stage}: {stages:?}");
    }
    assert!(events
        .iter()
        .any(|event| event["event"] == "download_progress"));
    assert!(!String::from_utf8(result.stdout)
        .unwrap()
        .contains("operation_diagnostic"));
}

#[test]
fn history_without_a_pairing_is_a_single_error_envelope() {
    let temp = tempfile::tempdir().unwrap();
    let config = selected(temp.path(), "standalone");
    let result = invocation(&config)
        .args(["session", "history", "session-1"])
        .output()
        .unwrap();
    let value = outcome(&result, 4);
    assert_eq!(value["command"], "session.history");
    assert_eq!(value["error"]["code"], "not_paired");
    assert!(result.stderr.is_empty());
}

fn paired_binary() -> (
    tokio::runtime::Runtime,
    tempfile::TempDir,
    PathBuf,
    whalerein_core::api::test_support::FakeAccess,
) {
    paired_binary_with(|_, _| {})
}

fn paired_binary_with(
    configure: impl FnOnce(&mut Command, &Path),
) -> (
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
    let mut command = invocation(&config);
    configure(&mut command, temp.path());
    let mut pairing = command
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

#[cfg(unix)]
#[test]
fn secrets_dir_persists_credentials_across_cli_processes() {
    use std::os::unix::fs::PermissionsExt;

    let (_runtime, temp, config, _access) = paired_binary_with(|command, root| {
        command.arg("--secrets-dir").arg(root.join("credentials"));
    });
    let directory = temp.path().join("credentials");
    let result = invocation(&config)
        .args(["session", "history", "session-1", "--secrets-dir"])
        .arg(&directory)
        .output()
        .unwrap();
    assert_eq!(outcome(&result, 0)["data"]["source"], "online");

    assert_eq!(
        directory.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    let files: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect();
    assert!(!files.is_empty());
    for file in files {
        let metadata = file.metadata().unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn verbose_history_separates_structured_stderr_from_one_stdout_envelope() {
    let (_runtime, _temp, config, _access) = paired_binary();

    let history = invocation(&config)
        .args(["--verbose", "session", "history", "session-1"])
        .output()
        .unwrap();
    let value = outcome(&history, 0);
    assert_eq!(value["command"], "session.history");
    assert_eq!(value["data"]["source"], "online");
    assert_eq!(value["data"]["messages"][0]["message_id"], "message-1");

    let stderr = String::from_utf8(history.stderr).unwrap();
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.iter().any(|event| {
            event["event"] == "operation_diagnostic" && event["stage"] == "history_page"
        }),
        "{events:?}"
    );
    assert!(events.iter().all(|event| event["trace_id"].is_string()));
    assert!(!stderr.contains("message-1"));
}

#[test]
fn verbose_send_reports_confirmed_input_and_read_stages_without_changing_stdout() {
    use whalerein_proto::MessageReplyState;

    let (_runtime, _temp, config, access) = paired_binary();

    access.host.lock().unwrap().message_reply_state =
        Some(MessageReplyState::MessageReplyInputRequired);
    let input = invocation(&config)
        .args(["--verbose", "session", "send", "session-1", "Hello"])
        .output()
        .unwrap();
    let value = outcome(&input, 0);
    assert_eq!(value["data"]["reply"]["state"], "input_required");
    assert_eq!(value["data"]["reply"]["kinds"][0], "approval");
    assert_eq!(value["data"]["input_kinds"][0], "approval");
    let stderr = String::from_utf8(input.stderr).unwrap();
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.iter().any(|event| {
            event["event"] == "operation_diagnostic" && event["stage"] == "send_input"
        }),
        "{events:?}"
    );
    assert!(!stderr.contains("Hello"));

    access.host.lock().unwrap().message_reply_state = None;
    access.host.lock().unwrap().history_messages.clear();
    let failed = invocation(&config)
        .args(["--verbose", "session", "send", "session-1", "Hello again"])
        .output()
        .unwrap();
    let value = outcome(&failed, 0);
    assert_eq!(value["data"]["reply"]["state"], "committed");
    assert!(value["data"]["reference"].is_string());
    assert_eq!(value["data"]["read_error"], "not_found");
    let stderr = String::from_utf8(failed.stderr).unwrap();
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (stage, outcome) in [
        ("send_reply_confirmed", "confirmed"),
        ("send_reply_read", "failed"),
    ] {
        assert!(
            events.iter().any(|event| {
                event["event"] == "operation_diagnostic"
                    && event["stage"] == stage
                    && event["outcome"] == outcome
            }),
            "{stage}: {events:?}"
        );
    }
    assert!(!stderr.contains("Hello again"));
}
