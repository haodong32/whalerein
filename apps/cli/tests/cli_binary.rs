//! Binary-level checks for stable output, exit codes and non-interactive modes.

use std::process::Command;

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
