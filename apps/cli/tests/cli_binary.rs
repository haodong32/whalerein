//! Binary-level checks for stable output, exit codes and non-interactive modes.

use std::io::Write;
use std::process::{Command, Stdio};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_whalerein"))
}

#[test]
fn doctor_json_is_stable_and_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let output = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert_eq!(value["command"], "doctor");
    assert!(value["data"]["address"].is_null());
    assert_eq!(value["data"]["commands"]["non_terminal"], 0);
    assert_eq!(value["data"]["commands"]["unknown"], 0);
    assert!(value["data"].get("connection").is_none());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("private"));
}

#[test]
fn exits_two_when_usage_is_invalid() {
    let output = binary().arg("sessions").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn pair_reads_the_invitation_from_stdin_without_echoing_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            "-",
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            b"  whalerein://pair?v=1&access=http%3A%2F%2F127.0.0.1%3A1&host=h&token=SECRETTOKEN\n",
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The URI was parsed (not a usage error) and the token stayed out of both
    // the envelope and diagnostics.
    assert_ne!(
        output.status.code(),
        Some(2),
        "stdin URI was treated as invalid input: {stdout} {stderr}"
    );
    assert!(!stdout.contains("SECRETTOKEN"), "{stdout}");
    assert!(!stderr.contains("SECRETTOKEN"), "{stderr}");
    assert!(
        !stdout.trim().is_empty(),
        "stdout was empty; stderr={stderr}; code={:?}",
        output.status.code()
    );
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert!(value["error"].is_object(), "{value}");
}

#[test]
fn requires_yes_for_non_interactive_pairing_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let output = binary()
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            "whalerein://pair?v=1&access=http%3A%2F%2F127.0.0.1%3A1&host=h&token=t",
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    let path = dir.path().join("diag.json");
    let output = binary()
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
            "--export",
            path.to_str().unwrap(),
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!path.exists());
}

#[test]
fn exits_four_when_command_requires_pairing() {
    let dir = tempfile::tempdir().unwrap();
    let output = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "hosts",
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "not_paired");
    assert_eq!(value["error"]["action"], "reauth");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("whalerein pair <pairing-uri>"),
        "{message}"
    );
    assert!(
        message.contains("whalerein forget --local-only"),
        "{message}"
    );

    // The reachable first-run human output names the next command too.
    let human = binary()
        .args(["--data-dir", dir.path().to_str().unwrap(), "hosts"])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(4));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&human.stderr),
        String::from_utf8_lossy(&human.stdout)
    );
    assert!(text.contains("not_paired"), "{text}");
    assert!(text.contains("whalerein pair"), "{text}");
}

#[test]
fn default_failure_is_rendered_once_while_diagnostics_remain_available() {
    let dir = tempfile::tempdir().unwrap();
    let invoke = |json: bool, detailed: bool| {
        let mut command = binary();
        command
            .args(["--data-dir", dir.path().to_str().unwrap(), "hosts"])
            .env("WHALEREIN_SECRET_BACKEND", "memory")
            .env_remove("WHALEREIN_LOG")
            .env_remove("RUST_LOG")
            .env("NO_COLOR", "1");
        if json {
            command.arg("--json");
        }
        if detailed {
            command.env("WHALEREIN_LOG", "info");
        }
        command.output().unwrap()
    };
    let human = invoke(false, false);
    assert_eq!(human.status.code(), Some(4));
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert_eq!(
        stderr.lines().count(),
        2,
        "default failure should contain one error and its diagnostic ID"
    );
    assert_eq!(stderr.matches("Diagnostic ID:").count(), 1);
    assert!(!stderr.contains("operation_diagnostic"));
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("last-failure.json")).unwrap())
            .unwrap();
    assert!(stderr.contains(record["trace_id"].as_str().unwrap()));
    assert_eq!(record["events"][0]["code"], "not_paired");

    let json = invoke(true, false);
    assert!(
        json.stderr.is_empty(),
        "default JSON failure should not duplicate the error on stderr"
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["error"]["code"], "not_paired");
    assert_eq!(value["error"]["diagnostic_id"].as_str().unwrap().len(), 32);

    let detailed = invoke(false, true);
    let stderr = String::from_utf8(detailed.stderr).unwrap();
    assert!(stderr.contains("operation_diagnostic"));
    assert_eq!(stderr.matches("error: client is not paired").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthorized_pairing_has_one_terminal_error_and_keeps_failure_record() {
    use std::sync::Arc;
    use whalerein_core::api::test_support::{MockResponse, MockServer};

    let server = MockServer::start(Arc::new(|_| {
        MockResponse::json(
            401,
            &serde_json::json!({"code": "unauthorized", "message": "unauthorized"}),
        )
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let access = server
        .url()
        .as_str()
        .replace(':', "%3A")
        .replace('/', "%2F");
    let uri = format!("whalerein://pair?v=1&access={access}&host=fixture-host&token=fixture-only");
    let output = binary()
        .args([
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            &uri,
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .env_remove("WHALEREIN_LOG")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 2);
    assert_eq!(
        stderr
            .matches("error: Access error: unauthorized (unauthorized)")
            .count(),
        1
    );
    assert!(!stderr.contains("operation_diagnostic"));
    assert!(!stderr.contains("fixture-only"));
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("last-failure.json")).unwrap())
            .unwrap();
    assert!(stderr.contains(record["trace_id"].as_str().unwrap()));
    assert_eq!(record["events"][0]["code"], "unauthorized");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_accepts_a_matching_fingerprint_without_yes() {
    use whalerein_core::api::test_support::FakeAccess;

    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = format!("file:{}", dir.path().join("secrets").display());
    let output = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            &access.pairing_uri(),
            "--fingerprint",
            &access.fingerprint(),
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "pair");
    assert!(value["data"]["device_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert!(dir.path().join("config.json").exists());

    let hosts = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "hosts",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .output()
        .unwrap();
    assert!(hosts.status.success(), "{hosts:?}");
    let hosts: serde_json::Value = serde_json::from_slice(&hosts.stdout).unwrap();
    assert_eq!(hosts["data"]["hosts"][0]["host_id"], "host-1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_rejects_a_mismatched_fingerprint_even_with_yes() {
    use whalerein_core::api::test_support::FakeAccess;

    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = format!("file:{}", dir.path().join("secrets").display());
    let output = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            &access.pairing_uri(),
            "--fingerprint",
            &"00".repeat(32),
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .env_remove("WHALEREIN_LOG")
        .env_remove("RUST_LOG")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "host_fingerprint_mismatch");
    let message = value["error"]["message"].as_str().unwrap();
    for hint in [
        "verify",
        "whalerein forget --local-only",
        "whalerein pair",
        "revoke",
    ] {
        assert!(message.contains(hint), "missing {hint:?}: {message}");
    }
    assert!(!message.contains(&"00".repeat(32)));
    assert!(!dir.path().join("config.json").exists());

    // A refused fingerprint must leave no authorization behind.
    let hosts = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "hosts",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .output()
        .unwrap();
    assert_eq!(hosts.status.code(), Some(4), "{hosts:?}");
    let value: serde_json::Value = serde_json::from_slice(&hosts.stdout).unwrap();
    assert_eq!(value["error"]["code"], "not_paired");
}

/// Read the single JSON envelope on stdout and assert stderr is quiet.
fn envelope(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(
        output.stderr.is_empty(),
        "stderr must stay quiet under --json: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "expected one envelope, got {stdout}");
    serde_json::from_str(lines[0]).unwrap()
}

#[test]
fn json_usage_errors_use_the_envelope_for_every_shape() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_str().unwrap();
    for args in [
        vec!["--json", "sessions"],
        vec!["--json", "send", "--session", "s"],
        vec!["--json", "sessions", "--json"],
        vec!["--json", "host"],
        vec!["--json"],
        vec!["--json", "doctor", "--export"],
    ] {
        let output = binary().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let value = envelope(&output);
        assert_eq!(value["schema"], "whalerein/v1", "{args:?}");
        assert_eq!(value["command"], "cli", "{args:?}");
        assert_eq!(value["error"]["code"], "invalid_argument", "{args:?}");
        assert_eq!(value["error"]["action"], "fix_input", "{args:?}");
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "{args:?}"
        );
    }
    // A usage error after a valid command token is still machine readable.
    let output = binary()
        .args([
            "--json",
            "--data-dir",
            data_dir,
            "tail",
            "--session",
            "s",
            "--bogus",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "invalid_argument");
}

#[test]
fn plain_usage_errors_keep_clap_text() {
    let output = binary().arg("sessions").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("required arguments"), "{stderr}");
    assert!(!stderr.contains("whalerein/v1"));
}

#[test]
fn json_help_and_version_are_explicit_envelopes() {
    for flag in ["--help", "--version"] {
        let output = binary().args(["--json", flag]).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let value = envelope(&output);
        assert_eq!(value["schema"], "whalerein/v1");
        assert_eq!(value["command"], "cli");
        assert!(value.get("error").is_none());
        if flag == "--help" {
            assert!(value["data"]["help"].as_str().unwrap().contains("Usage:"));
        } else {
            assert_eq!(
                value["data"]["version"],
                format!("whalerein {}", env!("CARGO_PKG_VERSION"))
            );
        }
    }
    // Plain help/version stay human text with a zero exit.
    let help = binary().arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
    assert!(help.stdout.first() != Some(&b'{'));
    let version = binary().arg("--version").output().unwrap();
    assert_eq!(version.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&version.stdout).contains("0.1.0"));
}

#[test]
fn hyphen_leading_message_text_reaches_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_str().unwrap();
    for args in [
        vec!["send", "--session", "s", "-hello"],
        vec!["send", "--session", "s", "--", "-hello"],
        vec![
            "queue",
            "edit",
            "--session",
            "s",
            "--item",
            "q",
            "-replacement",
        ],
    ] {
        let mut command = binary();
        command.args(["--json", "--data-dir", data_dir]).args(&args);
        let output = command
            .env("WHALEREIN_SECRET_BACKEND", "memory")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        // The text is a value: the command runs and fails only because this
        // temporary profile is not paired.
        assert_eq!(output.status.code(), Some(4), "{args:?}: {output:?}");
        let value = envelope(&output);
        assert_eq!(value["error"]["code"], "not_paired", "{args:?}");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("Usage:"),
            "help must not be printed for message text"
        );
    }
    // A bare --help still prints help.
    let help = binary().arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
}

#[test]
fn subcommand_host_is_rejected_with_actionable_guidance() {
    let output = binary()
        .args(["send", "--session", "s", "--host", "h", "text"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("place --host before the command"),
        "{stderr}"
    );
    assert!(!stderr.contains("-- --host"), "{stderr}");

    let output = binary()
        .args(["--json", "send", "--session", "s", "--host", "h", "text"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "invalid_argument");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("place --host before the command"));
}

#[test]
fn help_and_version_survive_a_broken_environment() {
    // Diagnosing a bad filter needs `--help`; only real commands reject it.
    for flag in ["--help", "--version"] {
        let output = binary()
            .arg(flag)
            .env("WHALEREIN_LOG", "bad[[")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{flag}: {output:?}");
        assert!(!output.stdout.is_empty());
    }
}

#[test]
fn invalid_environment_is_invalid_configuration_with_the_variable_named() {
    for (variable, value, expected) in [
        ("WHALEREIN_MQTT_URL", "http://x", "unsupported scheme"),
        ("WHALEREIN_CA_FILE", "/no/such/ca.pem", "could not be read"),
        ("WHALEREIN_LOG", "bad[[", "log filter"),
    ] {
        let output = binary()
            .args(["--json", "hosts"])
            .env(variable, value)
            .env("WHALEREIN_SECRET_BACKEND", "memory")
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{variable}");
        let value = envelope(&output);
        assert_eq!(
            value["error"]["code"], "invalid_configuration",
            "{variable}"
        );
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains(variable), "{variable}: {message}");
        assert!(message.contains(expected), "{variable}: {message}");
    }
}

#[test]
fn corrupt_configuration_is_quarantined_for_doctor_and_local_forget() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(secrets.join("identity-material"), b"retained").unwrap();
    std::fs::write(dir.path().join("config.json"), b"not json").unwrap();
    let backend = format!("file:{}", secrets.display());

    // A command that needs routing still fails explicitly.
    let blocked = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "hosts",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &backend)
        .output()
        .unwrap();
    assert_eq!(blocked.status.code(), Some(2));
    assert_eq!(envelope(&blocked)["error"]["code"], "invalid_configuration");

    let doctor = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &backend)
        .output()
        .unwrap();
    assert_eq!(doctor.status.code(), Some(0), "{doctor:?}");
    let value = envelope(&doctor);
    let quarantined = value["data"]["config_quarantined"]
        .as_str()
        .expect("quarantine path");
    assert!(std::path::Path::new(quarantined).exists());
    assert!(!dir.path().join("config.json").exists());
    // Identity secrets are untouched.
    assert_eq!(
        std::fs::read(secrets.join("identity-material")).unwrap(),
        b"retained"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(quarantined).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // A second corrupt file is recovered by forget --local-only.
    std::fs::write(dir.path().join("config.json"), b"{\"schema\":\"wrong\"}").unwrap();
    let forget = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "forget",
            "--local-only",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &backend)
        .output()
        .unwrap();
    assert_eq!(forget.status.code(), Some(0), "{forget:?}");
    assert!(envelope(&forget)["data"]["config_quarantined"].is_string());
}

#[test]
fn destructive_commands_refuse_without_yes_and_leave_state_alone() {
    let dir = tempfile::tempdir().unwrap();
    for args in [vec!["revoke"], vec!["forget", "--local-only"]] {
        let mut command = binary();
        command
            .args(["--json", "--data-dir", dir.path().to_str().unwrap()])
            .args(&args)
            .env("WHALEREIN_SECRET_BACKEND", "memory")
            .stdin(std::process::Stdio::null());
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let value = envelope(&output);
        assert_eq!(value["error"]["code"], "invalid_argument", "{args:?}");
        assert_eq!(value["error"]["action"], "fix_input", "{args:?}");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--yes"));
    }
    // Nothing was removed.
    assert!(!dir.path().join("config.json").exists());
}

#[test]
fn doctor_export_honors_streams_mode_and_file_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostic.json");
    let output = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
            "--export",
            path.to_str().unwrap(),
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .env_remove("WHALEREIN_LOG")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let value = envelope(&output);
    assert_eq!(value["command"], "doctor");
    assert_eq!(value["data"]["path"], path.display().to_string());
    assert!(path.exists());
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("whalerein/v1"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // A symlink target is refused by Core and surfaced as a usage error, so a
    // compromised link cannot redirect the bundle.
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let target = dir.path().join("do-not-touch.json");
        std::fs::write(&target, b"original").unwrap();
        let link = dir.path().join("export-link.json");
        symlink(&target, &link).unwrap();
        let output = binary()
            .args([
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
                "--export",
                link.to_str().unwrap(),
            ])
            .env("WHALEREIN_SECRET_BACKEND", "memory")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let value = envelope(&output);
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("symbolic link"));
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
    }

    // A directory target is a usage error, not an internal failure.
    let directory = tempfile::tempdir().unwrap();
    let output = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
            "--export",
            directory.path().to_str().unwrap(),
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "invalid_argument");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("directory"));
}

#[test]
fn malformed_fingerprint_is_rejected_before_the_network() {
    // 127.0.0.1:1 would be a transport failure (exit 5), never exit 2.
    let uri = "whalerein://pair?v=1&access=http%3A%2F%2F127.0.0.1%3A1&host=h&token=t";
    for pin in ["zz", "ab", ""] {
        let dir = tempfile::tempdir().unwrap();
        let output = binary()
            .args([
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "pair",
                uri,
                "--fingerprint",
                pin,
            ])
            .env("WHALEREIN_SECRET_BACKEND", "memory")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{pin:?}: {output:?}");
        let value = envelope(&output);
        assert_eq!(value["error"]["code"], "invalid_argument");
        let text = value.to_string();
        if !pin.is_empty() {
            assert!(
                !text.contains(&format!("\"{pin}\"")),
                "must not echo the pin"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_closed_stdout_pipe_exits_without_a_rust_panic() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;

    let (writer, reader) = UnixStream::pair().unwrap();
    let writer = OwnedFd::from(writer);
    // Close the consumer before the child writes anything.
    drop(reader);
    let dir = tempfile::tempdir().unwrap();
    let child = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(child.status.code(), Some(0), "{child:?}");
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("failed printing to stdout"), "{stderr}");
    assert_ne!(child.status.code(), Some(101));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigint_before_submission_exits_two_with_a_cancelled_envelope() {
    use std::process::Stdio;
    use std::time::Duration;
    use whalerein_core::api::test_support::FakeAccess;

    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_str().unwrap();
    let secrets = format!("file:{}", dir.path().join("secrets").display());
    let pair = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            data_dir,
            "pair",
            &access.pairing_uri(),
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(pair.status.code(), Some(0), "{pair:?}");

    access.host.lock().unwrap().wait_receipt_delay_ms = 5_000;
    let mut child = binary()
        .args([
            "--json",
            "--data-dir",
            data_dir,
            "wait",
            "--session",
            "session-1",
            "--timeout",
            "20",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "wait should still be running"
    );
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let output = tokio::time::timeout(Duration::from_secs(10), async {
        child.wait_with_output().unwrap()
    })
    .await
    .expect("process must exit after SIGINT");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("panicked"), "{stdout}");
    assert!(
        !stdout.trim().is_empty(),
        "stdout was empty; stderr={stderr}; code={:?}",
        output.status.code()
    );
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["schema"], "whalerein/v1");
    assert_eq!(value["command"], "wait");
    assert_eq!(value["error"]["code"], "cancelled");
    assert_eq!(access.host.lock().unwrap().commands_seen, 0);
}

#[test]
fn data_dir_flag_rejects_a_file_target_and_wins_over_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    let output = binary()
        .args(["--json", "--data-dir", file.to_str().unwrap(), "doctor"])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(envelope(&output)["error"]["code"], "invalid_configuration");

    // The flag beats WHALEREIN_DATA_DIR.
    let flag = dir.path().join("from-flag");
    let from_env = dir.path().join("from-env");
    let output = binary()
        .args(["--json", "--data-dir", flag.to_str().unwrap(), "doctor"])
        .env("WHALEREIN_DATA_DIR", &from_env)
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(flag.exists(), "the flag directory must be used");
    assert!(
        !from_env.exists(),
        "the environment directory must be ignored"
    );
}

#[test]
fn a_bad_secret_backend_is_an_invalid_configuration_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    for backend in ["bogus", "file:"] {
        let output = binary()
            .args([
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
            ])
            .env("WHALEREIN_SECRET_BACKEND", backend)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{backend}: {output:?}");
        let value = envelope(&output);
        assert_eq!(value["error"]["code"], "invalid_configuration", "{backend}");
    }
}

#[test]
fn piped_human_output_has_no_ansi_and_no_empty_list_dump() {
    let dir = tempfile::tempdir().unwrap();
    let output = binary()
        .args(["--data-dir", dir.path().to_str().unwrap(), "hosts"])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !text.contains('\u{1b}'),
        "piped output must not contain ANSI"
    );
    assert!(!text.contains("[]"), "empty results need actionable text");

    let output = binary()
        .args([
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
        ])
        .env("WHALEREIN_SECRET_BACKEND", "memory")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{1b}'));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approvals_request_prints_the_wire_state_and_unknown_is_not_found() {
    use whalerein_core::api::test_support::FakeAccess;

    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_str().unwrap();
    let secrets = format!("file:{}", dir.path().join("secrets").display());
    let pair = binary()
        .args([
            "--json",
            "--yes",
            "--data-dir",
            data_dir,
            "pair",
            &access.pairing_uri(),
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(pair.status.code(), Some(0), "{pair:?}");
    access
        .host
        .lock()
        .unwrap()
        .approvals
        .push(whalerein_proto::Approval {
            approval_id: "approval-1".into(),
            session_id: "session-1".into(),
            tool_name: "bash".into(),
            call_id: "call-1".into(),
            reason: "review".into(),
            digest: vec![1; 32],
            version: 3,
            expires_at_unix_ms: 9,
            state: whalerein_proto::ApprovalState::Approved as i32,
            decision: whalerein_proto::ApprovalDecision::Approve as i32,
        });

    let read = binary()
        .args([
            "--json",
            "--data-dir",
            data_dir,
            "approvals",
            "--request",
            "approval-1",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .output()
        .unwrap();
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let value = envelope(&read);
    assert_eq!(value["command"], "approvals");
    assert_eq!(value["data"]["approval"]["approval_id"], "approval-1");
    assert_eq!(
        value["data"]["approval"]["state"],
        "APPROVAL_STATE_APPROVED"
    );
    assert_eq!(value["data"]["approval"]["version"], 3);
    assert_eq!(
        value["data"]["approval"]["decision"],
        "APPROVAL_DECISION_APPROVE"
    );

    let human = binary()
        .args([
            "--data-dir",
            data_dir,
            "approvals",
            "--request",
            "approval-1",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(0), "{human:?}");
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("state: APPROVAL_STATE_APPROVED"), "{text}");
    assert!(text.contains("approval: approval-1"), "{text}");

    // An unknown id is not found, not an empty success.
    let unknown = binary()
        .args([
            "--json",
            "--data-dir",
            data_dir,
            "approvals",
            "--request",
            "missing",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(3), "{unknown:?}");
    let value = envelope(&unknown);
    assert_eq!(value["error"]["code"], "not_found");

    // The existing pending-list shape is unchanged.
    let list = binary()
        .args([
            "--json",
            "--data-dir",
            data_dir,
            "approvals",
            "--session",
            "session-1",
        ])
        .env("WHALEREIN_SECRET_BACKEND", &secrets)
        .output()
        .unwrap();
    assert_eq!(list.status.code(), Some(0), "{list:?}");
    let value = envelope(&list);
    assert_eq!(value["data"]["session_id"], "session-1");
    assert_eq!(value["data"]["approvals"][0]["approval_id"], "approval-1");
    assert!(value["data"].get("state").is_none());
}
