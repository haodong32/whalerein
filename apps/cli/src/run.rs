//! Command execution. All protocol behavior lives in `whalerein-core`.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use whalerein_core::api::{
    default_data_dir, CreateSessionInput, Environment, ExportManifest, MemorySecretStore,
    PairingDecision, PendingQueueAction, ProfileSelection, ProfileSummary, Profiles,
    RemovalOutcome, SecretStore, WaitReason,
};
use whalerein_core::{Core, CoreError, CoreOptions};

use crate::cli::{Cli, Command, ModelAction, ProfileAction, QueueCommand};
use crate::output::{
    command_data, core_error_outcome, doctor_data, hosts_data, is_tail_output,
    lifecycle_error_code, lifecycle_exit_code, manifest_data, render_approvals, render_command,
    render_doctor, render_hosts, render_lifecycle_outcome, render_manifest, render_pair,
    render_sessions, sessions_data, tail_event_json, CliError, CliOutcome, TailRenderer,
};

const DEFAULT_TAIL_TIMEOUT: Duration = Duration::from_secs(15);

fn parse_wait_seconds(value: &str) -> Result<u64, &'static str> {
    let invalid = "timeout must be positive seconds with at most three decimal places";
    let (whole, fraction) = value
        .split_once('.')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    if whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) {
        return Err(invalid);
    }
    let seconds: u64 = whole.parse().map_err(|_| invalid)?;
    let milliseconds = match fraction {
        None => 0,
        Some(part)
            if !part.is_empty() && part.len() <= 3 && part.bytes().all(|c| c.is_ascii_digit()) =>
        {
            part.parse::<u64>().map_err(|_| invalid)? * 10_u64.pow(3 - part.len() as u32)
        }
        Some(_) => return Err(invalid),
    };
    let total = seconds
        .checked_mul(1000)
        .and_then(|v| v.checked_add(milliseconds))
        .ok_or(invalid)?;
    if total == 0 || total > 2_147_483_647 {
        return Err(invalid);
    }
    Ok(total)
}

async fn queue_change(
    core: &Core,
    command: &Command,
    session: &str,
    item: &str,
    action: PendingQueueAction,
    text: &str,
) -> Result<CliOutcome, CoreError> {
    let handle = core
        .update_pending_queue(session, item, action, text)
        .await?;
    let refresh = core.pending_queue(session).await;
    let refresh_status = match refresh {
        Ok(queue) if queue.available => "current",
        _ => "unavailable",
    };
    let data = json!({"command":command_data(&handle), "refresh":refresh_status});
    let human = format!(
        "{}Queue refresh: {}\n",
        render_command(&handle),
        refresh_status
    );
    Ok(CliOutcome::success(command, data, human))
}

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
    if let Command::Profile { action } = &command {
        return match profile(&data_dir, &secrets, &command, action) {
            Ok(outcome) => outcome,
            Err(error) => core_error_outcome(&command, error),
        };
    }
    let core = match Core::new(CoreOptions { data_dir, secrets }) {
        Ok(core) => core,
        Err(error) => return core_error_outcome(&command, error),
    };

    let core = match cli.host.as_deref() {
        Some(host) => match core.with_session_host(host) {
            Ok(scoped) => scoped,
            Err(error) => return core_error_outcome(&command, error),
        },
        None => core,
    };

    let core = if let Some(session) = command.session_id() {
        let key = match core.session_key(session).await {
            Ok(key) => key,
            Err(error) => return core_error_outcome(&command, error),
        };
        match core.with_session_host(&key.host_id) {
            Ok(scoped) => scoped,
            Err(error) => return core_error_outcome(&command, error),
        }
    } else {
        core
    };

    let result = match &command {
        Command::Profile { .. } => unreachable!("profile commands are dispatched before Core"),
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
        Command::PairingHistory => match core.pairing_history() {
            Ok(records) => {
                let human = serde_json::to_string_pretty(&records).unwrap_or_else(|_| "[]".into());
                Ok(CliOutcome::success(
                    &command,
                    json!({ "pairings": records }),
                    format!("{human}\n"),
                ))
            }
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
        Command::Models { host } => match core.list_models(host).await {
            Ok(catalog) => Ok(CliOutcome::success(
                &command,
                json!(catalog),
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&catalog).unwrap_or_default()
                ),
            )),
            Err(error) => Err(error),
        },
        Command::Model {
            action: ModelAction::Get { session },
        } => match core.read_session_model(session).await {
            Ok(selection) => Ok(CliOutcome::success(
                &command,
                json!(selection),
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&selection).unwrap_or_default()
                ),
            )),
            Err(error) => Err(error),
        },
        Command::Model {
            action:
                ModelAction::Set {
                    session,
                    provider,
                    model,
                    effort,
                },
        } => match core
            .select_session_model(session, provider, model, effort.as_deref())
            .await
        {
            Ok(handle) => Ok(CliOutcome::success(
                &command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(error) => Err(error),
        },
        Command::SessionFork { session } => match core.fork_session(session).await {
            Ok(handle) => {
                let created = handle.lifecycle_receipt.as_ref().is_some_and(|receipt| {
                    !receipt.session_id.is_empty()
                        && matches!(receipt.disposition.as_str(), "READY" | "READY_WITH_WARNING")
                });
                let cwd = match &handle.lifecycle_receipt {
                    Some(receipt) if created => core
                        .get_session(&receipt.session_id)
                        .await
                        .ok()
                        .map(|s| s.cwd)
                        .filter(|s| !s.is_empty()),
                    _ => None,
                };
                let human = match &handle.lifecycle_receipt {
                    Some(receipt) if created => format!(
                        "Session {} forked from {}. The child shares the working directory; no worktree is created.{}{}\n",
                        receipt.session_id,
                        session,
                        cwd.as_ref().map(|dir| format!(" Directory: {dir}.")).unwrap_or_default(),
                        if receipt.warning_code.is_some() {
                            " Workspace attachment is incomplete; the child exists and must not be retried or deleted."
                        } else {
                            ""
                        }
                    ),
                    _ => render_lifecycle_outcome(&handle).unwrap_or_else(|| render_command(&handle)),
                };
                let mut data = command_data(&handle);
                if let Some(cwd) = cwd {
                    data["working_directory"] = json!(cwd);
                }
                Ok(CliOutcome::lifecycle(
                    &command,
                    &handle,
                    lifecycle_error_code(&handle),
                    human,
                    lifecycle_exit_code(&handle),
                ))
            }
            Err(error) => Err(error),
        },
        Command::SessionCreate {
            host,
            cwd,
            workspace,
            preset,
            session_id,
        } => {
            match core
                .create_session(
                    host,
                    CreateSessionInput {
                        cwd: cwd.clone(),
                        workspace_id: workspace.clone(),
                        agent_preset: preset.clone(),
                        preferred_session_id: session_id.clone(),
                    },
                )
                .await
            {
                Ok(handle) => {
                    let human = match &handle.lifecycle_receipt {
                        Some(receipt) if receipt.disposition == "READY_WITH_WARNING" => format!(
                            "Session {} is ready with a warning: {}.\nThe session was created; do not retry or delete it.\n",
                            receipt.session_id,
                            receipt.warning_code.as_deref().unwrap_or("workspace attach incomplete")
                        ),
                        Some(receipt) if receipt.disposition == "READY" => {
                            format!("Session {} is ready.\n", receipt.session_id)
                        }
                        _ => render_lifecycle_outcome(&handle)
                            .unwrap_or_else(|| render_command(&handle)),
                    };
                    Ok(CliOutcome::lifecycle(
                        &command,
                        &handle,
                        lifecycle_error_code(&handle),
                        human,
                        lifecycle_exit_code(&handle),
                    ))
                }
                Err(error) => Err(error),
            }
        }
        Command::CommandsPending => match (
            if core.is_paired() {
                core.pending_commands()
            } else {
                Ok(Vec::new())
            },
            core.unconfirmed_revoked_commands(),
            core.revoked_identity_retention_unverified(),
            core.authorization_verification_pending(),
            core.revocation_cleanup_requires_inspection(),
        ) {
            (
                Ok(commands),
                Ok(revoked),
                Ok(retention_unverified),
                Ok(authorization_pending),
                Ok(cleanup_inspection),
            ) => {
                let values: Vec<_> = commands.iter().map(|item| json!({"request_id": item.request_id, "command_id": item.command_id, "host_id": item.host_id, "status": item.status.as_str()})).collect();
                let mut human = String::new();
                for item in &commands {
                    human.push_str(&format!(
                        "request: {}  command: {}  host: {}  status: {}\n",
                        item.request_id,
                        item.command_id,
                        item.host_id,
                        item.status.as_str()
                    ));
                }
                for request in &revoked {
                    human.push_str(&format!("unconfirmed after revocation: request {request} (manual verification required)\n"));
                }
                if cleanup_inspection {
                    human.push_str(
                        "Revocation cleanup needs manual inspection before pairing again.\n",
                    );
                }
                if authorization_pending {
                    human.push_str("Authorization status is unconfirmed; inspect the pairing or retry revocation.\n");
                }
                if retention_unverified {
                    human.push_str("Unable to verify whether all unconfirmed command or pairing identities were retained during revocation.\n");
                }
                if human.is_empty() {
                    human.push_str("No pending commands.\n");
                }
                Ok(CliOutcome::success(
                    &command,
                    json!({"commands": values, "unconfirmed_after_revocation": revoked, "revoked_retention_unverified": retention_unverified, "authorization_verification_pending": authorization_pending, "revocation_cleanup_requires_inspection": cleanup_inspection}),
                    human,
                ))
            }
            (Err(error), _, _, _, _)
            | (_, Err(error), _, _, _)
            | (_, _, Err(error), _, _)
            | (_, _, _, Err(error), _)
            | (_, _, _, _, Err(error)) => Err(error),
        },
        Command::Recover { request } => match core.recover_pending_command(request).await {
            Ok(handle) => Ok(CliOutcome::success(
                &command,
                command_data(&handle),
                render_command(&handle),
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
        Command::Wait {
            session,
            timeout,
            fail_on_timeout,
        } => {
            let timeout_ms = match timeout.as_deref().map(parse_wait_seconds).transpose() {
                Ok(value) => value.unwrap_or(30_000),
                Err(error) => {
                    return CliOutcome::local_failure(&command, "invalid_argument", error, 2)
                }
            };
            match core.wait_session(session, timeout_ms).await {
                Ok(observation) => {
                    let mut outcome = CliOutcome::success(
                        &command,
                        serde_json::to_value(&observation).unwrap_or_else(|_| json!({})),
                        format!(
                            "Session {}: {:?}\n",
                            observation.session_id, observation.reason
                        ),
                    );
                    if *fail_on_timeout && observation.reason == WaitReason::Timeout {
                        outcome.exit = 7;
                    }
                    Ok(outcome)
                }
                Err(error) => Err(error),
            }
        }
        Command::Sync { session } => match core.sync_session(session).await {
            Ok(events) => Ok(CliOutcome::success(
                &command,
                json!({ "session_id": session, "events": events }),
                format!("Synced session {session} ({events} event(s)).\n"),
            )),
            Err(error) => Err(error),
        },
        Command::Send {
            session,
            text,
            delivery,
        } => match core
            .send_message_with_delivery(session, text, (*delivery).into())
            .await
        {
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
        Command::Questions { session, cursor } => {
            match core.list_questions(session, cursor).await {
                Ok(page) => {
                    let human = serde_json::to_string_pretty(&page).unwrap_or_else(|_| "{}".into());
                    Ok(CliOutcome::success(
                        &command,
                        json!(page),
                        format!("{human}\n"),
                    ))
                }
                Err(error) => Err(error),
            }
        }
        Command::Answer {
            session,
            batch,
            epoch,
            version,
            digest,
            phase,
            input,
            command_id,
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
            let answers = match read_question_answers(input) {
                Ok(answers) => answers,
                Err(error) => {
                    return CliOutcome::local_failure(
                        &command,
                        error.code.as_str(),
                        error.message,
                        2,
                    )
                }
            };
            match core.get_question(session, batch).await {
                Ok(current)
                    if current.source_epoch == *epoch
                        && current.version == *version
                        && current.digest == digest
                        && current.phase == *phase =>
                {
                    let actual_id = command_id.clone().unwrap_or_else(Core::new_command_id);
                    let answer = core.answer_question_with_id(&current, answers, Some(&actual_id));
                    let (outcome, interrupted) = if current.phase == "QUESTION_OPEN" {
                        match wait_open_answer(answer, tokio::signal::ctrl_c(), || core.cancel_question_answer(session, &actual_id),
                            Duration::from_millis(whalerein_core::api::CONTROL_REQUEST_TTL_MS), Duration::from_millis(250)).await {
                            OpenAnswerWait::Finished(result, interrupted) => (*result, interrupted),
                            OpenAnswerWait::CancellationRequested => return CliOutcome::local_failure(&command, "unknown", format!("Cancellation requested for command {actual_id}; recover its result."), 7),
                            OpenAnswerWait::CancellationUnconfirmed => return CliOutcome::local_failure(&command, "unknown", format!("Cancellation was not confirmed for command {actual_id}; recover its result."), 7),
                        }
                    } else {
                        (answer.await, false)
                    };
                    match outcome {
                        Ok(handle) => Ok(CliOutcome::success(
                            &command,
                            command_data(&handle),
                            render_command(&handle),
                        )),
                        Err(error)
                            if interrupted
                                && matches!(
                                    error.code(),
                                    "unknown" | "channel_mismatch" | "unavailable" | "timeout"
                                ) =>
                        {
                            return CliOutcome::local_failure(&command, "unknown", format!("Outcome of command {actual_id} is unconfirmed; recover its result."), 7);
                        }
                        Err(error) => Err(error),
                    }
                }
                Ok(_) => Err(CoreError::Conflict(
                    "question changed; refresh before answering".into(),
                )),
                Err(error) if error.code() == "not_found" => Err(CoreError::Conflict(
                    "question unavailable; refresh before answering".into(),
                )),
                Err(error) => Err(error),
            }
        }
        Command::QuestionCancel {
            session,
            command: command_id,
        } => match core.cancel_question_answer(session, command_id).await {
            Ok(requested) => Ok(CliOutcome::success(
                &command,
                json!({"cancellation_requested": requested}),
                if requested {
                    "Cancellation requested; recover the command result to confirm its outcome.\n"
                } else {
                    "No matching active answer; it may have completed or not reached the Host. Recover its result.\n"
                },
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
        Command::Queue { action } => match action {
            QueueCommand::List { session } => match core.pending_queue(session).await {
                Ok(queue) => {
                    let data = serde_json::to_value(&queue).unwrap_or_else(|_| json!({}));
                    let human = if queue.available {
                        format!(
                            "{}\n",
                            serde_json::to_string_pretty(&data).unwrap_or_default()
                        )
                    } else {
                        "Queue unavailable (session may be cold).\n".to_owned()
                    };
                    Ok(CliOutcome::success(&command, data, human))
                }
                Err(error) => Err(error),
            },
            QueueCommand::Edit {
                session,
                item,
                text,
            } => {
                queue_change(
                    &core,
                    &command,
                    session,
                    item,
                    PendingQueueAction::Edit,
                    text,
                )
                .await
            }
            QueueCommand::Remove { session, item } => {
                queue_change(
                    &core,
                    &command,
                    session,
                    item,
                    PendingQueueAction::Remove,
                    "",
                )
                .await
            }
            QueueCommand::Steer { session, item } => {
                queue_change(
                    &core,
                    &command,
                    session,
                    item,
                    PendingQueueAction::Promote,
                    "",
                )
                .await
            }
        },
        Command::Interrupt { session } => match core.interrupt(session).await {
            Ok(handle) => Ok(CliOutcome::success(
                &command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(error) => Err(error),
        },
        Command::Revoke => match core.revoke().await {
            Ok(outcome) => {
                let unverified = matches!(
                    outcome.retention,
                    whalerein_core::api::RevocationRetention::Unverified
                );
                let mut human =
                    "Revoked this device's grants. Local identity was kept.\n".to_owned();
                if unverified {
                    human.push_str("Unable to verify whether unconfirmed command or pairing identities were retained.\n");
                }
                Ok(CliOutcome::success(
                    &command,
                    json!({ "device_id":outcome.device_id, "revoked":true, "revoked_retention_unverified":unverified }),
                    human,
                ))
            }
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
    let started = tokio::time::Instant::now();
    let key = match timeout {
        Some(limit) => tokio::time::timeout(limit, core.session_key(session))
            .await
            .map_err(|_| CoreError::Timeout)??,
        None => core.session_key(session).await?,
    };
    let timeout = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
    if timeout.is_some_and(|remaining| remaining.is_zero()) {
        return Err(CoreError::Timeout);
    }
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

fn profile(
    data_dir: &std::path::Path,
    secrets: &Arc<dyn SecretStore>,
    command: &Command,
    action: &ProfileAction,
) -> Result<CliOutcome, CoreError> {
    let profiles = Profiles::new(data_dir);
    match action {
        ProfileAction::List { include_staging } => {
            let profiles_list = profiles.list(secrets, *include_staging)?;
            let data = json!({ "profiles": profiles_list });
            let human = render_profile_list(&profiles_list);
            Ok(CliOutcome::success(command, data, human))
        }
        ProfileAction::Current => {
            let selection = profiles.current(secrets)?;
            let data = serde_json::to_value(&selection)
                .unwrap_or_else(|_| json!({ "registry_present": selection.registry_present }));
            Ok(CliOutcome::success(
                command,
                data,
                render_profile_current(&selection),
            ))
        }
        ProfileAction::Add {
            name,
            staging,
            enable_testing,
            api_origin,
            issuer,
            client_id,
            access_url,
        } => {
            let custom_supplied = api_origin.is_some()
                || issuer.is_some()
                || client_id.is_some()
                || access_url.is_some();
            if *staging && custom_supplied {
                return Err(CoreError::InvalidInput(
                    "--staging cannot be combined with custom service arguments".into(),
                ));
            }
            let summary = if *staging {
                if !*enable_testing {
                    return Err(CoreError::Unsupported);
                }
                profiles.add_official_staging(name, true, secrets)?
            } else {
                if *enable_testing {
                    return Err(CoreError::InvalidInput(
                        "--enable-testing requires --staging".into(),
                    ));
                }
                let api_origin = api_origin.as_ref().ok_or_else(|| {
                    CoreError::InvalidInput("custom profile requires --api-origin".into())
                })?;
                let issuer = issuer.as_ref().ok_or_else(|| {
                    CoreError::InvalidInput("custom profile requires --issuer".into())
                })?;
                let client_id = client_id.as_ref().ok_or_else(|| {
                    CoreError::InvalidInput("custom profile requires --client-id".into())
                })?;
                profiles.add_custom(
                    name,
                    whalerein_core::api::CustomServiceInput {
                        api_origin: api_origin.clone(),
                        issuer: issuer.clone(),
                        native_client_id: client_id.clone(),
                        access_url: access_url.clone(),
                    },
                    secrets,
                )?
            };
            let data = json!({ "profile": summary, "current": false });
            let human = format!(
                "Added profile {} ({}); active profile unchanged.\n",
                summary.name,
                environment_label(summary.environment)
            );
            Ok(CliOutcome::success(command, data, human))
        }
        ProfileAction::Use { name } => {
            let outcome = profiles.activate(name)?;
            let data = serde_json::to_value(&outcome)
                .unwrap_or_else(|_| json!({ "profile": outcome.profile }));
            let mut human = format!(
                "Active profile: {}\nEnvironment: {}\nService: {}\n",
                outcome.profile.name,
                environment_label(outcome.profile.environment),
                profile_service_label(&outcome.profile),
            );
            if let Some(service) = &outcome.profile.service {
                if service.environment() == Environment::Custom {
                    human.push_str(&format!("Issuer: {}\n", service.issuer_display()));
                }
            }
            if let Some(warning) = &outcome.rollback_warning {
                human.push_str(&format!("Warning: {warning}\n"));
            }
            Ok(CliOutcome::success(command, data, human))
        }
        ProfileAction::Remove { name } => {
            let outcome = profiles.remove(name, secrets)?;
            let data = serde_json::to_value(&outcome)
                .unwrap_or_else(|_| json!({ "remote_revocation": outcome.remote_revocation }));
            Ok(CliOutcome::success(
                command,
                data,
                render_profile_removal(&outcome),
            ))
        }
    }
}

fn environment_label(environment: Environment) -> &'static str {
    match environment {
        Environment::Production => "Production",
        Environment::Staging => "Staging (testing only)",
        Environment::Custom => "Custom",
        Environment::Standalone => "Standalone",
    }
}

fn profile_service_label(profile: &ProfileSummary) -> &'static str {
    profile
        .service
        .as_ref()
        .map_or("Standalone", whalerein_core::api::ServiceDescriptor::label)
}

fn render_profile_list(profiles: &[ProfileSummary]) -> String {
    if profiles.is_empty() {
        return "No local profiles.\n".into();
    }
    let mut output = String::from("NAME\tENVIRONMENT\tCURRENT\n");
    for profile in profiles {
        output.push_str(&format!(
            "{}\t{}\t{}\n",
            profile.name,
            environment_label(profile.environment),
            if profile.active { "yes" } else { "no" }
        ));
    }
    output
}

fn render_profile_current(selection: &ProfileSelection) -> String {
    match &selection.active {
        Some(profile) => {
            let mut output = format!(
                "profile: {}\nenvironment: {}\nservice: {}\n",
                profile.name,
                environment_label(profile.environment),
                profile_service_label(profile),
            );
            if let Some(service) = &profile.service {
                if service.environment() == Environment::Custom {
                    output.push_str(&format!("issuer: {}\n", service.issuer_display()));
                }
            }
            output.push_str(&format!(
                "legacy: {}\naccount: {}\n",
                if profile.legacy { "yes" } else { "no" },
                if profile.account.is_some() {
                    "bound"
                } else {
                    "signed out"
                }
            ));
            output
        }
        None => {
            let default = selection
                .default_service
                .as_ref()
                .map_or("none", whalerein_core::api::ServiceDescriptor::label);
            format!("profile: none\naccount service: {default} (default)\nstandalone: available\n")
        }
    }
}

fn render_profile_removal(outcome: &RemovalOutcome) -> String {
    let mut output = format!(
        "Removed profile {} (local state only).\nRemote grants and account sessions were not revoked.\n",
        outcome.removed.name
    );
    match &outcome.active.active {
        Some(profile) => output.push_str(&format!(
            "Active profile: {} ({})\n",
            profile.name,
            environment_label(profile.environment)
        )),
        None => output.push_str("Active profile: none\n"),
    }
    output
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

enum OpenAnswerWait {
    Finished(
        Box<whalerein_core::api::CoreResult<whalerein_core::api::CommandHandle>>,
        bool,
    ),
    CancellationRequested,
    CancellationUnconfirmed,
}

async fn wait_open_answer<A, I, C, F>(
    answer: A,
    interrupt: I,
    mut cancel: C,
    timeout: Duration,
    retry: Duration,
) -> OpenAnswerWait
where
    A: std::future::Future<
        Output = whalerein_core::api::CoreResult<whalerein_core::api::CommandHandle>,
    >,
    I: std::future::Future<Output = std::io::Result<()>>,
    C: FnMut() -> F,
    F: std::future::Future<Output = whalerein_core::api::CoreResult<bool>>,
{
    tokio::pin!(answer);
    tokio::select! {
        result = &mut answer => OpenAnswerWait::Finished(Box::new(result), false),
        signal = interrupt => {
            if signal.is_err() { return OpenAnswerWait::Finished(Box::new(answer.await), false); }
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                tokio::select! {
                    result = &mut answer => return OpenAnswerWait::Finished(Box::new(result), true),
                    cancellation = cancel() => {
                        if matches!(cancellation, Ok(true)) { return OpenAnswerWait::CancellationRequested; }
                    },
                    _ = tokio::time::sleep_until(deadline) => return OpenAnswerWait::CancellationUnconfirmed,
                }
                tokio::select! {
                    result = &mut answer => return OpenAnswerWait::Finished(Box::new(result), true),
                    _ = tokio::time::sleep(retry) => {},
                    _ = tokio::time::sleep_until(deadline) => return OpenAnswerWait::CancellationUnconfirmed,
                }
            }
        },
    }
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

fn read_question_answers(
    path: &std::path::Path,
) -> Result<Vec<whalerein_core::api::QuestionAnswer>, CliError> {
    #[derive(serde::Deserialize)]
    struct Input {
        answers: Vec<whalerein_core::api::QuestionAnswer>,
    }
    let mut text = String::new();
    let result = if path == std::path::Path::new("-") {
        std::io::stdin().read_to_string(&mut text)
    } else {
        std::fs::File::open(path).and_then(|mut file| file.read_to_string(&mut text))
    };
    result.map_err(|_| CliError::new("invalid_argument", "could not read answer input"))?;
    serde_json::from_str::<Input>(&text)
        .map(|input| input.answers)
        .map_err(|_| CliError::new("invalid_argument", "invalid structured answer input"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn wait_seconds_are_exact_and_bounded() {
        for (input, expected) in [
            ("0.001", 1),
            ("30", 30_000),
            ("1.25", 1_250),
            ("2147483.647", 2_147_483_647),
        ] {
            assert_eq!(super::parse_wait_seconds(input), Ok(expected));
        }
        for input in [
            "",
            "0",
            "0.000",
            "-1",
            ".1",
            "1.",
            "1.0001",
            "2147483.648",
            "99999999999999999999",
        ] {
            assert!(super::parse_wait_seconds(input).is_err());
        }
    }
    use super::*;
    use clap::Parser;
    use whalerein_core::api::test_support::FakeAccess;
    use whalerein_core::FileSecretStore;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[tokio::test]
    async fn session_create_displays_confirmed_identity_and_capability_denial() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let pair = cli(&[
            "whalerein",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            &access.pairing_uri(),
        ]);
        assert_eq!(run_with(pair, secrets.clone()).await.exit, 0);
        let make = || {
            cli(&[
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "session-create",
                "--host",
                "host-1",
            ])
        };
        let denied = run_with(make(), secrets.clone()).await;
        assert_eq!(denied.exit, 9);
        assert_eq!(access.host.lock().unwrap().commands_seen, 0);
        access.host.lock().unwrap().session_create_supported = true;
        let result = run_with(make(), secrets).await;
        assert_eq!(result.exit, 0);
        assert_eq!(
            result.to_json()["data"]["lifecycle_receipt"]["session_id"],
            "created-1"
        );
        assert!(result.render().contains("created-1"));
    }

    #[tokio::test]
    async fn revoke_without_cache_reports_unverified_retention_as_warning() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let path = dir.path().to_str().unwrap();
        assert_eq!(
            run_with(
                cli(&[
                    "whalerein",
                    "--yes",
                    "--data-dir",
                    path,
                    "pair",
                    &access.pairing_uri()
                ]),
                secrets.clone()
            )
            .await
            .exit,
            0
        );
        let outcome = run_with(cli(&["whalerein", "--data-dir", path, "revoke"]), secrets).await;
        assert_eq!(outcome.exit, 0);
        assert_eq!(
            outcome.to_json()["data"]["revoked_retention_unverified"],
            true
        );
        assert!(outcome.render().contains("Unable to verify"));
    }

    #[tokio::test]
    async fn revoked_pending_identity_is_visible_without_a_pairing() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let path = dir.path().to_str().unwrap();
        assert_eq!(
            run_with(
                cli(&[
                    "whalerein",
                    "--yes",
                    "--data-dir",
                    path,
                    "pair",
                    &access.pairing_uri()
                ]),
                secrets.clone()
            )
            .await
            .exit,
            0
        );
        {
            let mut host = access.host.lock().unwrap();
            host.session_create_supported = true;
            host.settled_command_status = whalerein_core::api::test_support::fake_unknown_status();
        }
        assert_ne!(
            run_with(
                cli(&[
                    "whalerein",
                    "--data-dir",
                    path,
                    "session-create",
                    "--host",
                    "host-1"
                ]),
                secrets.clone()
            )
            .await
            .exit,
            0
        );
        let before = run_with(
            cli(&["whalerein", "--data-dir", path, "commands-pending"]),
            secrets.clone(),
        )
        .await
        .to_json();
        let request = before["data"]["commands"][0]["request_id"]
            .as_str()
            .unwrap();
        assert_eq!(
            run_with(
                cli(&["whalerein", "--data-dir", path, "revoke"]),
                secrets.clone()
            )
            .await
            .exit,
            0
        );
        let after = run_with(
            cli(&["whalerein", "--data-dir", path, "commands-pending"]),
            secrets,
        )
        .await;
        assert_eq!(after.exit, 0);
        assert_eq!(
            after.to_json()["data"]["unconfirmed_after_revocation"][0],
            request
        );
        assert!(after.render().contains("manual verification required"));
    }

    #[tokio::test]
    async fn pending_command_can_be_listed_and_recovered_without_reexecution() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let pair = cli(&[
            "whalerein",
            "--yes",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "pair",
            &access.pairing_uri(),
        ]);
        assert_eq!(run_with(pair, secrets.clone()).await.exit, 0);
        {
            let mut host = access.host.lock().unwrap();
            host.session_create_supported = true;
            host.settled_command_status = whalerein_core::api::test_support::fake_unknown_status();
        }
        let create = cli(&[
            "whalerein",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "session-create",
            "--host",
            "host-1",
        ]);
        assert_ne!(run_with(create, secrets.clone()).await.exit, 0);
        let pending = cli(&[
            "whalerein",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "commands-pending",
        ]);
        let listing = run_with(pending, secrets.clone()).await;
        assert_eq!(listing.exit, 0);
        let value = listing.to_json();
        let commands = value["data"]["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 1);
        let request = commands[0]["request_id"].as_str().unwrap();
        assert!(listing.render().contains(request));
        access.host.lock().unwrap().settled_command_status =
            whalerein_core::api::test_support::fake_succeeded_status();
        let recover = cli(&[
            "whalerein",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "command-recover",
            "--request",
            request,
        ]);
        let recovered = run_with(recover, secrets).await;
        assert_eq!(recovered.exit, 0);
        assert_eq!(
            recovered.to_json()["data"]["lifecycle_receipt"]["session_id"],
            "created-1"
        );
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
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
                "queue",
                "list",
                "--session",
                "s",
            ],
            vec![
                "whalerein",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "queue",
                "remove",
                "--session",
                "s",
                "--item",
                "q",
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
            match tokio::time::timeout(
                Duration::from_secs(10),
                Box::pin(run_with(cli(&args), secrets)),
            )
            .await
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

        let catalog = run_args(&data_dir, secrets.clone(), &["models", "--host", "host-1"]).await;
        assert_eq!(catalog.exit, 0);
        assert_eq!(
            catalog.to_json()["data"]["default_selection"]["model"],
            "model"
        );
        let read = run_args(
            &data_dir,
            secrets.clone(),
            &["model", "get", "--session", "session-1"],
        )
        .await;
        assert_eq!(read.exit, 0);
        assert!(read.to_json()["data"]["pending"].is_null());
        let selected = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "model",
                "set",
                "--session",
                "session-1",
                "--provider",
                "provider",
                "--model",
                "model",
            ],
        )
        .await;
        assert_eq!(selected.exit, 0);
        assert_eq!(
            selected.to_json()["data"]["model_selection"]["default_persistence"],
            "unconfirmed"
        );
        assert!(selected.render().contains("provider: provider"));
        assert!(selected.render().contains("model: model"));
        assert!(selected
            .render()
            .contains("effect: session_and_source_default_attempt"));
        assert!(selected
            .render()
            .contains("default persistence: unconfirmed"));
        let waited = run_args(
            &data_dir,
            secrets.clone(),
            &["wait", "--session", "session-1", "--timeout", "3.000"],
        )
        .await;
        assert_eq!(waited.exit, 0);
        assert!(waited.human.contains("Idle"));
        assert_eq!(waited.to_json()["data"]["reason"], "idle");
        assert_eq!(waited.to_json()["data"]["state"]["queue_available"], false);
        let default_wait = run_args(
            &data_dir,
            secrets.clone(),
            &["wait", "--session", "session-1"],
        )
        .await;
        assert_eq!(default_wait.exit, 0);
        assert_eq!(default_wait.to_json()["data"]["reason"], "idle");
        // The slice leaves a small response allowance inside the 30s
        // transport budget; one millisecond of scheduling drift is allowed.
        let slice = access.host.lock().unwrap().wait_last_timeout_ms;
        assert!((27_900..=28_000).contains(&slice), "wait slice {slice}");
        access.host.lock().unwrap().wait_timeouts = usize::MAX;
        for (extra, exit) in [(false, 0), (true, 7)] {
            let mut args = vec!["wait", "--session", "session-1", "--timeout", "0.2"];
            if extra {
                args.push("--fail-on-timeout");
            }
            let outcome = run_args(&data_dir, secrets.clone(), &args).await;
            assert_eq!(outcome.exit, exit);
            assert_eq!(outcome.to_json()["data"]["reason"], "timeout");
        }
        access.host.lock().unwrap().wait_timeouts = 0;
        access.host.lock().unwrap().set_query_unavailable(true);
        let transport_failure = run_args(
            &data_dir,
            secrets.clone(),
            &["wait", "--session", "session-1"],
        )
        .await;
        assert_eq!(transport_failure.to_json()["error"]["code"], "unavailable");
        access.host.lock().unwrap().set_query_unavailable(false);
        let invalid_wait = run_args(
            &data_dir,
            secrets.clone(),
            &["wait", "--session", "session-1", "--timeout", "0"],
        )
        .await;
        assert_eq!(invalid_wait.exit, 2);
        let queue = run_args(
            &data_dir,
            secrets.clone(),
            &["queue", "list", "--session", "session-1"],
        )
        .await;
        assert_eq!(queue.exit, 0);
        assert_eq!(queue.to_json()["data"]["items"][0]["item_id"], "queue-1");
        for action in ["edit", "remove", "steer"] {
            let mut args = vec![
                "queue",
                action,
                "--session",
                "session-1",
                "--item",
                "queue-1",
            ];
            if action == "edit" {
                args.push("new fixture");
            }
            let result = run_args(&data_dir, secrets.clone(), &args).await;
            assert_eq!(result.exit, 0);
            assert_eq!(result.to_json()["data"]["refresh"], "current");
            assert!(!result.to_json().to_string().contains("new fixture"));
        }
        access.host.lock().unwrap().fail_pending_queue = true;
        let result = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "queue",
                "remove",
                "--session",
                "session-1",
                "--item",
                "queue-1",
            ],
        )
        .await;
        assert_eq!(result.exit, 0);
        assert_eq!(result.to_json()["data"]["command"]["status"], "succeeded");
        assert_eq!(result.to_json()["data"]["refresh"], "unavailable");
        access.host.lock().unwrap().fail_pending_queue = false;
        let current = run_args(
            &data_dir,
            secrets.clone(),
            &["queue", "list", "--session", "session-1"],
        )
        .await;
        assert_eq!(current.exit, 0);
        assert_eq!(current.to_json()["data"]["items"][0]["item_id"], "queue-1");

        let questions = run_args(
            &data_dir,
            secrets.clone(),
            &["questions", "--session", "session-1"],
        )
        .await;
        assert_eq!(questions.exit, 0);
        assert_eq!(
            questions.to_json()["data"]["batches"][0]["batch_id"],
            "batch-1"
        );
        let answer_path = dir.path().join("answer.json");
        std::fs::write(
            &answer_path,
            r#"{"answers":[{"id":"q","selected":["A"],"custom":null}]}"#,
        )
        .unwrap();
        let answer = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "answer",
                "--session",
                "session-1",
                "--batch",
                "batch-1",
                "--epoch",
                "epoch",
                "--version",
                "1",
                "--digest",
                &"07".repeat(32),
                "--phase",
                "QUESTION_CONTINUED",
                "--input",
                answer_path.to_str().unwrap(),
            ],
        )
        .await;
        assert_eq!(answer.exit, 0, "{answer:?}");
        assert_eq!(
            answer.to_json()["data"]["question_receipt"]["phase"],
            "QUESTION_REPLY_QUEUED"
        );
        let stale = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "answer",
                "--session",
                "session-1",
                "--batch",
                "missing",
                "--epoch",
                "epoch",
                "--version",
                "1",
                "--digest",
                &"07".repeat(32),
                "--phase",
                "QUESTION_OPEN",
                "--input",
                answer_path.to_str().unwrap(),
            ],
        )
        .await;
        assert_eq!(stale.to_json()["error"]["code"], "conflict");
        std::fs::write(&answer_path, "synthetic-private-marker").unwrap();
        let bad = run_args(
            &data_dir,
            secrets.clone(),
            &[
                "answer",
                "--session",
                "session-1",
                "--batch",
                "batch-1",
                "--epoch",
                "epoch",
                "--version",
                "1",
                "--digest",
                &"07".repeat(32),
                "--phase",
                "QUESTION_CONTINUED",
                "--input",
                answer_path.to_str().unwrap(),
            ],
        )
        .await;
        assert_ne!(bad.exit, 0);
        assert!(!bad
            .to_json()
            .to_string()
            .contains("synthetic-private-marker"));

        let sent = run_args(
            &data_dir,
            secrets.clone(),
            &["send", "--session", "session-1", "continue"],
        )
        .await;
        assert_eq!(sent.exit, 0);
        assert_eq!(sent.to_json()["data"]["status"], "succeeded");
        assert_eq!(sent.to_json()["data"]["message_id"], "message-1");
        assert_eq!(sent.to_json()["data"]["delivery_id"], "delivery-1");
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
mod profile_tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::MemorySecretStore;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[tokio::test]
    async fn profile_current_defaults_to_official_production_without_state() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "profile",
                "current",
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["command"], "profile-current");
        assert!(value["data"]["active"].is_null());
        assert_eq!(
            value["data"]["default_service"]["kind"],
            "official-production"
        );
        assert_eq!(value["data"]["standalone_available"], true);
        assert!(!dir.path().join("profiles.json").exists());
    }

    #[tokio::test]
    async fn profile_staging_requires_explicit_testing_and_never_activates() {
        let dir = tempfile::tempdir().unwrap();
        let dir_text = dir.path().to_str().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let denied = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "add",
                "official-staging",
                "--staging",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(denied.exit, 9, "{denied:?}");
        assert_eq!(denied.to_json()["error"]["code"], "unsupported_capability");
        assert!(!dir.path().join("profiles.json").exists());

        let added = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "add",
                "official-staging",
                "--staging",
                "--enable-testing",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(added.exit, 0, "{added:?}");
        assert_eq!(added.to_json()["data"]["current"], false);
        assert_eq!(added.to_json()["data"]["profile"]["environment"], "staging");
        assert_eq!(added.to_json()["data"]["profile"]["testing"], true);

        let current = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "current",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(
            current.to_json()["data"]["active"]["name"],
            "official-production"
        );

        let hidden = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "list",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(
            hidden.to_json()["data"]["profiles"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let shown = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "list",
                "--include-staging",
            ]),
            secrets,
        )
        .await;
        assert_eq!(
            shown.to_json()["data"]["profiles"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(shown.to_json()["data"]["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["name"] == "official-staging" && value["environment"] == "staging"));
    }

    #[tokio::test]
    async fn profile_activation_labels_staging_and_persists_across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let dir_text = dir.path().to_str().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let added = run_with(
            cli(&[
                "whalerein",
                "--data-dir",
                dir_text,
                "profile",
                "add",
                "official-staging",
                "--staging",
                "--enable-testing",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(added.exit, 0, "{added:?}");
        let used = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "use",
                "official-staging",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(used.exit, 0, "{used:?}");
        assert_eq!(used.to_json()["data"]["profile"]["environment"], "staging");
        assert!(used.human.contains("Staging"));

        let current = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "current",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(
            current.to_json()["data"]["active"]["name"],
            "official-staging"
        );
        assert_eq!(current.to_json()["data"]["active"]["testing"], true);

        let removed = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "remove",
                "official-staging",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(removed.exit, 0, "{removed:?}");
        assert_eq!(
            removed.to_json()["data"]["remote_revocation"],
            "not_attempted"
        );
        assert_eq!(
            removed.to_json()["data"]["active"]["active"]["name"],
            "official-production"
        );
        let after = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "current",
            ]),
            secrets,
        )
        .await;
        assert_eq!(
            after.to_json()["data"]["active"]["name"],
            "official-production"
        );
    }

    #[tokio::test]
    async fn profile_custom_service_requires_every_explicit_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let dir_text = dir.path().to_str().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let missing = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "add",
                "corp",
                "--api-origin",
                "https://api.corp.example",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(missing.exit, 2, "{missing:?}");
        assert_eq!(missing.to_json()["error"]["code"], "invalid_argument");

        let added = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "add",
                "corp",
                "--api-origin",
                "https://api.corp.example",
                "--issuer",
                "https://id.corp.example",
                "--client-id",
                "whalerein-cli",
                "--access-url",
                "https://access.corp.example",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(added.exit, 0, "{added:?}");
        assert_eq!(added.to_json()["data"]["profile"]["environment"], "custom");
        let used = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "profile",
                "use",
                "corp",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(used.exit, 0, "{used:?}");
        assert_eq!(
            used.to_json()["data"]["profile"]["service"]["issuer"],
            "https://id.corp.example"
        );
        assert!(used.human.contains("https://id.corp.example"));
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
    #[tokio::test]
    async fn open_answer_cancellation_retries_and_never_assumes_the_outcome() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let never_answer = || {
            std::future::pending::<
                whalerein_core::api::CoreResult<whalerein_core::api::CommandHandle>,
            >()
        };
        let calls = AtomicUsize::new(0);
        let result = wait_open_answer(
            never_answer(),
            async { Ok(()) },
            || async { Ok(calls.fetch_add(1, Ordering::SeqCst) > 0) },
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, OpenAnswerWait::CancellationRequested));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let result = wait_open_answer(
            never_answer(),
            async { Ok(()) },
            || async { Ok(false) },
            Duration::from_millis(5),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, OpenAnswerWait::CancellationUnconfirmed));
        let result = wait_open_answer(
            async { Err(CoreError::UnknownResult) },
            std::future::pending(),
            || async { Ok(true) },
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(
            result,
            OpenAnswerWait::Finished(value, false) if matches!(*value, Err(CoreError::UnknownResult))
        ));
        let result = wait_open_answer(
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Err(CoreError::UnknownResult)
            },
            async { Ok(()) },
            std::future::pending,
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(
            result,
            OpenAnswerWait::Finished(value, true) if matches!(*value, Err(CoreError::UnknownResult))
        ));
        let result = wait_open_answer(
            async { Err(CoreError::UnknownResult) },
            async { Err(std::io::Error::other("signal")) },
            || async { Ok(false) },
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(
            result,
            OpenAnswerWait::Finished(value, false) if matches!(*value, Err(CoreError::UnknownResult))
        ));
    }
    #[tokio::test]
    async fn cli_wait_and_tail_deadlines_include_slow_host_discovery() {
        let cli = |args: &[&str]| Cli::try_parse_from(args).unwrap();
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: secrets.clone(),
        })
        .unwrap();
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        access.host.lock().unwrap().hosts_delay_ms = 2500;
        let started = std::time::Instant::now();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "wait",
                "--session",
                "session-1",
                "--timeout",
                "0.05",
                "--data-dir",
                dir.path().to_str().unwrap(),
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(outcome.to_json()["error"]["code"], "timeout");
        assert!(started.elapsed() < Duration::from_millis(900));
        let started = std::time::Instant::now();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "tail",
                "--session",
                "session-1",
                "--timeout",
                "1",
                "--data-dir",
                dir.path().to_str().unwrap(),
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.to_json()["error"]["code"], "timeout");
        assert!(started.elapsed() < Duration::from_millis(1800));
        assert_eq!(access.host.lock().unwrap().wait_queries, 0);
    }
}
