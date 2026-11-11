//! Command execution. All protocol behavior lives in `whalerein-core`.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use whalerein_core::api::{
    default_data_dir, ExportManifest, MemorySecretStore, PairingDecision, SecretStore,
};
use whalerein_core::{Core, CoreError, CoreOptions};

use crate::cli::{Cli, Command};
use crate::output::{
    command_data, core_error_outcome, doctor_data, hosts_data, is_tail_output, manifest_data,
    render_approvals, render_command, render_doctor, render_hosts, render_manifest, render_pair,
    render_sessions, sessions_data, tail_event_json, CliError, CliOutcome, TailRenderer,
};

const DEFAULT_TAIL_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn run(cli: Cli) -> CliOutcome {
    let command = cli.command.clone();
    let secrets = match secret_store() {
        Ok(secrets) => secrets,
        Err(error) => {
            return CliOutcome::local_failure(&command, error.code.as_str(), error.message, 2)
        }
    };
    run_with(cli, secrets).await
}

/// Injectable variant used by tests. The only injected boundary is SecretStore.
pub(crate) async fn run_with(cli: Cli, secrets: Arc<dyn SecretStore>) -> CliOutcome {
    let command = cli.command.clone();
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    let core = match Core::new(CoreOptions { data_dir, secrets }) {
        Ok(core) => core,
        Err(error) => return core_error_outcome(&command, error),
    };

    let result = match &command {
        Command::Pair {
            uri,
            fingerprint,
            name,
        } => {
            pair(
                &core,
                &command,
                &cli,
                uri,
                fingerprint.as_deref(),
                name.as_deref(),
            )
            .await
        }
        Command::PairResume => match core.resume_pairing().await {
            Ok(outcome) => Ok(CliOutcome::success(
                &command,
                serde_json::to_value(&outcome).unwrap_or_else(|_| json!({})),
                render_pair(&outcome),
            )),
            Err(error) => Err(error),
        },
        Command::Hosts => match core.hosts().await {
            Ok(hosts) => Ok(CliOutcome::success(
                &command,
                hosts_data(&hosts),
                render_hosts(&hosts),
            )),
            Err(error) => Err(error),
        },
        Command::Sessions { host } => match core.sessions(host).await {
            Ok(list) => Ok(CliOutcome::success(
                &command,
                sessions_data(host, &list),
                render_sessions(&list),
            )),
            Err(error) => Err(error),
        },
        Command::Tail {
            session,
            once,
            timeout,
        } => {
            let timeout = timeout.map(Duration::from_secs).or(if *once {
                Some(DEFAULT_TAIL_TIMEOUT)
            } else {
                None
            });
            tail(&core, &command, cli.json, session, *once, timeout).await
        }
        Command::Sync { session } => match core.sync_session(session).await {
            Ok(events) => Ok(CliOutcome::success(
                &command,
                json!({ "session_id": session, "events": events }),
                format!("Synced session {session} ({events} event(s)).\n"),
            )),
            Err(error) => Err(error),
        },
        Command::Send { session, text } => match core.send_message(session, text).await {
            Ok(handle) => Ok(CliOutcome::success(
                &command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(error) => Err(error),
        },
        Command::Approvals { session } => match core.list_approvals(session).await {
            Ok(approvals) => Ok(CliOutcome::success(
                &command,
                json!({ "session_id": session, "approvals": approvals }),
                render_approvals(&approvals),
            )),
            Err(error) => Err(error),
        },
        Command::Approve {
            request,
            digest,
            version,
        } => {
            let digest = match parse_digest(digest) {
                Ok(digest) => digest,
                Err(error) => {
                    return CliOutcome::local_failure(
                        &command,
                        error.code.as_str(),
                        error.message,
                        2,
                    )
                }
            };
            match core.approve(request, &digest, *version).await {
                Ok(handle) => Ok(CliOutcome::success(
                    &command,
                    command_data(&handle),
                    render_command(&handle),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Reject {
            request,
            digest,
            version,
        } => {
            let digest = match parse_digest(digest) {
                Ok(digest) => digest,
                Err(error) => {
                    return CliOutcome::local_failure(
                        &command,
                        error.code.as_str(),
                        error.message,
                        2,
                    )
                }
            };
            match core.reject(request, &digest, *version).await {
                Ok(handle) => Ok(CliOutcome::success(
                    &command,
                    command_data(&handle),
                    render_command(&handle),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Interrupt { session } => match core.interrupt(session).await {
            Ok(handle) => Ok(CliOutcome::success(
                &command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(error) => Err(error),
        },
        Command::Revoke => match core.revoke().await {
            Ok(device_id) => Ok(CliOutcome::success(
                &command,
                json!({ "device_id": device_id, "revoked": true }),
                "Revoked this device's grants. Local identity was kept.\n".to_owned(),
            )),
            Err(error) => Err(error),
        },
        Command::Forget { local_only } => {
            if !local_only && core.is_paired() {
                let _ = core.hosts().await;
            }
            let forgotten = if *local_only {
                core.forget_local_device()
            } else {
                core.forget_device()
            };
            forgotten.map(|()| CliOutcome::success(&command, json!({"identity_forgotten": true,"local_only":local_only}), if *local_only {"Forgot local identity. Its server entry may remain; revoke the old device in host settings.\n"} else {"Forgot this local device identity.\n"}))
        }
        Command::Doctor {
            export,
            clear_diagnostics,
        } => {
            if *clear_diagnostics {
                core.clear_diagnostics().map(|()| {
                    CliOutcome::success(
                        &command,
                        json!({"diagnostics_cleared": true}),
                        "Cleared local diagnostic history.\n",
                    )
                })
            } else {
                doctor(&core, &command, &cli, export.as_deref())
            }
        }
    };
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let mut outcome = core_error_outcome(&command, error);
            if let Some(error) = outcome.error.as_mut() {
                error.diagnostic_id = Some(core.diagnostic_id().to_owned());
                if core.save_failure(&error.code).is_err() {
                    tracing::warn!(event = "diagnostic_save_failed", trace_id = %core.diagnostic_id(), code = "unavailable");
                }
            }
            outcome
        }
    }
}

fn secret_store() -> Result<Arc<dyn SecretStore>, CliError> {
    let value = std::env::var("WHALEREIN_SECRET_BACKEND").unwrap_or_default();
    if value.is_empty() || value == "keyring" {
        return Ok(Arc::new(whalerein_core::api::KeyringSecretStore::new(
            "whalerein",
        )));
    }
    if value == "memory" {
        return Ok(Arc::new(MemorySecretStore::new()));
    }
    if let Some(path) = value.strip_prefix("file:") {
        if path.is_empty() {
            return Err(CliError::new(
                "invalid_configuration",
                "WHALEREIN_SECRET_BACKEND=file:<dir> requires a directory",
            ));
        }
        return Ok(Arc::new(whalerein_core::api::FileSecretStore::new(
            PathBuf::from(path),
        )));
    }
    Err(CliError::new(
        "invalid_configuration",
        "unknown WHALEREIN_SECRET_BACKEND",
    ))
}

fn display_name(explicit: Option<&str>) -> String {
    match explicit.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => name.to_owned(),
        None => computer_name(),
    }
}

fn computer_name() -> String {
    macos_computer_name()
        .or_else(hostname_name)
        .unwrap_or_default()
}

fn macos_computer_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        command_stdout("scutil", &["--get", "ComputerName"])
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn hostname_name() -> Option<String> {
    command_stdout("hostname", &[])
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

async fn pair(
    core: &Core,
    command: &Command,
    cli: &Cli,
    uri: &str,
    fingerprint: Option<&str>,
    name: Option<&str>,
) -> Result<CliOutcome, CoreError> {
    let interactive = std::io::stdin().is_terminal();
    if !cli.yes && fingerprint.is_none() && !interactive {
        return Ok(CliOutcome::local_failure(
            command,
            "invalid_argument",
            "non-interactive pairing requires --yes or --fingerprint",
            2,
        ));
    }
    let expected = fingerprint.map(str::to_owned);
    let yes = cli.yes;
    let decision = move |prompt: &whalerein_core::api::PairingPrompt| {
        if let Some(expected) = &expected {
            return PairingDecision::expect(expected.clone());
        }
        if yes {
            return PairingDecision::approve();
        }
        eprintln!("Host: {}", prompt.host_id);
        eprintln!("Access: {}", prompt.access_url);
        eprintln!("Fingerprint: {}", prompt.fingerprint_display);
        if prompt_yes_no("Pair with this host?") {
            PairingDecision::approve()
        } else {
            PairingDecision::reject()
        }
    };
    match core.pair_as(uri, decision, &display_name(name)).await {
        Ok(outcome) => Ok(CliOutcome::success(
            command,
            serde_json::to_value(&outcome).unwrap_or_else(|_| json!({})),
            render_pair(&outcome),
        )),
        Err(error) => Err(error),
    }
}

async fn tail(
    core: &Core,
    command: &Command,
    json_output: bool,
    session: &str,
    once: bool,
    timeout: Option<Duration>,
) -> Result<CliOutcome, CoreError> {
    let key = core.session_key(session)?;
    let mut renderer = TailRenderer::default();
    let mut visible = false;
    let emitted = core
        .watch_session(&key, timeout, |event| {
            if json_output {
                let mut value = tail_event_json(event);
                value["data"]["event"]["session"] =
                    json!({"host_id":key.host_id,"session_id":key.session_id});
                println!("{}", serde_json::to_string(&value).unwrap_or_default());
            } else {
                print!("{}", renderer.render(event));
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            visible |= is_tail_output(event);
            !(once && visible)
        })
        .await;
    if !json_output {
        print!("{}", renderer.finish_line());
    }
    let emitted = emitted?;
    if once && !visible {
        return Err(CoreError::Timeout);
    }
    Ok(CliOutcome::success(
        command,
        json!({ "session_id": session, "events": emitted }),
        String::new(),
    ))
}

fn doctor(
    core: &Core,
    command: &Command,
    cli: &Cli,
    export: Option<&std::path::Path>,
) -> Result<CliOutcome, CoreError> {
    match export {
        Some(path) => {
            let fields = ExportManifest::fields();
            eprintln!("Diagnostic export will include:");
            for field in &fields {
                eprintln!("  - {field}");
            }
            eprintln!(
                "Last failure: at most 32 metadata events, retained for 7 days, under 32 KiB."
            );
            eprintln!("It never includes message content, tool arguments, private keys,");
            eprintln!("device tokens, pairing tokens, full ciphertext, or host/device ids.");
            if !cli.yes && !std::io::stdin().is_terminal() {
                return Ok(CliOutcome::local_failure(
                    command,
                    "invalid_argument",
                    "diagnostic export requires --yes in non-interactive mode",
                    2,
                ));
            }
            if !cli.yes && !prompt_yes_no("Write the diagnostic bundle?") {
                return Ok(CliOutcome::local_failure(
                    command,
                    "cancelled",
                    "diagnostic export cancelled",
                    2,
                ));
            }
            let manifest = core.export_diagnostics(path)?;
            Ok(CliOutcome::success(
                command,
                manifest_data(&manifest),
                render_manifest(&manifest),
            ))
        }
        None => {
            let report = core.doctor()?;
            Ok(CliOutcome::success(
                command,
                doctor_data(&report),
                render_doctor(&report),
            ))
        }
    }
}

fn prompt_yes_no(prompt: &str) -> bool {
    use std::io::Write;
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn parse_digest(value: &str) -> Result<Vec<u8>, CliError> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    // Digest display grouping allows ASCII whitespace, colons, and hyphens only.
    if !value
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c.is_ascii_whitespace() || matches!(c, ':' | '-'))
    {
        return Err(CliError::new(
            "invalid_argument",
            "digest contains an invalid character",
        ));
    }
    let normalized: String = value.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if normalized.len() != 64 {
        return Err(CliError::new(
            "invalid_argument",
            "digest must be a 32-byte SHA-256 value in hex",
        ));
    }
    hex::decode(normalized)
        .map_err(|_| CliError::new("invalid_argument", "digest is not valid hex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::test_support::FakeAccess;
    use whalerein_core::FileSecretStore;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[test]
    fn display_name_prefers_an_explicit_name() {
        assert_eq!(display_name(Some("  办公室 ")), "办公室");
        assert_eq!(display_name(Some(" \n ")), computer_name());
        let name = computer_name();
        assert!(!name.is_empty());
        assert!(!name.contains('\n'));
        assert!(hostname_name().is_some());
        assert!(command_stdout("whalerein-missing-command", &[]).is_none());
        assert_eq!(command_stdout("true", &[]), None);
    }

    #[test]
    fn digest_parsing_is_strict() {
        let digest = parse_digest(&"ab".repeat(32)).unwrap();
        assert_eq!(digest, vec![0xab; 32]);
        assert_eq!(
            parse_digest(&format!("sha256:{}", "AB".repeat(32))).unwrap(),
            vec![0xab; 32]
        );
        assert!(parse_digest("abcd").is_err());
        assert!(parse_digest(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn digest_accepts_documented_prefix_and_display_grouping_without_changing_bytes() {
        let expected: Vec<u8> = (0..32).collect();
        let hex = hex::encode(&expected);
        for separator in [" ", ":", "-"] {
            let grouped = hex
                .as_bytes()
                .chunks(4)
                .map(|chunk| std::str::from_utf8(chunk).unwrap())
                .collect::<Vec<_>>()
                .join(separator);
            assert_eq!(
                parse_digest(&format!("sha256:{grouped}")).unwrap(),
                expected
            );
        }
        assert!(parse_digest(&"ab".repeat(31)).is_err());
        assert!(parse_digest(&"ab".repeat(33)).is_err());
        assert!(parse_digest(&format!("{}🦀", "ab".repeat(32))).is_err());
    }

    #[test]
    fn digest_rejects_non_hex_garbage_instead_of_silently_discarding_it() {
        for value in [
            format!("!{}", "ab".repeat(32)),
            format!("{}z", "ab".repeat(32)),
            format!("{}{}", "ab".repeat(16), "zab".repeat(16)),
        ] {
            assert!(
                parse_digest(&value).is_err(),
                "non-hex input must be rejected"
            );
        }
    }

    #[test]
    fn constructs_core_for_unpaired_doctor_command() {
        let dir = tempfile::tempdir().unwrap();
        let options = CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: Arc::new(MemorySecretStore::new()),
        };
        let core = Core::new(options).unwrap();
        assert!(!core.is_paired());
        assert!(core.access_address().is_none());
        let report = core.doctor().unwrap();
        assert!(report.address.is_none());
        assert_eq!(report.commands.non_terminal, 0);
    }

    #[tokio::test]
    async fn doctor_runs_without_pairing_using_memory_backend() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = cli(&[
            "whalerein",
            "--json",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "doctor",
        ]);
        let outcome = run_with(parsed, Arc::new(MemorySecretStore::new())).await;
        assert_eq!(outcome.exit, 0);
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/v1");
        assert!(value["data"]["address"].is_null());
        assert_eq!(value["data"]["commands"]["non_terminal"], 0);
        assert_eq!(value["data"]["commands"]["unknown"], 0);
        assert!(value["data"].get("connection").is_none());
    }

    #[tokio::test]
    async fn pairing_required_commands_return_not_paired_while_doctor_remains_available() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        for args in [
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "hosts",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "sessions",
                "--host",
                "h",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "send",
                "--session",
                "s",
                "text",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "interrupt",
                "--session",
                "s",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "revoke",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "sync",
                "--session",
                "s",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "approvals",
                "--session",
                "s",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "approve",
                "--request",
                "a",
                "--digest",
                &"ab".repeat(32),
                "--version",
                "1",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "reject",
                "--request",
                "a",
                "--digest",
                &"cd".repeat(32),
                "--version",
                "1",
            ],
        ] {
            let outcome = run_with(cli(&args), secrets.clone()).await;
            assert_eq!(outcome.exit, 4, "{args:?}");
            assert_eq!(outcome.error.unwrap().code, "not_paired");
        }
        let doctor = run_with(
            cli(&[
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
            ]),
            secrets,
        )
        .await;
        assert_eq!(doctor.exit, 0);
        assert!(doctor.error.is_none());
    }

    #[tokio::test]
    async fn returns_usage_error_for_invalid_pairing_uri() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "pair",
                "not-a-uri",
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        assert_eq!(outcome.exit, 2);
        assert_eq!(outcome.error.unwrap().code, "invalid_argument");
    }

    #[tokio::test]
    async fn doctor_export_requires_yes_in_non_interactive_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostic.json");
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
                "--export",
                path.to_str().unwrap(),
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        if std::io::stdin().is_terminal() {
            return;
        }
        assert_eq!(outcome.exit, 2);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn doctor_export_with_yes_writes_redacted_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostic.json");
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
                "--export",
                path.to_str().unwrap(),
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        assert_eq!(outcome.exit, 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("whalerein/v1"));
        assert!(text.contains("address"));
        assert!(!text.contains("private key\""));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["fields"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn failures_have_exportable_diagnostics_and_can_be_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "whalerein",
            "hosts",
            "--data-dir",
            dir.path().to_str().unwrap(),
        ])
        .unwrap();
        let outcome = run_with(cli, Arc::new(MemorySecretStore::new())).await;
        let id = outcome.to_json()["error"]["diagnostic_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(id.len(), 32);
        assert!(outcome.render().contains(&id));
        assert_eq!(outcome.to_json()["error"]["layer"], "core");
        let record = whalerein_core::api::test_support::load_failure(dir.path()).unwrap();
        assert_eq!(record.trace_id, id);
        assert_eq!(record.events.last().unwrap().code, "not_paired");
        let cli = Cli::try_parse_from([
            "whalerein",
            "doctor",
            "--clear-diagnostics",
            "--data-dir",
            dir.path().to_str().unwrap(),
        ])
        .unwrap();
        let outcome = run_with(cli, Arc::new(MemorySecretStore::new())).await;
        assert_eq!(outcome.exit, 0);
        assert!(whalerein_core::api::test_support::load_failure(dir.path()).is_none());
        assert!(Cli::try_parse_from([
            "whalerein",
            "doctor",
            "--clear-diagnostics",
            "--export",
            "x"
        ])
        .is_err());
    }

    #[tokio::test]
    async fn completes_cli_commands_against_fake_access() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> =
            Arc::new(FileSecretStore::new(dir.path().join("secrets")));
        let data_dir = dir.path().to_str().unwrap().to_owned();

        async fn run_args(
            data_dir: &str,
            secrets: Arc<dyn SecretStore>,
            command: &[&str],
        ) -> CliOutcome {
            let mut args = vec!["whalerein", "--json", "--data-dir", data_dir];
            args.extend_from_slice(command);
            match tokio::time::timeout(Duration::from_secs(10), run_with(cli(&args), secrets)).await
            {
                Ok(outcome) => outcome,
                Err(_) => panic!("command timed out: {command:?}"),
            }
        }

        let pairing_uri = access.pairing_uri();
        let paired = run_args(&data_dir, secrets.clone(), &["pair", &pairing_uri, "--yes"]).await;
        assert_eq!(paired.exit, 0, "{paired:?}");
        let device_id = paired.to_json()["data"]["device_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(!paired.to_json()["data"]["fingerprint"]
            .as_str()
            .unwrap()
            .is_empty());

        let hosts = run_args(&data_dir, secrets.clone(), &["hosts"]).await;
        assert_eq!(hosts.exit, 0);
        assert_eq!(hosts.to_json()["data"]["hosts"][0]["host_id"], "host-1");

        let sessions = run_args(
            &data_dir,
            secrets.clone(),
            &["sessions", "--host", "host-1"],
        )
        .await;
        assert_eq!(sessions.exit, 0);
        assert_eq!(
            sessions.to_json()["data"]["sessions"][0]["session_id"],
            "session-1"
        );

        let sent = run_args(
            &data_dir,
            secrets.clone(),
            &["send", "--session", "session-1", "continue"],
        )
        .await;
        assert_eq!(sent.exit, 0);
        assert_eq!(sent.to_json()["data"]["status"], "succeeded");
        assert_eq!(sent.to_json()["data"]["message_id"], "message-1");
        assert!(sent.to_json()["data"]["request_id"].as_str().is_some());

        let approved = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "approve",
                "--request",
                "approval-1",
                "--digest",
                &"ab".repeat(32),
                "--version",
                "3",
            ],
        )
        .await;
        assert_eq!(approved.exit, 0);
        assert_eq!(approved.to_json()["command"], "approve");

        let rejected = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "reject",
                "--request",
                "approval-2",
                "--digest",
                &"cd".repeat(32),
                "--version",
                "4",
            ],
        )
        .await;
        assert_eq!(rejected.exit, 0);

        let interrupted = run_args(
            &data_dir,
            secrets.clone(),
            &["interrupt", "--session", "session-1"],
        )
        .await;
        assert_eq!(interrupted.exit, 0);

        let tailed = run_args(
            &data_dir,
            secrets.clone(),
            &["sync", "--session", "session-1"],
        )
        .await;
        assert_eq!(tailed.exit, 0, "{tailed:?}");
        assert_eq!(tailed.to_json()["data"]["events"], 1);

        let doctor = run_args(&data_dir, secrets.clone(), &["doctor"]).await;
        assert_eq!(doctor.exit, 0);
        assert!(doctor.to_json()["data"]["commands"]["non_terminal"].is_number());
        assert!(doctor.to_json()["data"].get("connection").is_none());
        assert!(doctor.to_json()["data"].get("detail").is_none());

        let export_path = dir.path().join("bundle.json");
        let exported = run_args(
            &data_dir,
            secrets.clone(),
            &["doctor", "--export", export_path.to_str().unwrap(), "--yes"],
        )
        .await;
        assert_eq!(exported.exit, 0);
        assert!(export_path.exists());
        let bundle = std::fs::read_to_string(&export_path).unwrap();
        assert!(!bundle.contains("continue"));
        assert!(!bundle.contains("test session"));

        let revoked = run_args(&data_dir, secrets.clone(), &["revoke"]).await;
        assert_eq!(revoked.exit, 0);
        assert_eq!(revoked.to_json()["data"]["revoked"], true);
        assert_eq!(revoked.to_json()["data"]["device_id"], device_id);

        let after = run_args(&data_dir, secrets, &["hosts"]).await;
        assert_eq!(after.exit, 4);
    }
}

#[cfg(test)]
mod forget_tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::test_support::FakeAccess;
    #[tokio::test]
    async fn only_explicit_local_forget_bypasses_active_grant_check() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecretStore::new());
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: secrets.clone(),
        })
        .unwrap();
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let normal = Cli::try_parse_from([
            "whalerein",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "forget",
        ])
        .unwrap();
        assert_eq!(run_with(normal, secrets.clone()).await.exit, 8);
        let local = Cli::try_parse_from([
            "whalerein",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "forget",
            "--local-only",
        ])
        .unwrap();
        let result = run_with(local, secrets.clone()).await;
        assert_eq!(result.exit, 0);
        assert!(result.human.contains("server entry may remain"));
        assert!(secrets.is_empty());
    }
}
