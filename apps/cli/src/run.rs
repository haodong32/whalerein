//! Command execution. All protocol behavior lives in `whalerein-core`.

use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use whalerein_core::api::{
    AttachmentLimits, AttachmentReference, Cancellation, ClientService, CreateSessionInput,
    DiagnosticContext, DownloadRequest, InputKind, ObservationInterests, PairingDecision,
    PendingQueueAction, SecretStore, SendFollowUp, SendReply, SessionEventPayload, SessionKey,
    SessionModel, WaitReason,
};
use whalerein_core::conversation::ViewUpdate;
use whalerein_core::history::{HistoryPageRequest, MessageReference};
use whalerein_core::{Core, CoreError};

use crate::cli::{Cli, Command, ModelAction, QueueCommand};
use crate::diagnostics::CliDiagnosticSink;
use crate::output::{
    approval_data, command_data, core_error_outcome, doctor_data, download_data,
    history_message_data, history_page_data, hosts_data, is_tail_output, lifecycle_error_code,
    lifecycle_exit_code, manifest_data, message_json, render_approval, render_approvals,
    render_command, render_doctor, render_download, render_history_message, render_history_page,
    render_host_status, render_hosts, render_lifecycle_outcome, render_manifest, render_models,
    render_pair, render_pairing_history, render_questions, render_queue, render_session_model,
    render_sessions, retain_visible_sessions, sanitize_field, sessions_data, shell_quote,
    tail_event_json, CliError, CliOutcome, DownloadProgressRenderer, TailRenderer,
};

const DEFAULT_TAIL_TIMEOUT: Duration = Duration::from_secs(15);

/// Default total `wait` deadline. It starts before key resolution so
/// authorized-Host discovery is charged to the same budget as the observation.
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest accepted deadline, matching the Host request budget.
const MAX_TIMEOUT_MS: u64 = 2_147_483_647;

/// Parse `--timeout` for both `wait` and `tail`: seconds, non-zero, with at
/// most millisecond precision. The one parser keeps the two commands from
/// drifting in units or bounds.
fn parse_timeout_seconds(value: &str) -> Result<Duration, &'static str> {
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
    if total == 0 || total > MAX_TIMEOUT_MS {
        return Err(invalid);
    }
    Ok(Duration::from_millis(total))
}

/// Parse the download deadline and derive bounded local limits.
///
/// `--timeout` is the overall transfer budget. The idle budget follows it down
/// so a short deadline never yields an invalid limit relation.
fn download_limits(value: Option<&str>) -> Result<AttachmentLimits, CoreError> {
    let mut limits = AttachmentLimits::default();
    if let Some(value) = value {
        let timeout = parse_timeout_seconds(value)
            .map_err(|message| CoreError::InvalidInput(format!("--timeout {message}")))?;
        limits.overall_timeout = timeout;
        if limits.idle_timeout > timeout {
            limits.idle_timeout = timeout;
        }
    }
    Ok(limits)
}

/// Build one download request from the exact history save action.
///
/// The positional reference is opaque and never carries authorization scope, so
/// the owning Host-qualified session travels alongside it. An empty reference is
/// rejected before any Core or network work.
fn download_request(
    target: &SessionKey,
    reference: &str,
    destination: std::path::PathBuf,
    force: bool,
    name: Option<&str>,
) -> Result<DownloadRequest, CoreError> {
    if reference.trim().is_empty() {
        return Err(CoreError::InvalidInput(
            "attachment reference is empty; use the save action shown by `session history`".into(),
        ));
    }
    let mut request = DownloadRequest::new(
        AttachmentReference::new(target.clone(), reference),
        destination,
    )
    .overwrite(force);
    if let Some(name) = name {
        request = request.file_name(name);
    }
    Ok(request)
}

async fn queue_change(
    core: &Core,
    command: &Command,
    key: &SessionKey,
    item: &str,
    action: PendingQueueAction,
    text: &str,
) -> Result<CliOutcome, CoreError> {
    let handle = core.update_pending_queue(key, item, action, text).await?;
    let refresh = core.pending_queue(key).await;
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

/// The total elapsed-time budget for commands that observe a session over
/// time. It is parsed before key resolution so Host discovery is charged to the
/// same deadline as the observation itself.
fn command_budget(command: &Command) -> Result<Option<Duration>, &'static str> {
    match command {
        Command::Send { timeout, .. } => Ok(Some(match timeout.as_deref() {
            Some(value) => parse_timeout_seconds(value)?,
            None => Duration::from_secs(120),
        })),
        Command::Wait { timeout, .. } => Ok(Some(match timeout.as_deref() {
            Some(value) => parse_timeout_seconds(value)?,
            None => DEFAULT_WAIT_TIMEOUT,
        })),
        Command::Tail { once, timeout, .. } => match timeout.as_deref() {
            Some(value) => Ok(Some(parse_timeout_seconds(value)?)),
            None if *once => Ok(Some(DEFAULT_TAIL_TIMEOUT)),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

fn log_target_resolution<T>(
    diagnostics: &CliDiagnosticSink,
    core: &Core,
    target: &'static str,
    result: &Result<T, CoreError>,
) {
    diagnostics.cli_event(core.diagnostic_id(), || {
        json!({
            "event": "target_resolved",
            "target": target,
            "outcome": if result.is_ok() { "succeeded" } else { "failed" },
            "code": result.as_ref().err().map(CoreError::code),
        })
    });
}

/// Borrow the key resolved by `run_with_signal`. Every session command reaches
/// dispatch only after resolution, so a missing key is an internal invariant
/// violation rather than a user-input problem.
fn resolved_key(key: Option<&SessionKey>) -> Result<&SessionKey, CoreError> {
    key.ok_or_else(|| CoreError::Internal("session key was not resolved before dispatch".into()))
}

/// What is left of a command's total deadline. `None` keeps an unbounded
/// command unbounded; an expired deadline is a timeout.
fn remaining_budget(deadline: Option<tokio::time::Instant>) -> Result<Option<Duration>, CoreError> {
    match deadline {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                Err(CoreError::Timeout)
            } else {
                Ok(Some(remaining))
            }
        }
        None => Ok(None),
    }
}

struct Execution {
    host: Option<String>,
    json: bool,
    yes: bool,
    show_reasoning: bool,
}

fn default_config_path() -> Result<std::path::PathBuf, CoreError> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.filter(|path| !path.is_empty())
        .map(|path| {
            std::path::PathBuf::from(path)
                .join(".whalerein")
                .join("config.toml")
        })
        .ok_or_else(|| {
            CoreError::Config("cannot resolve the home directory; pass --config FILE".into())
        })
}

pub async fn run(
    cli: Cli,
    diagnostics: &CliDiagnosticSink,
    context: DiagnosticContext,
) -> CliOutcome {
    let secrets: Arc<dyn SecretStore> =
        Arc::new(whalerein_core::api::KeyringSecretStore::new("whalerein"));
    #[cfg(unix)]
    let secrets: Arc<dyn SecretStore> = match &cli.secrets_dir {
        Some(path) => Arc::new(whalerein_core::api::FileSecretStore::new(path)),
        None => secrets,
    };
    run_with_signal(
        cli,
        secrets,
        crate::signal::registered_interrupt(),
        diagnostics,
        context,
    )
    .await
}

pub(crate) async fn run_with_signal<S>(
    mut parsed: Cli,
    secrets: Arc<dyn SecretStore>,
    signal: S,
    diagnostics: &CliDiagnosticSink,
    context: DiagnosticContext,
) -> CliOutcome
where
    S: std::future::Future<Output = ()>,
{
    let config = parsed.config.take();
    let json = parsed.json;
    let yes = parsed.yes;
    let show_reasoning = parsed.show_reasoning;
    crate::output::set_full_tool_output(parsed.full);
    let (mut command, host) = parsed.into_command();
    diagnostics.cli_event(context.trace_id(), || {
        json!({
            "event": "command_started",
            "command": command.name(),
            "configuration_source": if config.is_some() { "explicit" } else { "default" },
        })
    });
    if let Command::Send { operation_id, .. } = &mut command {
        operation_id.get_or_insert_with(Core::new_command_id);
    }
    let cli = Execution {
        host,
        json,
        yes,
        show_reasoning,
    };
    let config = match config.map_or_else(default_config_path, Ok) {
        Ok(path) => path,
        Err(error) => {
            diagnostics.cli_event(
                context.trace_id(),
                || json!({"event": "configuration_failed", "code": error.code()}),
            );
            return core_error_outcome(&command, error);
        }
    };
    let core = match Core::open_with_diagnostics(&config, secrets, context) {
        Ok(core) => core,
        Err(error) => return core_error_outcome(&command, error),
    };
    if let Command::Sessions { host, .. }
    | Command::Models { host }
    | Command::SessionCreate { host, .. } = &mut command
    {
        let resolved = core.resolve_operation_host(cli.host.as_deref()).await;
        log_target_resolution(diagnostics, &core, "host", &resolved);
        *host = match resolved {
            Ok(host) => host,
            Err(error) => return core_error_outcome(&command, error),
        };
    }
    // The total `wait`/`tail` deadline starts before session resolution, so a
    // slow authorized-Host discovery cannot extend the observation window.
    let started = tokio::time::Instant::now();
    let budget = match command_budget(&command) {
        Ok(budget) => budget,
        Err(error) => {
            return CliOutcome::local_failure(&command, "invalid_argument", error, 2);
        }
    };
    let deadline = budget.map(|budget| started + budget);

    // Resolve the Host-qualified key exactly once, before dispatch. The key
    // travels with the request; no ambient Host scope is installed.
    let key = if let Some(session) = command.session_id() {
        let resolve = async {
            core.operation_session_key(cli.host.as_deref(), session)
                .await
        };
        let resolved = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, resolve).await {
                Ok(resolved) => resolved,
                Err(_) => Err(CoreError::Timeout),
            },
            None => resolve.await,
        };
        log_target_resolution(diagnostics, &core, "session", &resolved);
        match resolved {
            Ok(key) => Some(key),
            Err(error) => return core_error_outcome(&command, error),
        }
    } else {
        None
    };
    let key = key.as_ref();

    // `answer` owns a richer interrupt path: it asks the Host to cancel the
    // open answer and only then reports unknown. Racing that against the
    // generic handler would exit before the cancellation request is sent.
    let may_submit = may_submit(&command);
    let before = if may_submit {
        pending_commands(&core)
    } else {
        Vec::new()
    };
    let mut signal = std::pin::pin!(signal);
    let cancellation = Cancellation::new();
    let result = if matches!(command, Command::Answer { .. }) {
        dispatch(&core, &cli, &command, key, deadline, &cancellation).await
    } else if matches!(command, Command::SessionDownload { .. }) {
        // A download owns a local cancellation token so Core drops its Host
        // transfer, releases reservations and removes staging before the
        // process reports cancellation. Dropping the future would skip those
        // bounded cleanup stages.
        let mut work = Box::pin(dispatch(
            &core,
            &cli,
            &command,
            key,
            deadline,
            &cancellation,
        ));
        let interrupted = tokio::select! {
            result = &mut work => Some(result),
            _ = &mut signal => None,
        };
        match interrupted {
            Some(result) => result,
            None => {
                cancellation.cancel();
                match tokio::time::timeout(Duration::from_secs(5), &mut work).await {
                    Ok(result) => result,
                    Err(_) => Ok(finish_core_result(
                        &core,
                        &command,
                        Err(CoreError::Cancelled),
                    )),
                }
            }
        }
    } else {
        // The dispatch future is boxed so the interrupt branch can drop it
        // *before* the cleanup window: dropping a `wait` future runs Core's
        // observation guard, which sends CancelSessionWait from a spawned task.
        let mut work = Box::pin(dispatch(
            &core,
            &cli,
            &command,
            key,
            deadline,
            &cancellation,
        ));
        tokio::select! {
            result = &mut work => result,
            _ = &mut signal => {
                if let Command::Send { operation_id: Some(id), .. } = &command {
                    drop(work);
                    return if matches!(core.send_operation(id), Err(CoreError::NotFound(_))) {
                        CliOutcome::local_failure(&command, "cancelled", "Cancelled before reserving the send operation.", 2)
                    } else {
                        uncertain_send(&core, &command, id, key)
                    };
                }
                if matches!(command, Command::Connect { .. }) {
                    drop(work);
                    if let Ok(Some((id, host))) = pending_pairing(&core, &command) {
                        return uncertain_pairing(&core, &command, &id, &host);
                    }
                    return CliOutcome::local_failure(&command, "cancelled", "Host connection cancelled.", 2);
                }
                let after = if may_submit {
                    pending_commands(&core)
                } else {
                    Vec::new()
                };
                let outcome = crate::signal::interruption_outcome(
                    &command,
                    &before,
                    &after,
                    core.diagnostic_id(),
                );
                if outcome.exit == 7 {
                    let _ = core.save_failure("unknown");
                }
                drop(work);
                if matches!(command, Command::Wait { .. }) {
                    // Give the best-effort CancelSessionWait a bounded window so
                    // a Ctrl-C never leaves a Host wait claim behind.
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                return outcome;
            }
        }
    };
    let mut outcome = finish_core_result(&core, &command, result);
    if let Some(key) = key {
        let data = outcome.data.get_or_insert_with(|| json!({}));
        if let Some(object) = data.as_object_mut() {
            object.entry("target").or_insert_with(|| json!(key));
        }
    }
    outcome
}

/// Commands that can create a journaled command identity. Only these need the
/// pre-dispatch pending snapshot; read-only commands must not open the cache
/// just to answer an interrupt (opening it changes revocation retention
/// reporting).
fn may_submit(command: &Command) -> bool {
    matches!(
        command,
        Command::Approve { .. }
            | Command::Reject { .. }
            | Command::Interrupt { .. }
            | Command::SessionCreate { .. }
            | Command::SessionFork { .. }
            | Command::QuestionCancel { .. }
            | Command::Answer { .. }
            | Command::Recover { .. }
            | Command::Model {
                action: ModelAction::Set { .. },
            }
            | Command::Queue {
                action: QueueCommand::Edit { .. }
                    | QueueCommand::Remove { .. }
                    | QueueCommand::Steer { .. },
            }
    )
}

/// Human warning for a doctor report whose local cache could not be opened.
///
/// The report itself stays usable (exit 0); commands that need the real cache
/// keep failing with `cache_error`.
fn render_cache_warning(warning: &str) -> String {
    format!(
        "Warning: the local cache is unavailable ({}). Unfinished-command counts may be incomplete; cache-backed commands still fail until the cache is rebuilt.\n",
        crate::output::sanitize_field(warning)
    )
}

/// Human outcome of a successful revocation.
///
/// The unverified branch is a warning with a concrete next step: review the
/// retained identities before pairing again and never reuse the old identity.
fn revoke_human(unverified: bool) -> String {
    let mut human = "Revoked this device's grants. Local identity was kept.\n".to_owned();
    if unverified {
        human.push_str(
            "Unable to verify whether unconfirmed command or pairing identities were retained.\nReview them with `whalerein operation list` before pairing again; do not reuse the old identity.\n",
        );
    }
    human
}

/// Human sentence for the child working directory of a fork.
///
/// A fork is complete once the child identity is confirmed; the directory is a
/// best-effort second read. When that read fails the fork stays successful and
/// the sentence says so instead of silently omitting the directory.
fn fork_directory_sentence(
    working_directory: Option<&str>,
    working_directory_error: Option<&str>,
) -> String {
    match (working_directory, working_directory_error) {
        (Some(directory), _) => format!(" Directory: {}.", crate::output::sanitize_field(directory)),
        (None, Some(error)) => format!(
            " The child working directory could not be read ({}); the child still exists and must not be retried or deleted.",
            crate::output::sanitize_field(error)
        ),
        (None, None) => String::new(),
    }
}

fn pending_pairing(core: &Core, command: &Command) -> Result<Option<(String, String)>, CoreError> {
    match core.client_config().service {
        ClientService::WhaleReinCloud => {
            let Command::Connect {
                host: Some(target), ..
            } = command
            else {
                return Ok(None);
            };
            Ok(core
                .pending_cloud_pairings()?
                .into_iter()
                .find(|(_, host)| host == target))
        }
        ClientService::Standalone => core.pending_standalone_pairing(),
    }
}

fn pending_commands(core: &Core) -> Vec<whalerein_core::api::PendingCommand> {
    core.pending_commands().unwrap_or_default()
}

fn finish_core_result(
    core: &Core,
    command: &Command,
    result: Result<CliOutcome, CoreError>,
) -> CliOutcome {
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            if matches!(command, Command::Connect { .. })
                && matches!(
                    error,
                    CoreError::Unavailable(_)
                        | CoreError::Timeout
                        | CoreError::UnknownResult
                        | CoreError::PairingCommittedUnconfirmed
                        | CoreError::PairingCommittedUnavailable
                )
            {
                if let Ok(Some((id, host))) = pending_pairing(core, command) {
                    return uncertain_pairing(core, command, &id, &host);
                }
            }
            let mut outcome = core_error_outcome(command, error);
            if let Some(error) = outcome.error.as_mut() {
                error.diagnostic_id = Some(core.diagnostic_id().to_owned());
                if !matches!(command, Command::AccountStatus | Command::Hosts) {
                    let _ = core.save_failure(&error.code);
                }
            }
            outcome
        }
    }
}

async fn dispatch(
    core: &Core,
    cli: &Execution,
    command: &Command,
    key: Option<&SessionKey>,
    deadline: Option<tokio::time::Instant>,
    cancellation: &Cancellation,
) -> Result<CliOutcome, CoreError> {
    let result = match command {
        Command::Login
        | Command::AccountStatus
        | Command::AccountSessions
        | Command::AccountRevokeAllSessions
        | Command::AccountRevokeSession { .. }
        | Command::Logout => crate::account::run(core, command).await,
        Command::DefaultHost { host } => {
            core.select_default_host(host).await?;
            Ok(CliOutcome::success(
                command,
                json!({"host_id": host}),
                format!("Default Host: {host}"),
            ))
        }
        Command::Connect {
            host: Some(host), ..
        } => {
            let outcome = core.connect_cloud(host).await?;
            Ok(CliOutcome::success(
                command,
                json!({"host_id": host, "connection": outcome}),
                format!("Connected Host {host}"),
            ))
        }
        Command::Connect {
            invite_stdin: true,
            fingerprint,
            name,
            ..
        } => {
            if core.client_config().service != ClientService::Standalone {
                return Err(CoreError::InvalidInput(
                    "Use Standalone configuration for an invitation".into(),
                ));
            }
            pair(
                core,
                command,
                cli,
                "-",
                fingerprint.as_deref(),
                name.as_deref(),
            )
            .await
        }
        Command::Connect { .. } => Err(CoreError::InvalidInput(
            "Specify a Host ID or --invite-stdin".into(),
        )),
        Command::Hosts if core.client_config().service == ClientService::WhaleReinCloud => {
            crate::account::run(core, command).await
        }
        Command::HostStatus { host } => {
            let Some(host_id) = host.clone().or_else(|| core.default_host().ok().flatten()) else {
                return Ok(CliOutcome::local_failure(
                    command,
                    "invalid_argument",
                    "Select a Host with --host or `whalerein host default`.",
                    2,
                ));
            };
            // The probe is the point: a reported state always means "right
            // now", never a cached value from an earlier session.
            let snapshot = match core.connect_host(&host_id).await {
                Ok(snapshot) => snapshot,
                // A terminal refusal still has a specific state to report; an
                // unknown Host keeps the original error.
                Err(error) => match core.connection_snapshot(&host_id) {
                    Some(snapshot) => snapshot,
                    None => return Err(error),
                },
            };
            Ok(CliOutcome::success(
                command,
                json!({
                    "host_id": snapshot.host_id,
                    "connection": {
                        "state": snapshot.state.as_str(),
                        "code": snapshot.code,
                        "since_unix_ms": snapshot.since_unix_ms,
                    }
                }),
                render_host_status(&snapshot),
            ))
        }
        Command::Hosts => match core.inspect_regional_hosts().await {
            Ok(hosts) => Ok(CliOutcome::success(
                command,
                hosts_data(&hosts),
                render_hosts(&hosts),
            )),
            Err(error) => Err(error),
        },
        Command::Sessions { host, all } => match core.sessions(host).await {
            Ok(mut list) => {
                retain_visible_sessions(&mut list, *all);
                Ok(CliOutcome::success(
                    command,
                    sessions_data(host, &list),
                    render_sessions(&list),
                ))
            }
            Err(error) => Err(error),
        },
        Command::Models { host } => match core.list_models(host).await {
            Ok(catalog) => Ok(CliOutcome::success(
                command,
                json!(catalog),
                render_models(&catalog),
            )),
            Err(error) => Err(error),
        },
        Command::Model {
            action: ModelAction::Get { .. },
        } => {
            let key = resolved_key(key)?;
            match core.read_session_model(key).await {
                Ok(selection) => Ok(CliOutcome::success(
                    command,
                    json!(selection),
                    render_session_model(&selection),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Model {
            action:
                ModelAction::Set {
                    session,
                    provider,
                    model,
                    effort,
                    ..
                },
        } => {
            let key = resolved_key(key)?;
            // Re-selecting the effective next model is a no-op. It must not
            // journal a second indistinguishable pending command (QMQ-10).
            if let Ok(current) = core.read_session_model(key).await {
                if model_selection_matches(&current, provider, model, effort.as_deref()) {
                    return Ok(CliOutcome::success(
                        command,
                        json!({"session": session, "selection": current.next, "noop": true}),
                        "Already selected; no command was sent.\n",
                    ));
                }
            }
            match core
                .select_session_model(key, provider, model, effort.as_deref())
                .await
            {
                Ok(handle) => Ok(CliOutcome::success(
                    command,
                    command_data(&handle),
                    render_command(&handle),
                )),
                Err(error) => Err(error),
            }
        }
        Command::SessionFork { session } => match core
            .fork_session_outcome(resolved_key(key)?)
            .await
        {
            Ok(outcome) => {
                let handle = outcome.handle;
                let created = handle.lifecycle_receipt.as_ref().is_some_and(|receipt| {
                    !receipt.session_id.is_empty()
                        && matches!(receipt.disposition.as_str(), "READY" | "READY_WITH_WARNING")
                });
                let human = match &handle.lifecycle_receipt {
                        Some(receipt) if created => format!(
                            "Session {} forked from {}. The child shares the working directory; no worktree is created.{}{}\n",
                            crate::output::sanitize_field(&receipt.session_id),
                            crate::output::sanitize_field(session),
                            fork_directory_sentence(
                                outcome.working_directory.as_deref(),
                                outcome.working_directory_error.as_deref(),
                            ),
                            if receipt.warning_code.is_some() {
                                " Workspace attachment is incomplete; the child exists and must not be retried or deleted."
                            } else {
                                ""
                            }
                        ),
                        _ => render_lifecycle_outcome(&handle)
                            .unwrap_or_else(|| render_command(&handle)),
                    };
                let mut data = command_data(&handle);
                if let Some(cwd) = &outcome.working_directory {
                    data["working_directory"] = json!(cwd);
                }
                if let Some(error) = &outcome.working_directory_error {
                    data["working_directory_error"] = json!(error);
                }
                let mut outcome = CliOutcome::lifecycle(
                    command,
                    &handle,
                    lifecycle_error_code(&handle),
                    human,
                    lifecycle_exit_code(&handle),
                );
                // `lifecycle()` rebuilds the data payload from the handle, so
                // the fork-only directory fields are installed afterwards.
                outcome.data = Some(data);
                Ok(outcome)
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
                        command,
                        &handle,
                        lifecycle_error_code(&handle),
                        human,
                        lifecycle_exit_code(&handle),
                    ))
                }
                Err(error) => Err(error),
            }
        }
        Command::CommandsPending => {
            let sends = core.send_operations()?;
            let commands = if core.is_paired() {
                core.pending_commands()?
            } else {
                Vec::new()
            };
            let pairings = match core.client_config().service {
                ClientService::WhaleReinCloud => core.pending_cloud_pairings()?,
                ClientService::Standalone => {
                    core.pending_standalone_pairing()?.into_iter().collect()
                }
            };
            let mut human = String::new();
            for send in &sends {
                human.push_str(&format!(
                    "{}  {}  {}\n",
                    send.operation_id, send.target.host_id, send.target.session_id
                ));
            }
            for entry in &commands {
                if sends
                    .iter()
                    .all(|send| send.operation_id != entry.request_id)
                {
                    human.push_str(&format!(
                        "{}  {}  {}\n",
                        entry.request_id,
                        entry.host_id,
                        entry.status.as_str()
                    ));
                }
            }
            for (id, host) in &pairings {
                human.push_str(&format!("{id}  {host}  pairing\n"));
            }
            let pairing_history = if core.data_dir().is_dir() {
                core.pairing_history()?
            } else {
                Vec::new()
            };
            if !pairing_history.is_empty() {
                human.push_str(&render_pairing_history(&pairing_history));
            }
            if human.is_empty() {
                human.push_str("No retained operations.\n");
            }
            Ok(CliOutcome::success(
                command,
                json!({"sends": sends, "pending_commands": commands, "pending_pairing": pairings.first(), "pending_pairings": pairings, "pairing_history": pairing_history}),
                human,
            ))
        }
        Command::Recover { request } => {
            if core.has_cloud_pairing_operation(request)? {
                let outcome = core.recover_cloud_pairing(request).await?;
                let human = render_pair(&outcome);
                return Ok(CliOutcome::success(
                    command,
                    json!({"operation_id": request, "connection": outcome}),
                    human,
                ));
            }
            if core.has_standalone_pairing_operation(request)? {
                let outcome = core.recover_standalone_pairing(request).await?;
                let human = render_pair(&outcome);
                return Ok(CliOutcome::success(
                    command,
                    json!({"operation_id": request, "connection": outcome}),
                    human,
                ));
            }
            match core.recover_send_operation(request).await {
                Ok(operation) => {
                    render_send(
                        core,
                        command,
                        operation,
                        false,
                        Some(Duration::from_secs(120)),
                        cli.show_reasoning,
                    )
                    .await
                }
                Err(CoreError::NotFound(_)) => {
                    let handle = core.recover_pending_command(request).await?;
                    Ok(CliOutcome::success(
                        command,
                        command_data(&handle),
                        render_command(&handle),
                    ))
                }
                Err(CoreError::UnknownResult | CoreError::Timeout | CoreError::Unavailable(_)) => {
                    Ok(uncertain_send(core, command, request, None))
                }
                Err(error) => Err(error),
            }
        }
        Command::Tail { once, .. } => {
            let key = resolved_key(key)?;
            // Resolution already ran inside the same budget, so tail gets only
            // what is left of the original deadline.
            let timeout = remaining_budget(deadline)?;
            tail(
                core,
                command,
                cli.json,
                cli.show_reasoning,
                key,
                *once,
                timeout,
            )
            .await
        }
        Command::Wait {
            fail_on_timeout, ..
        } => {
            let key = resolved_key(key)?;
            let timeout_ms = remaining_budget(deadline)?
                .map(|remaining| remaining.as_millis() as u64)
                .unwrap_or(DEFAULT_WAIT_TIMEOUT.as_millis() as u64);
            if timeout_ms == 0 {
                return Err(CoreError::Timeout);
            }
            match core.wait_session(key, timeout_ms).await {
                Ok(observation) => {
                    let mut outcome = CliOutcome::success(
                        command,
                        serde_json::to_value(&observation).unwrap_or_else(|_| json!({})),
                        format!(
                            "Session {}: {:?}\n",
                            crate::output::sanitize_field(&observation.session_id),
                            observation.reason
                        ),
                    );
                    if *fail_on_timeout && observation.reason == WaitReason::Timeout {
                        // A timeout observation that is configured to fail is
                        // an error envelope with data, matching `tail` instead
                        // of a bare non-zero exit with `error: null`.
                        outcome.exit = 7;
                        outcome.error = Some(crate::output::CliError::new(
                            "timeout",
                            "wait deadline expired before the session settled",
                        ));
                    }
                    Ok(outcome)
                }
                Err(error) => Err(error),
            }
        }
        Command::Sync { .. } => {
            let target = resolved_key(key)?;
            let summary = core.get_session(target).await?;
            Ok(CliOutcome::success(
                command,
                json!({ "target": target, "session": summary }),
                format!(
                    "{}  {}\n",
                    target.session_id,
                    sanitize_field(&summary.title)
                ),
            ))
        }
        Command::History {
            cache,
            continuation,
            message,
            max_messages,
            max_bytes,
            ..
        } => {
            let target = resolved_key(key)?;
            if let Some(reference) = message {
                let reference: MessageReference = reference.parse()?;
                if reference.session() != target {
                    return Err(CoreError::InvalidInput(
                        "message reference belongs to another session; use the session that owns it".into(),
                    ));
                }
                let message = core.history_message(&reference).await?;
                Ok(CliOutcome::success(
                    command,
                    history_message_data(target, &reference, &message, cli.show_reasoning),
                    render_history_message(target, &reference, &message, cli.show_reasoning),
                ))
            } else {
                let mut request = HistoryPageRequest::new(target.clone());
                if *cache {
                    request = request.cache();
                }
                if let Some(value) = continuation {
                    request.continuation = Some(value.parse()?);
                }
                if let Some(value) = max_messages {
                    if *value == 0 {
                        return Err(CoreError::InvalidInput(
                            "--max-messages must be at least 1".into(),
                        ));
                    }
                    request.max_messages = *value;
                }
                if let Some(value) = max_bytes {
                    if *value == 0 {
                        return Err(CoreError::InvalidInput(
                            "--max-bytes must be at least 1".into(),
                        ));
                    }
                    request.max_bytes = *value;
                }
                let page = core.history_page(&request).await?;
                Ok(CliOutcome::success(
                    command,
                    history_page_data(target, &page, cli.show_reasoning),
                    render_history_page(target, &page, cli.show_reasoning),
                ))
            }
        }
        Command::SessionDownload {
            reference,
            to,
            force,
            name,
            timeout,
            session: _,
        } => {
            let target = key.ok_or_else(|| {
                CoreError::InvalidInput(
                    "session download requires the --session scope shown by the history save action"
                        .into(),
                )
            })?;
            let limits = download_limits(timeout.as_deref())?;
            let request = download_request(target, reference, to.clone(), *force, name.as_deref())?;
            let mut progress = DownloadProgressRenderer::new(cli.json);
            let result = core
                .download_attachment_with_limits(request, limits, cancellation, |update| {
                    if let Some(line) = progress.report(update) {
                        let _ = crate::stream::to_stderr(line.as_bytes());
                    }
                })
                .await?;
            Ok(CliOutcome::success(
                command,
                download_data(target, reference, &result),
                render_download(&result),
            ))
        }
        Command::Send {
            text,
            delivery,
            detach,
            operation_id,
            ..
        } => {
            let target = resolved_key(key)?;
            let id = operation_id
                .as_deref()
                .ok_or_else(|| CoreError::Internal("operation ID not prepared".into()))?;
            let work = core.submit_message_operation(id, target, text, (*delivery).into());
            let result = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, work)
                    .await
                    .unwrap_or(Err(CoreError::Timeout)),
                None => work.await,
            };
            match result {
                Ok(operation) => {
                    render_send(
                        core,
                        command,
                        operation,
                        *detach,
                        remaining_budget(deadline).ok().flatten(),
                        cli.show_reasoning,
                    )
                    .await
                }
                Err(CoreError::UnknownResult | CoreError::Timeout | CoreError::Unavailable(_)) => {
                    Ok(uncertain_send(core, command, id, Some(target)))
                }
                Err(error) => Err(error),
            }
        }
        Command::Approvals { session, request } => {
            if let Some(approval_id) = request {
                // APPR-03 read path: one approval's live or terminal state.
                // A known id is success; an unknown id stays not_found (exit 3).
                let host = core.resolve_operation_host(cli.host.as_deref()).await?;
                return match core.get_approval(&host, approval_id).await {
                    Ok(approval) => Ok(CliOutcome::success(
                        command,
                        approval_data(&approval),
                        render_approval(&approval),
                    )),
                    Err(error) => Err(error),
                };
            }
            // With a session the resolved key names the Host. Without a
            // session, only an explicit --host can scope the empty-session
            // listing; the session-less, Host-less shape stays the same input
            // error it always was instead of listing every Host's approvals.
            let (host, session_id) = match session.as_deref() {
                Some(session) => {
                    let key = resolved_key(key)?;
                    (key.host_id.clone(), session.to_owned())
                }
                None => match cli.host.as_deref() {
                    Some(_) => (
                        core.resolve_operation_host(cli.host.as_deref()).await?,
                        String::new(),
                    ),
                    None => return Err(CoreError::InvalidInput("session identity missing".into())),
                },
            };
            match core.list_approvals(&host, &session_id).await {
                Ok(approvals) => Ok(CliOutcome::success(
                    command,
                    json!({ "session_id": session_id, "approvals": approvals }),
                    render_approvals(&approvals),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Questions { cursor, .. } => {
            let key = resolved_key(key)?;
            // One-shot list on the shared Core question reader; observation and
            // listing never acquire a native question claim.
            match core.read_questions(key, cursor).await {
                Ok(page) => Ok(CliOutcome::success(
                    command,
                    json!(page),
                    render_questions(&page),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Answer {
            batch,
            epoch,
            version,
            digest,
            phase,
            input,
            command_id,
            ..
        } => {
            let digest = match parse_digest(digest) {
                Ok(digest) => digest,
                Err(error) => {
                    return Ok(CliOutcome::local_failure(
                        command,
                        error.code.as_str(),
                        error.message,
                        2,
                    ))
                }
            };
            let answers = match read_question_answers(input) {
                Ok(answers) => answers,
                Err(error) => {
                    return Ok(CliOutcome::local_failure(
                        command,
                        error.code.as_str(),
                        error.message,
                        2,
                    ))
                }
            };
            let key = resolved_key(key)?;
            match core.read_question(key, batch).await {
                Ok(current) => {
                    // An unknown or unanswerable phase is an input problem, not
                    // evidence that the source data went stale.
                    validate_requested_question_phase(phase)?;
                    validate_source_question_phase(&current.phase)?;
                    if current.source_epoch != *epoch
                        || current.version != *version
                        || current.digest != digest
                        || current.phase != *phase
                    {
                        return Err(CoreError::Conflict(
                            "question changed; refresh before answering".into(),
                        ));
                    }
                    let actual_id = command_id.clone().unwrap_or_else(Core::new_command_id);
                    let answer =
                        core.answer_question_with_id(key, &current, answers, Some(&actual_id));
                    let (outcome, interrupted) = if current.phase == "QUESTION_OPEN" {
                        match wait_open_answer(answer, crate::signal::interrupt(), || core.cancel_question_answer(key, &actual_id),
                            Duration::from_millis(whalerein_core::api::CONTROL_REQUEST_TTL_MS), Duration::from_millis(250)).await {
                            OpenAnswerWait::Finished(result, interrupted) => (*result, interrupted),
                            OpenAnswerWait::CancellationRequested => {
                                return Ok(unknown_outcome(core, command, format!("Cancellation requested for command {actual_id}; recover its result with `whalerein operation list` and `whalerein operation recover <request-id>`.")))
                            }
                            OpenAnswerWait::CancellationUnconfirmed => {
                                return Ok(unknown_outcome(core, command, format!("Cancellation was not confirmed for command {actual_id}; recover its result with `whalerein operation list` and `whalerein operation recover <request-id>`.")))
                            }
                        }
                    } else {
                        (answer.await, false)
                    };
                    match outcome {
                        Ok(handle) => Ok(CliOutcome::success(
                            command,
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
                            return Ok(unknown_outcome(core, command, format!("Outcome of command {actual_id} is unconfirmed; recover its result with `whalerein operation list` and `whalerein operation recover <request-id>`.")));
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) if error.code() == "not_found" => Err(CoreError::Conflict(
                    "question unavailable; refresh before answering".into(),
                )),
                Err(error) => Err(error),
            }
        }
        Command::QuestionCancel {
            command: command_id,
            ..
        } => match core
            .cancel_question_answer(resolved_key(key)?, command_id)
            .await
        {
            Ok(requested) => Ok(CliOutcome::success(
                command,
                json!({"cancellation_requested": requested}),
                if requested {
                    "Cancellation requested; recover the command result to confirm its outcome.\n"
                        .to_owned()
                } else {
                    question_cancel_guidance(core, command_id)
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
                    return Ok(CliOutcome::local_failure(
                        command,
                        error.code.as_str(),
                        error.message,
                        2,
                    ))
                }
            };
            let host = core.resolve_operation_host(cli.host.as_deref()).await?;
            match core.approve(&host, request, &digest, *version).await {
                Ok(handle) => Ok(CliOutcome::success(
                    command,
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
                    return Ok(CliOutcome::local_failure(
                        command,
                        error.code.as_str(),
                        error.message,
                        2,
                    ))
                }
            };
            let host = core.resolve_operation_host(cli.host.as_deref()).await?;
            match core.reject(&host, request, &digest, *version).await {
                Ok(handle) => Ok(CliOutcome::success(
                    command,
                    command_data(&handle),
                    render_command(&handle),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Queue { action } => match action {
            QueueCommand::List { .. } => {
                let key = resolved_key(key)?;
                match core.pending_queue(key).await {
                    Ok(queue) => {
                        let human = render_queue(&queue);
                        let data = serde_json::to_value(&queue).unwrap_or_else(|_| json!({}));
                        Ok(CliOutcome::success(command, data, human))
                    }
                    Err(error) => Err(error),
                }
            }
            QueueCommand::Edit { item, text, .. } => {
                queue_change(
                    core,
                    command,
                    resolved_key(key)?,
                    item,
                    PendingQueueAction::Edit,
                    text,
                )
                .await
            }
            QueueCommand::Remove { item, .. } => {
                queue_change(
                    core,
                    command,
                    resolved_key(key)?,
                    item,
                    PendingQueueAction::Remove,
                    "",
                )
                .await
            }
            QueueCommand::Steer { item, .. } => {
                queue_change(
                    core,
                    command,
                    resolved_key(key)?,
                    item,
                    PendingQueueAction::Promote,
                    "",
                )
                .await
            }
        },
        Command::Interrupt { .. } => match core.interrupt(resolved_key(key)?).await {
            Ok(handle) => Ok(CliOutcome::success(
                command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(error) => Err(error),
        },
        Command::Revoke if core.client_config().service == ClientService::WhaleReinCloud => {
            if let Err(outcome) = require_destructive_confirmation(
                command,
                cli,
                "Revoke this Device's Cloud Host grant?",
            ) {
                return Ok(outcome);
            }
            let host = core.resolve_operation_host(cli.host.as_deref()).await?;
            let result = core.revoke_cloud(&host).await?;
            let text = format!("Device revocation for Host {host}.");
            Ok(CliOutcome::success(
                command,
                json!({"host_id": host, "revocation": result}),
                text,
            ))
        }
        Command::Revoke => match require_destructive_confirmation(
            command,
            cli,
            "Revoke this device's grants and clear local authorization material?",
        ) {
            Err(outcome) => Ok(outcome),
            Ok(()) => match core.revoke().await {
                Ok(outcome) => {
                    let unverified = matches!(
                        outcome.retention,
                        whalerein_core::api::RevocationRetention::Unverified
                    );
                    let human = revoke_human(unverified);
                    Ok(CliOutcome::success(
                        command,
                        json!({ "device_id":outcome.device_id, "revoked":true, "revoked_retention_unverified":unverified }),
                        human,
                    ))
                }
                Err(error) => Err(error),
            },
        },
        Command::Forget { local_only } => {
            if let Err(outcome) =
                require_destructive_confirmation(command, cli, "Forget this local device identity?")
            {
                return Ok(outcome);
            }
            if !local_only && core.is_paired() {
                let _ = core.hosts().await;
            }
            let forgotten = if *local_only {
                core.forget_local_device()
            } else {
                core.forget_device()
            };
            forgotten.map(|()| {
                CliOutcome::success(
                    command,
                    json!({
                        "identity_forgotten": true,
                        "local_only": local_only,
                        "remote_grant_may_remain": local_only,
                    }),
                    if *local_only {
                        "Forgot local identity. The remote grant may still be active; revoke the old device in host settings before reusing that identity.\n"
                    } else {
                        "Forgot this local device identity.\n"
                    },
                )
            })
        }
        Command::Doctor {
            export,
            clear_diagnostics,
        } => {
            if *clear_diagnostics {
                core.clear_diagnostics().map(|()| {
                    CliOutcome::success(
                        command,
                        json!({"diagnostics_cleared": true}),
                        "Cleared local diagnostic history.\n",
                    )
                })
            } else {
                doctor(core, command, cli, export.as_deref())
            }
        }
    };
    result
}

fn uncertain_pairing(core: &Core, command: &Command, id: &str, host: &str) -> CliOutcome {
    let mut outcome = CliOutcome::local_failure_with_diagnostic(
        command,
        "unknown",
        format!(
            "Connection to Host {} is uncertain. Run `whalerein operation recover {id}`.",
            sanitize_field(host)
        ),
        7,
        core.diagnostic_id(),
    );
    outcome.data = Some(json!({"operation_id": id, "host_id": host, "state": "uncertain"}));
    outcome
}

fn uncertain_send(
    core: &Core,
    command: &Command,
    id: &str,
    target: Option<&SessionKey>,
) -> CliOutcome {
    let mut outcome = CliOutcome::local_failure_with_diagnostic(
        command,
        "unknown",
        format!("Operation {id} is uncertain. Run `whalerein operation recover {id}`."),
        7,
        core.diagnostic_id(),
    );
    outcome.data = Some(json!({"operation_id": id, "target": target, "state": "uncertain"}));
    outcome
}

/// Wire-ready next-step commands for the evidenced pending-input kinds.
///
/// Only the kinds the Source proved are shown. Approval and question commands
/// are scoped to the original Host and session and safely quoted.
fn send_input_commands(target: &SessionKey, kinds: &[InputKind]) -> Vec<String> {
    let host = shell_quote(&target.host_id);
    let session = shell_quote(&target.session_id);
    let mut commands = Vec::new();
    for kind in kinds {
        match kind {
            InputKind::Approval => commands.push(format!(
                "whalerein session approval list --session {session} --host {host}"
            )),
            InputKind::Question => commands.push(format!(
                "whalerein session question list --session {session} --host {host}"
            )),
            InputKind::Other => {}
        }
    }
    commands
}

fn send_recovery_command(target: &SessionKey, reference: &str) -> String {
    format!(
        "whalerein session history {} --host {} --message {}",
        shell_quote(&target.session_id),
        shell_quote(&target.host_id),
        shell_quote(reference)
    )
}

fn send_followup_data(
    operation: &whalerein_core::api::SendOperation,
    follow: &SendFollowUp,
    show_reasoning: bool,
) -> Value {
    let mut data = json!({
        "operation_id": &operation.operation_id,
        "target": &operation.target,
        "reply": &follow.reply,
    });
    match &follow.reply {
        SendReply::Committed { reference } => {
            data["reference"] = json!(reference.to_token());
        }
        SendReply::InputRequired { kinds, .. } => {
            // Keep the typed kinds at the top level for scripts while the
            // canonical reply object keeps its own tag and fields.
            data["input_kinds"] = json!(kinds);
        }
        SendReply::Pending | SendReply::Uncertain => {}
    }
    if let Some(message) = &follow.message {
        data["message"] = message_json(message, show_reasoning);
    }
    data["read_error"] = match follow.read_error_code() {
        Some(code) => json!(code),
        None => Value::Null,
    };
    data
}

fn send_followup_human(
    operation: &whalerein_core::api::SendOperation,
    follow: &SendFollowUp,
    show_reasoning: bool,
) -> String {
    let target = &operation.target;
    let header = format!(
        "Host {} · Session {}\n",
        sanitize_field(&target.host_id),
        sanitize_field(&target.session_id)
    );
    match &follow.reply {
        SendReply::Committed { reference } => {
            let mut out = format!(
                "{}Confirmed committed message {} (operation {}).\n",
                header,
                sanitize_field(reference.message_id()),
                sanitize_field(&operation.operation_id)
            );
            if let Some(message) = &follow.message {
                let body = crate::output::render_committed_blocks(message, show_reasoning);
                if !body.is_empty() {
                    out.push_str(&format!("[{}] {body}\n", message.role.as_str_name()));
                }
                return out;
            }
            let recovery = send_recovery_command(target, &reference.to_token());
            let code = follow.read_error_code().unwrap_or("unavailable");
            if code == "snapshot_required" {
                out.push_str(&format!(
                    "The confirmed message reference is stale ({code}), so its content could not be read. Refresh history and retry the exact read:\n  {recovery}\nDo not resend the message; the submission is already committed.\n"
                ));
            } else {
                out.push_str(&format!(
                    "The reply content could not be read ({code}). The confirmation above still holds; retry the exact read:\n  {recovery}\nDo not resend the message because content was unavailable.\n"
                ));
            }
            out
        }
        SendReply::InputRequired { kinds, session, .. } => {
            let mut out = format!(
                "{}The submitted message is confirmed and requires input (operation {}).\n",
                header,
                sanitize_field(&operation.operation_id)
            );
            let commands = send_input_commands(session, kinds);
            if commands.is_empty() {
                out.push_str(
                    "Review the session's pending input, then continue in the source client. Do not resubmit the message.\n",
                );
            } else {
                out.push_str("Inspect the pending input:\n");
                for command in commands {
                    out.push_str(&format!("  {command}\n"));
                }
                out.push_str("Do not resubmit the message; answer the pending request instead.\n");
            }
            out
        }
        SendReply::Pending => format!(
            "{}Submission {} is confirmed, but the reply is not settled yet. Do not resend it.\nInspect progress with `whalerein session watch {} --host {}` or recover the original operation with `whalerein operation recover {}`.\n",
            header,
            sanitize_field(&operation.operation_id),
            shell_quote(&target.session_id),
            shell_quote(&target.host_id),
            sanitize_field(&operation.operation_id)
        ),
        SendReply::Uncertain => String::new(),
    }
}

async fn render_send(
    core: &Core,
    command: &Command,
    operation: whalerein_core::api::SendOperation,
    detach: bool,
    budget: Option<Duration>,
    show_reasoning: bool,
) -> Result<CliOutcome, CoreError> {
    let mut data = json!({
        "operation_id": &operation.operation_id,
        "target": &operation.target,
        "receipt": &operation.receipt,
    });
    if detach {
        data["state"] = json!("submission");
        let human = operation
            .receipt
            .as_ref()
            .map_or_else(String::new, render_command);
        return Ok(CliOutcome::success(command, data, human));
    }
    let follow = core
        .follow_send_operation_with_content(operation.clone(), budget.unwrap_or_default())
        .await?;
    if matches!(follow.reply, SendReply::Uncertain) {
        return Ok(uncertain_send(
            core,
            command,
            &operation.operation_id,
            Some(&operation.target),
        ));
    }
    // Following the reply can recover and persist a newer delivery receipt.
    data["receipt"] = json!(core.send_operation(&operation.operation_id)?.receipt);
    let follow_data = send_followup_data(&operation, &follow, show_reasoning);
    for (key, value) in follow_data.as_object().into_iter().flatten() {
        data[key] = value.clone();
    }
    let human = send_followup_human(&operation, &follow, show_reasoning);
    Ok(CliOutcome::success(command, data, human))
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
    cli: &Execution,
    uri: &str,
    fingerprint: Option<&str>,
    name: Option<&str>,
) -> Result<CliOutcome, CoreError> {
    // `-` reads the invitation from standard input so the one-time token never
    // appears in argv, `ps` output or shell history. Surrounding whitespace is
    // removed; the Core parser validates and bounds the value.
    let uri_owned;
    let uri = if uri == "-" {
        let mut text = String::new();
        std::io::stdin()
            .take(16_385)
            .read_to_string(&mut text)
            .map_err(|_| {
                CoreError::InvalidInput("pairing URI could not be read from standard input".into())
            })?;
        if text.len() > 16_384 {
            return Err(CoreError::InvalidInput(
                "invitation exceeds the input limit".into(),
            ));
        }
        uri_owned = text.trim().to_owned();
        if uri_owned.is_empty() {
            return Ok(CliOutcome::local_failure(
                command,
                "invalid_argument",
                "standard input did not contain a pairing URI",
                2,
            ));
        }
        uri_owned.as_str()
    } else {
        uri
    };
    // Reject a malformed pin locally: sending it to the Host would surface as a
    // fingerprint mismatch (or a transport error) and mislead the MITM check.
    let fingerprint = match fingerprint {
        Some(value) => match parse_fingerprint(value) {
            Ok(fingerprint) => Some(fingerprint),
            Err(error) => {
                return Ok(CliOutcome::local_failure(
                    command,
                    error.code.as_str(),
                    error.message,
                    2,
                ))
            }
        },
        None => None,
    };
    let interactive = std::io::stdin().is_terminal();
    if !cli.yes && fingerprint.is_none() && !interactive {
        return Ok(CliOutcome::local_failure(
            command,
            "invalid_argument",
            "non-interactive pairing requires --yes or --fingerprint",
            2,
        ));
    }
    let expected = fingerprint;
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
    match core
        .connect_standalone_as(uri, decision, &display_name(name))
        .await
    {
        Ok(outcome) => Ok(CliOutcome::success(
            command,
            serde_json::to_value(&outcome).unwrap_or_else(|_| json!({})),
            render_pair(&outcome),
        )),
        Err(error) => Err(error),
    }
}

/// Render one typed observation event for `tail`.
///
/// JSON keeps the existing `whalerein/client/v1` envelope and adds the resolved
/// session to the typed event. The human renderer owns terminal boundaries
/// only; Core owns preview ordering and completion.
fn render_tail_event(
    json_output: bool,
    renderer: &mut TailRenderer,
    key: &SessionKey,
    update: &ViewUpdate,
    show_reasoning: bool,
) -> String {
    if json_output {
        let Some(mut value) = tail_event_json(update, show_reasoning) else {
            // Hidden reasoning never enters default JSON.
            return String::new();
        };
        value["data"]["event"]["session"] =
            json!({"host_id": key.host_id, "session_id": key.session_id});
        format!("{}\n", serde_json::to_string(&value).unwrap_or_default())
    } else {
        renderer.render(update)
    }
}

async fn tail(
    core: &Core,
    command: &Command,
    json_output: bool,
    show_reasoning: bool,
    key: &SessionKey,
    once: bool,
    timeout: Option<Duration>,
) -> Result<CliOutcome, CoreError> {
    let session = key.session_id.as_str();
    let mut renderer = TailRenderer::new(show_reasoning);
    let mut visible = false;
    let mut closed = false;
    let mut stopped = false;
    let mut emitted = 0u64;
    let mut observation = core
        .observe_session(
            key,
            ObservationInterests::conversation(),
            Cancellation::new(),
        )
        .await?;
    let operation = async {
        while let Some(event) = observation.next_event().await {
            let SessionEventPayload::Conversation(update) = event.payload else {
                continue;
            };
            if matches!(
                update,
                ViewUpdate::Preview { .. }
                    | ViewUpdate::Message { .. }
                    | ViewUpdate::AttemptEnded { .. }
            ) {
                emitted += 1;
            }
            let text = render_tail_event(json_output, &mut renderer, key, &update, show_reasoning);
            visible |= is_tail_output(&update, show_reasoning);
            match crate::stream::to_stdout(text.as_bytes()) {
                Ok(()) => {
                    if once && visible {
                        stopped = true;
                        observation.close().await;
                        break;
                    }
                }
                Err(failure) if failure.is_closed() => {
                    // The consumer closed the pipe (`tail | head -1`): stop the
                    // stream cleanly instead of panicking on the next write.
                    closed = true;
                    stopped = true;
                    observation.close().await;
                    break;
                }
                Err(_) => {
                    stopped = true;
                    observation.close().await;
                    break;
                }
            }
        }
    };
    let finished = match timeout {
        Some(limit) => tokio::time::timeout(limit, operation).await.is_ok(),
        None => {
            operation.await;
            true
        }
    };
    if !finished {
        observation.close().await;
    }
    if closed {
        return Ok(CliOutcome::success(
            command,
            json!({ "session_id": session, "events": 0, "closed": true }),
            String::new(),
        ));
    }
    if !json_output {
        let tail = renderer.finish_line();
        let _ = crate::stream::to_stdout(tail.as_bytes());
    }
    if !finished && emitted == 0 {
        return Err(observation
            .take_terminal_error()
            .or_else(|| observation.take_last_error())
            .unwrap_or(CoreError::Timeout));
    }
    if finished && !stopped {
        // Core owns reconnect. An observation that ends before the caller's
        // deadline or a consumer stop is terminal; surface its specific fatal
        // cause (revoked, not paired, unsupported, ...) rather than a generic
        // unavailable result.
        if let Some(error) = observation.take_terminal_error() {
            return Err(error);
        }
        return Err(CoreError::Unavailable("session observation ended".into()));
    }
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
    cli: &Execution,
    export: Option<&std::path::Path>,
) -> Result<CliOutcome, CoreError> {
    match export {
        Some(path) => {
            // A directory is a usage error, not an internal write failure.
            if path.exists() && path.is_dir() {
                return Ok(CliOutcome::local_failure(
                    command,
                    "invalid_argument",
                    "diagnostic export target must be a file, not a directory",
                    2,
                ));
            }
            if let Err(outcome) =
                require_destructive_confirmation(command, cli, "Write the diagnostic bundle?")
            {
                return Ok(outcome);
            }
            // The manifest fields and redactions are the only explanation; they
            // belong to the envelope, never to stderr, so `--json` keeps the
            // stream separation scripts rely on.
            let manifest = core.export_diagnostics(path)?;
            Ok(CliOutcome::success(
                command,
                manifest_data(&manifest),
                render_manifest(&manifest),
            ))
        }
        None => {
            // A corrupt cache must not make diagnostics unavailable: Core
            // returns the degraded report plus an explicit warning.
            let (report, cache_warning) = core.doctor_with_cache_status()?;
            let mut data = doctor_data(&report);
            let mut human = render_doctor(&report);
            if let Some(warning) = cache_warning {
                data["cache_warning"] = json!(warning);
                human.push_str(&render_cache_warning(warning));
            }
            Ok(CliOutcome::success(command, data, human))
        }
    }
}

/// Decision for a destructive command, factored out so a unit test never needs
/// a terminal or stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestructiveDecision {
    Proceed,
    RefuseNonInteractive,
    Ask,
}

pub fn destructive_decision(yes: bool, interactive: bool) -> DestructiveDecision {
    if yes {
        DestructiveDecision::Proceed
    } else if interactive {
        DestructiveDecision::Ask
    } else {
        DestructiveDecision::RefuseNonInteractive
    }
}

/// Enforce the destructive-command contract.
///
/// `--yes` is the only non-interactive consent. On a TTY the user is asked and
/// the default answer is "no". Nothing destructive happens before this returns.
#[allow(clippy::result_large_err)]
fn require_destructive_confirmation(
    command: &Command,
    cli: &Execution,
    prompt: &str,
) -> Result<(), CliOutcome> {
    match destructive_decision(cli.yes, std::io::stdin().is_terminal()) {
        DestructiveDecision::Proceed => Ok(()),
        DestructiveDecision::RefuseNonInteractive => Err(CliOutcome::local_failure(
            command,
            "invalid_argument",
            format!("{prompt} Re-run with --yes in non-interactive mode."),
            2,
        )),
        DestructiveDecision::Ask => {
            if prompt_yes_no(prompt) {
                Ok(())
            } else {
                Err(CliOutcome::local_failure(
                    command,
                    "cancelled",
                    "cancelled by the user; nothing was changed",
                    2,
                ))
            }
        }
    }
}

/// Normalize `--fingerprint` before any network call.
///
/// The pin is SHA-256 hex; display grouping (ASCII whitespace, `:` or `-`) and
/// an optional `sha256:` prefix are accepted. The invalid value is never echoed
/// because a mistyped pin may itself be pasted key material.
pub fn parse_fingerprint(value: &str) -> Result<String, CliError> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if !value.chars().all(|character| {
        character.is_ascii_hexdigit()
            || character.is_ascii_whitespace()
            || matches!(character, ':' | '-')
    }) {
        return Err(CliError::new(
            "invalid_argument",
            "fingerprint must be 64 hex characters (spaces, ':' or '-' may group them)",
        ));
    }
    let normalized: String = value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .map(|character| character.to_ascii_lowercase())
        .collect();
    if normalized.len() != 64 {
        return Err(CliError::new(
            "invalid_argument",
            "fingerprint must be exactly 64 hex characters (32 bytes)",
        ));
    }
    Ok(normalized)
}

#[cfg(unix)]
fn prompt_yes_no(prompt: &str) -> bool {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};

    // A blocking stdin read cannot observe the async signal handler, so a
    // Ctrl-C at the prompt would otherwise hang forever. Install a temporary
    // synchronous handler, poll stdin with a bounded timeout and treat SIGINT
    // as a refusal (the safe default). The previous disposition is restored so
    // the rest of the command keeps the normal cancellation contract.
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);
    extern "C" fn mark_interrupted(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }

    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();

    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = mark_interrupted as *const () as usize;
    action.sa_flags = 0;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    let mut previous_int: libc::sigaction = unsafe { std::mem::zeroed() };
    let mut previous_term: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigaction(libc::SIGINT, &action, &mut previous_int);
        libc::sigaction(libc::SIGTERM, &action, &mut previous_term);
    }

    let answer = loop {
        if INTERRUPTED.swap(false, Ordering::SeqCst) {
            break false;
        }
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 200) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break false;
        }
        if ready == 0 {
            continue;
        }
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            break false;
        }
        break matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    };

    unsafe {
        libc::sigaction(libc::SIGINT, &previous_int, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &previous_term, std::ptr::null_mut());
    }
    answer
}

#[cfg(not(unix))]
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
    I: std::future::Future<Output = ()>,
    C: FnMut() -> F,
    F: std::future::Future<Output = whalerein_core::api::CoreResult<bool>>,
{
    tokio::pin!(answer);
    tokio::select! {
        result = &mut answer => OpenAnswerWait::Finished(Box::new(result), false),
        _ = interrupt => {
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

/// Local `unknown` outcome that stays forensically traceable.
///
/// The bounded diagnostic history is written as well so a later
/// `doctor --export` can explain what was not observed.
fn unknown_outcome(core: &Core, command: &Command, message: String) -> CliOutcome {
    let _ = core.save_failure("unknown");
    CliOutcome::local_failure_with_diagnostic(command, "unknown", message, 7, core.diagnostic_id())
}

/// Phases the source accepts an answer for. Everything else is an input
/// problem and must never be reported as stale source data (QMQ-04).
fn question_phase_answerable(phase: &str) -> bool {
    matches!(phase, "QUESTION_OPEN" | "QUESTION_CONTINUED")
}

fn validate_requested_question_phase(phase: &str) -> Result<(), CoreError> {
    if question_phase_answerable(phase) {
        return Ok(());
    }
    if matches!(
        phase,
        "QUESTION_REPLY_QUEUED" | "QUESTION_SETTLED" | "QUESTION_PHASE_UNSPECIFIED"
    ) {
        return Err(CoreError::InvalidInput(format!(
            "question phase {phase} is not answerable; only QUESTION_OPEN and QUESTION_CONTINUED accept an answer"
        )));
    }
    Err(CoreError::InvalidInput(
        "unknown question phase; expected QUESTION_OPEN or QUESTION_CONTINUED".into(),
    ))
}

fn validate_source_question_phase(phase: &str) -> Result<(), CoreError> {
    if question_phase_answerable(phase) {
        return Ok(());
    }
    Err(CoreError::InvalidInput(format!(
        "the current question phase {phase} is not answerable; wait for the next question or refresh"
    )))
}

/// Point a failed cancellation at a real pending request or state that the
/// supplied command id cannot be recovered (QMQ-05).
fn question_cancel_guidance(core: &Core, command_id: &str) -> String {
    question_cancel_message(command_id, &pending_commands(core))
}

fn question_cancel_message(
    command_id: &str,
    pending: &[whalerein_core::api::PendingCommand],
) -> String {
    if let Some(command) = pending.iter().find(|item| item.command_id == command_id) {
        return format!(
            "No active open answer matches command {} (request {}). It may have completed already; recover its result with `whalerein operation recover {}`.\n",
            sanitize_field(&command.command_id),
            sanitize_field(&command.request_id),
            sanitize_field(&command.request_id)
        );
    }
    let candidates: Vec<String> = pending
        .iter()
        .take(3)
        .map(|item| {
            format!(
                "{} (request {})",
                sanitize_field(&item.command_id),
                sanitize_field(&item.request_id)
            )
        })
        .collect();
    if candidates.is_empty() {
        format!(
            "No pending command matches {}; a mistyped command id cannot be recovered. Run `whalerein operation list` to list accepted commands.\n",
            sanitize_field(command_id)
        )
    } else {
        format!(
            "No pending command matches {}; a mistyped command id cannot be recovered. Pending commands: {}.\n",
            sanitize_field(command_id),
            candidates.join(", ")
        )
    }
}

/// The requested selection is already the effective next model. Sending it
/// again would only create a second indistinguishable pending command
/// (QMQ-10).
fn model_selection_matches(
    current: &SessionModel,
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> bool {
    current.next.as_ref().is_some_and(|choice| {
        choice.provider == provider
            && choice.model == model
            && choice.effort.as_deref().unwrap_or("") == effort.unwrap_or("")
    })
}

#[cfg(test)]
mod qmq_cli_tests {
    use super::*;
    use whalerein_core::api::{ModelChoice, PendingCommand};

    fn pending(request: &str, command: &str) -> PendingCommand {
        PendingCommand {
            access_url: "http://127.0.0.1:8080".into(),
            host_fingerprint: "00".repeat(32),
            request_id: request.into(),
            command_id: command.into(),
            host_id: "host-1".into(),
            device_id: "device-1".into(),
            status: whalerein_core::api::OnlineStatus::HostAccepted,
        }
    }

    #[test]
    fn question_phase_validation_is_an_input_error_not_stale_data() {
        assert!(validate_requested_question_phase("QUESTION_OPEN").is_ok());
        assert!(validate_requested_question_phase("QUESTION_CONTINUED").is_ok());
        for invalid in ["question_open", "OPEN", "", "QUESTION_OPEN "] {
            let error = validate_requested_question_phase(invalid).unwrap_err();
            assert_eq!(error.code(), "invalid_argument");
            assert_eq!(error.exit_code(), 2);
        }
        let unanswerable = validate_requested_question_phase("QUESTION_REPLY_QUEUED").unwrap_err();
        assert_eq!(unanswerable.code(), "invalid_argument");
        assert!(unanswerable.to_string().contains("not answerable"));
        assert!(validate_source_question_phase("QUESTION_CONTINUED").is_ok());
        let current = validate_source_question_phase("QUESTION_SETTLED").unwrap_err();
        assert_eq!(current.code(), "invalid_argument");
        assert!(current.to_string().contains("current question phase"));
    }

    #[test]
    fn question_cancel_guidance_points_at_a_real_pending_request() {
        let commands = vec![pending("request-1", "command-1")];
        let matched = question_cancel_message("command-1", &commands);
        assert!(matched.contains("operation recover request-1"), "{matched}");
        let typo = question_cancel_message("command-typо", &commands);
        assert!(typo.contains("No pending command matches"), "{typo}");
        assert!(
            typo.contains("Pending commands: command-1 (request request-1)"),
            "{typo}"
        );
        let empty = question_cancel_message("command-typо", &[]);
        assert!(empty.contains("operation list"), "{empty}");
        assert!(!empty.contains("Pending commands:"), "{empty}");
    }

    #[test]
    fn model_selection_match_requires_the_exact_effective_choice() {
        let current = SessionModel {
            pending: None,
            last_used: None,
            default_selection: None,
            default_is_fallback: false,
            next: Some(ModelChoice {
                provider: "provider".into(),
                model: "model".into(),
                effort: Some("high".into()),
            }),
            pending_known: true,
        };
        assert!(model_selection_matches(
            &current,
            "provider",
            "model",
            Some("high")
        ));
        assert!(!model_selection_matches(
            &current, "provider", "model", None
        ));
        assert!(!model_selection_matches(
            &current,
            "provider",
            "other",
            Some("high")
        ));
        assert!(!model_selection_matches(
            &current,
            "other",
            "model",
            Some("high")
        ));
        let empty = SessionModel {
            next: None,
            ..current.clone()
        };
        assert!(!model_selection_matches(
            &empty,
            "provider",
            "model",
            Some("high")
        ));
    }
}

#[cfg(test)]
mod instance_command_tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::{test_support::FakeAccess, ClientConfig, MemorySecretStore};

    async fn execute(
        config: &std::path::Path,
        secrets: Arc<dyn SecretStore>,
        args: &[&str],
    ) -> CliOutcome {
        let config = config.to_str().expect("fixture path");
        let cli = Cli::try_parse_from(
            ["whalerein", "--config", config, "--json"]
                .into_iter()
                .chain(args.iter().copied()),
        )
        .unwrap();
        let diagnostics = CliDiagnosticSink::disabled();
        Box::pin(run_with_signal(
            cli,
            secrets,
            std::future::pending(),
            &diagnostics,
            DiagnosticContext::new(diagnostics.clone()),
        ))
        .await
    }

    #[tokio::test]
    async fn bounded_watch_reports_host_offline() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let access = FakeAccess::start().await;
        let core = Core::open(&config, secrets.clone()).unwrap();
        core.connect_standalone(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();

        access.host.lock().unwrap().host_online = false;
        let watch = execute(
            &config,
            secrets,
            &[
                "session",
                "watch",
                "session-1",
                "--host",
                "host-1",
                "--once",
                "--timeout",
                "3",
            ],
        )
        .await;
        assert_eq!(
            watch.to_json()["error"]["code"],
            "host_offline",
            "{watch:?}"
        );
    }

    #[tokio::test]
    async fn grouped_commands_use_one_open_instance_and_exact_send_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let access = FakeAccess::start().await;
        let core = Core::open(&config, secrets.clone()).unwrap();
        core.connect_standalone(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();

        let status = execute(&config, secrets.clone(), &["status"]).await;
        assert_eq!(status.exit, 0, "{status:?}");
        assert_eq!(status.to_json()["data"]["device_access"], "ready");
        assert_eq!(status.to_json()["data"]["default_host"], "host-1");

        let hosts = execute(&config, secrets.clone(), &["host", "list"]).await;
        assert_eq!(hosts.to_json()["data"]["hosts"][0]["host_id"], "host-1");
        let sessions = execute(&config, secrets.clone(), &["session", "list"]).await;
        assert_eq!(
            sessions.to_json()["data"]["sessions"][0]["session_id"],
            "session-1"
        );

        let read = execute(
            &config,
            secrets.clone(),
            &["session", "show", "session-1", "--host", "host-1"],
        )
        .await;
        assert_eq!(read.to_json()["data"]["target"]["host_id"], "host-1");
        access.host.lock().unwrap().omit_next_command_response_body = true;
        let sent = execute(
            &config,
            secrets.clone(),
            &[
                "session",
                "send",
                "session-1",
                "fixture message",
                "--timeout",
                "5",
            ],
        )
        .await;
        assert_eq!(sent.exit, 0, "{sent:?}");
        let data = sent.to_json()["data"].clone();
        assert_eq!(data["reply"]["state"], "committed");
        assert!(data["reply"]["reference"].is_string(), "{data}");
        assert_eq!(data["message"]["message_id"], "message-1");
        assert_eq!(data["read_error"], serde_json::Value::Null);
        assert_eq!(data["receipt"]["message_id"], "message-1");
        let operation = data["operation_id"].as_str().unwrap();
        let commands_seen = access.host.lock().unwrap().commands_seen;
        let recovered = execute(
            &config,
            secrets.clone(),
            &["operation", "recover", operation],
        )
        .await;
        assert_eq!(recovered.exit, 0, "{recovered:?}");
        assert_eq!(recovered.to_json()["data"]["reply"]["state"], "committed");
        assert_eq!(
            recovered.to_json()["data"]["message"]["message_id"],
            "message-1"
        );
        assert_eq!(recovered.to_json()["data"]["receipt"], data["receipt"]);
        assert_eq!(access.host.lock().unwrap().commands_seen, commands_seen);

        let detached = execute(
            &config,
            secrets.clone(),
            &["session", "send", "session-1", "fixture detach", "--detach"],
        )
        .await;
        assert_eq!(detached.to_json()["data"]["state"], "submission");
        assert!(detached.to_json()["data"]["reply"].is_null());

        let missing_host = execute(
            &config,
            secrets.clone(),
            &["session", "show", "session-1", "--host", "unknown"],
        )
        .await;
        assert_eq!(missing_host.to_json()["error"]["code"], "not_found");
        let no_consent = execute(&config, secrets, &["device", "forget"]).await;
        assert_eq!(no_consent.exit, 2);
        assert_eq!(core.default_host().unwrap().as_deref(), Some("host-1"));
    }

    #[test]
    fn timeout_seconds_are_exact_and_bounded() {
        for (input, milliseconds) in [("1", 1000), ("0.001", 1), ("0.125", 125), ("3.5", 3500)] {
            assert_eq!(
                parse_timeout_seconds(input),
                Ok(Duration::from_millis(milliseconds))
            );
        }
        for input in ["-1", "0", "1.0001", "NaN", "1e2", "2147484"] {
            assert!(parse_timeout_seconds(input).is_err(), "{input}");
        }
    }

    #[tokio::test]
    async fn grouped_advanced_session_controls_preserve_native_guards() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let core = Core::open(&config, secrets.clone()).unwrap();
        let paired = core
            .connect_standalone(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let device_id = paired.device_id;

        let catalog = execute(
            &config,
            secrets.clone(),
            &["session", "models", "--host", "host-1"],
        )
        .await;
        assert_eq!(catalog.exit, 0);
        assert_eq!(
            catalog.to_json()["data"]["default_selection"]["model"],
            "model"
        );
        let read = execute(
            &config,
            secrets.clone(),
            &["session", "model", "get", "--session", "session-1"],
        )
        .await;
        assert_eq!(read.exit, 0);
        assert!(read.to_json()["data"]["pending"].is_null());
        let selected = execute(
            &config,
            secrets.clone(),
            &[
                "session",
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
        let waited = execute(
            &config,
            secrets.clone(),
            &["session", "wait", "session-1", "--timeout", "3.000"],
        )
        .await;
        assert_eq!(waited.exit, 0);
        assert!(waited.human.contains("Idle"));
        assert_eq!(waited.to_json()["data"]["reason"], "idle");
        assert_eq!(waited.to_json()["data"]["state"]["queue_available"], false);
        let default_wait =
            execute(&config, secrets.clone(), &["session", "wait", "session-1"]).await;
        assert_eq!(default_wait.exit, 0);
        assert_eq!(default_wait.to_json()["data"]["reason"], "idle");
        // The slice leaves a small response allowance inside the 30s
        // transport budget; one millisecond of scheduling drift is allowed.
        let slice = access.host.lock().unwrap().wait_last_timeout_ms;
        assert!((27_900..=28_000).contains(&slice), "wait slice {slice}");
        access.host.lock().unwrap().wait_timeouts = usize::MAX;
        for (extra, exit) in [(false, 0), (true, 7)] {
            // One second leaves room for Host discovery under parallel test load
            // while still proving the timeout-observation contract.
            let mut args = vec!["session", "wait", "session-1", "--timeout", "1"];
            if extra {
                args.push("--fail-on-timeout");
            }
            let outcome = execute(&config, secrets.clone(), &args).await;
            assert_eq!(outcome.exit, exit);
            assert_eq!(outcome.to_json()["data"]["reason"], "timeout");
        }
        access.host.lock().unwrap().wait_timeouts = 0;
        access.host.lock().unwrap().set_query_unavailable(true);
        let transport_failure =
            execute(&config, secrets.clone(), &["session", "wait", "session-1"]).await;
        assert_eq!(transport_failure.to_json()["error"]["code"], "unavailable");
        access.host.lock().unwrap().set_query_unavailable(false);
        let invalid_wait = execute(
            &config,
            secrets.clone(),
            &["session", "wait", "session-1", "--timeout", "0"],
        )
        .await;
        assert_eq!(invalid_wait.exit, 2);
        let queue = execute(
            &config,
            secrets.clone(),
            &["session", "queue", "list", "--session", "session-1"],
        )
        .await;
        assert_eq!(queue.exit, 0);
        assert_eq!(queue.to_json()["data"]["items"][0]["item_id"], "queue-1");
        for action in ["edit", "remove", "steer"] {
            let mut args = vec![
                "session",
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
            let result = execute(&config, secrets.clone(), &args).await;
            assert_eq!(result.exit, 0);
            assert_eq!(result.to_json()["data"]["refresh"], "current");
            assert!(!result.to_json().to_string().contains("new fixture"));
        }
        access.host.lock().unwrap().fail_pending_queue = true;
        let result = execute(
            &config,
            secrets.clone(),
            &[
                "session",
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
        let current = execute(
            &config,
            secrets.clone(),
            &["session", "queue", "list", "--session", "session-1"],
        )
        .await;
        assert_eq!(current.exit, 0);
        assert_eq!(current.to_json()["data"]["items"][0]["item_id"], "queue-1");

        let questions = execute(
            &config,
            secrets.clone(),
            &["session", "question", "list", "--session", "session-1"],
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
        let answer = execute(
            &config,
            secrets.clone(),
            &[
                "session",
                "question",
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
        let stale = execute(
            &config,
            secrets.clone(),
            &[
                "session",
                "question",
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
        let bad = execute(
            &config,
            secrets.clone(),
            &[
                "session",
                "question",
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

        let approved = execute(
            &config,
            secrets.clone(),
            &[
                "session",
                "approval",
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
        assert_eq!(approved.to_json()["command"], "session.approval.approve");

        let rejected = execute(
            &config,
            secrets.clone(),
            &[
                "session",
                "approval",
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

        let interrupted = execute(
            &config,
            secrets.clone(),
            &["session", "interrupt", "session-1"],
        )
        .await;
        assert_eq!(interrupted.exit, 0);

        let doctor = execute(&config, secrets.clone(), &["doctor"]).await;
        assert_eq!(doctor.exit, 0);
        assert!(doctor.to_json()["data"]["commands"]["non_terminal"].is_number());
        assert!(doctor.to_json()["data"].get("connection").is_none());
        assert!(doctor.to_json()["data"].get("detail").is_none());

        let export_path = dir.path().join("bundle.json");
        let exported = execute(
            &config,
            secrets.clone(),
            &["doctor", "--export", export_path.to_str().unwrap(), "--yes"],
        )
        .await;
        assert_eq!(exported.exit, 0);
        assert!(export_path.exists());
        let bundle = std::fs::read_to_string(&export_path).unwrap();
        assert!(!bundle.contains("continue"));
        assert!(!bundle.contains("test session"));

        let revoked = execute(&config, secrets.clone(), &["device", "revoke", "--yes"]).await;
        assert_eq!(revoked.exit, 0);
        assert_eq!(revoked.to_json()["data"]["revoked"], true);
        assert_eq!(revoked.to_json()["data"]["device_id"], device_id);

        let after = execute(&config, secrets, &["host", "list"]).await;
        assert_eq!(after.exit, 4);
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
            async {},
            || async { Ok(calls.fetch_add(1, Ordering::SeqCst) > 0) },
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, OpenAnswerWait::CancellationRequested));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let result = wait_open_answer(
            never_answer(),
            async {},
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
            async {},
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
            std::future::pending::<()>(),
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

    #[test]
    fn fingerprint_is_normalized_and_rejected_before_any_network_call() {
        let expected = "ab".repeat(32);
        assert_eq!(parse_fingerprint(&expected).unwrap(), expected);
        let grouped = "AB"
            .repeat(32)
            .as_bytes()
            .chunks(4)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(parse_fingerprint(&grouped).unwrap(), expected);
        let colon_grouped = format!("sha256:{}ab", "ab:".repeat(31));
        assert_eq!(parse_fingerprint(&colon_grouped).unwrap(), expected);
        for value in ["zz", "", "ab", &"ab".repeat(31), &"ab".repeat(33), "ab cd"] {
            let error = parse_fingerprint(value).unwrap_err();
            assert_eq!(error.code, "invalid_argument", "{value:?}");
            assert!(
                !error.message.contains(value) || value.is_empty(),
                "must not echo the pin: {value:?}"
            );
        }
    }

    #[test]
    fn fork_directory_sentence_covers_success_warning_and_absence() {
        assert_eq!(
            fork_directory_sentence(Some("/work-child"), None),
            " Directory: /work-child."
        );
        assert_eq!(
            fork_directory_sentence(None, Some("unavailable")),
            " The child working directory could not be read (unavailable); the child still exists and must not be retried or deleted."
        );
        assert_eq!(fork_directory_sentence(None, None), "");
        // A hostile directory cannot inject layout or ANSI.
        let hostile = fork_directory_sentence(Some("/tmp\tx\u{1b}[31m"), None);
        assert!(!hostile.contains('\u{1b}'), "{hostile}");
        assert!(hostile.contains("\\t"), "{hostile}");
    }

    #[test]
    fn revocation_warning_names_the_next_step_only_when_retention_is_unverified() {
        let verified = revoke_human(false);
        assert_eq!(
            verified,
            "Revoked this device's grants. Local identity was kept.\n"
        );
        assert!(!verified.contains("operation list"));
        let unverified = revoke_human(true);
        assert!(unverified.contains("Unable to verify"));
        assert!(unverified.contains("whalerein operation list"));
        assert!(unverified.contains("do not reuse the old identity"));
        assert!(unverified.ends_with('\n'));
    }

    #[tokio::test]
    async fn lost_standalone_confirmation_renders_and_recovers_the_exact_operation() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let core = Core::open(&config, secrets.clone()).unwrap();
        let access = FakeAccess::start().await;
        let invocation = Execution {
            host: None,
            json: true,
            yes: true,
            show_reasoning: false,
        };
        let command = Command::Connect {
            host: None,
            invite_stdin: true,
            fingerprint: None,
            name: None,
        };

        let rejected = Box::pin(pair(
            &core,
            &command,
            &invocation,
            &access.pairing_uri(),
            Some("invalid-pin"),
            None,
        ))
        .await
        .unwrap();
        assert_eq!(rejected.exit, 2);
        assert!(!rejected.to_json().to_string().contains("invalid-pin"));

        access.host.lock().unwrap().drop_pair_finish_once = true;
        let result = Box::pin(pair(
            &core,
            &command,
            &invocation,
            &access.pairing_uri(),
            Some(&access.fingerprint()),
            Some("fixture"),
        ))
        .await;
        let uncertain = finish_core_result(&core, &command, result);
        assert_eq!(uncertain.exit, 7, "{uncertain:?}");
        let output = uncertain.to_json();
        let operation = output["data"]["operation_id"].as_str().unwrap();
        assert_eq!(output["data"]["host_id"], "host-1");
        assert!(core.default_host().unwrap().is_none());
        assert!(!output.to_string().contains("whalerein://pair"));
        access.host.lock().unwrap().drop_pair_finish_once = false;

        let recovery = execute(
            &config,
            secrets.clone(),
            &["operation", "recover", operation],
        )
        .await;
        assert_eq!(recovery.exit, 0, "{recovery:?}");
        assert_eq!(recovery.to_json()["data"]["operation_id"], operation);
        let selected = execute(&config, secrets, &["host", "default", "host-1"]).await;
        assert_eq!(selected.exit, 0, "{selected:?}");
        assert_eq!(selected.to_json()["data"]["host_id"], "host-1");
    }

    #[tokio::test]
    async fn cloud_rejects_standalone_invitation_and_fresh_logout_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::WhaleReinCloud, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let result = execute(
            &config,
            secrets.clone(),
            &["host", "connect", "--invite-stdin"],
        )
        .await;
        assert_eq!(result.exit, 2, "{result:?}");
        assert!(result.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Standalone"));
        let result = execute(&config, secrets, &["logout"]).await;
        assert_eq!(result.exit, 0, "{result:?}");
        assert_eq!(result.to_json()["data"]["signed_out"], true);
        assert!(!temp.path().join("instance").exists());
    }

    #[tokio::test]
    async fn watch_timeout_keeps_the_resolved_host_and_session_in_json() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("client.toml");
        Core::write_config(
            &config,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let core = Core::open(&config, secrets.clone()).unwrap();
        let access = FakeAccess::start().await;
        Box::pin(core.connect_standalone(&access.pairing_uri(), |_| PairingDecision::approve()))
            .await
            .unwrap();
        let watched = execute(
            &config,
            secrets,
            &[
                "session",
                "watch",
                "session-1",
                "--once",
                "--timeout",
                "0.05",
            ],
        )
        .await;
        let value = watched.to_json();
        assert_eq!(value["command"], "session.watch");
        assert!(value["error"].is_object(), "{watched:?}");
        assert_eq!(
            value["data"]["target"],
            json!({"host_id": "host-1", "session_id": "session-1"})
        );
    }
}

#[cfg(test)]
mod coverage_tests;

#[cfg(test)]
mod session_observation_cli_tests {
    use super::coverage_tests::Fixture;
    use super::*;
    use whalerein_core::conversation::{
        content_block::{self, TextContent},
        AttemptOutcome, AttemptRef, ContentBlock, ConversationMessage, MessageRole,
    };

    fn session_key() -> SessionKey {
        SessionKey::new("host-1", "session-1").unwrap()
    }

    fn attempt() -> AttemptRef {
        AttemptRef {
            live_epoch: "epoch".into(),
            attempt_id: "attempt-1".into(),
        }
    }

    fn text_message(text: &str) -> ViewUpdate {
        ViewUpdate::Message {
            message: ConversationMessage {
                message_id: "message-1".into(),
                role: MessageRole::Assistant,
                blocks: vec![ContentBlock {
                    block_id: "text".into(),
                    content: Some(content_block::Content::Text(TextContent {
                        text: text.into(),
                    })),
                }],
                authored_at_unix_ms: 1,
            },
            origin: Some(attempt()),
            preview_prefix: None,
        }
    }

    // AC-13 tail-human/JSON: both renderers consume the same Core-owned typed
    // event, and JSON keeps its documented envelope without wire/native DTOs.
    #[test]
    fn cli_session_observation_ac_13_tail_human_and_json_shape_is_unchanged() {
        let key = session_key();
        let update = text_message("hello");
        let human = render_tail_event(false, &mut TailRenderer::default(), &key, &update, false);
        assert_eq!(human, "[ASSISTANT] hello\n");
        let line = render_tail_event(true, &mut TailRenderer::default(), &key, &update, false);
        assert!(line.ends_with('\n'), "{line:?}");
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["schema"], "whalerein/client/v1");
        assert_eq!(value["command"], "session.watch");
        assert_eq!(value["data"]["event"]["type"], "message");
        assert_eq!(value["data"]["event"]["session"]["host_id"], "host-1");
        assert_eq!(value["data"]["event"]["session"]["session_id"], "session-1");
        assert_eq!(value["data"]["event"]["blocks"][0]["type"], "text");
        assert_eq!(value["data"]["event"]["blocks"][0]["text"], "hello");
        assert!(value["data"]["event"].get("payload").is_none());
        assert!(value["data"]["event"].get("data_json").is_none());
    }

    // AC-13 tail-completion: `--once` completes only on visible output, not on
    // state/reset metadata or a committed attempt marker.
    #[test]
    fn cli_session_observation_ac_13_tail_completion_tracks_visible_output() {
        let key = session_key();
        let mut renderer = TailRenderer::default();
        let state = ViewUpdate::State(Default::default());
        assert!(!render_tail_event(false, &mut renderer, &key, &state, false).is_empty());
        assert!(!is_tail_output(&state, false));
        assert!(!is_tail_output(&ViewUpdate::Reset, false));
        assert!(!is_tail_output(
            &ViewUpdate::Preview {
                attempt: attempt(),
                block_id: "text".into(),
                kind: whalerein_core::conversation::BlockKind::Text,
                text: String::new(),
                replace: false,
                incomplete: false,
            },
            false
        ));
        assert!(is_tail_output(&text_message("done"), false));
        assert!(is_tail_output(
            &ViewUpdate::Preview {
                attempt: attempt(),
                block_id: "text".into(),
                kind: whalerein_core::conversation::BlockKind::Text,
                text: "live".into(),
                replace: false,
                incomplete: false,
            },
            false
        ));
        assert!(is_tail_output(
            &ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Abandoned,
            },
            false
        ));
        assert!(!is_tail_output(
            &ViewUpdate::AttemptEnded {
                attempt: attempt(),
                outcome: AttemptOutcome::Committed,
            },
            false
        ));
    }

    // AC-13 tail-once: an observation with no visible output reaches the
    // caller's deadline and reports the unchanged timeout envelope.
    #[tokio::test]
    async fn cli_session_observation_ac_13_watch_once_without_visible_output_times_out() {
        let fixture = Fixture::paired().await;
        let started = std::time::Instant::now();
        let outcome = fixture
            .run(&[
                "session",
                "watch",
                "session-1",
                "--once",
                "--timeout",
                "0.2",
            ])
            .await;
        assert_eq!(outcome.exit, 7, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/client/v1");
        assert_eq!(value["command"], "session.watch");
        assert_eq!(value["error"]["code"], "timeout");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn cli_session_observation_ac_13_question_list_is_one_shot_with_stamp() {
        let fixture = Fixture::paired().await;
        let before = fixture.access.host.lock().unwrap().question_reads;
        let outcome = fixture
            .run(&["session", "question", "list", "--session", "session-1"])
            .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        assert!(outcome.human.contains("batch batch-1"));
        assert!(outcome.human.contains("Synthetic question"));
        let value = outcome.to_json();
        assert_eq!(
            value["data"]["stamp"]["observation_epoch"],
            "question-epoch-1"
        );
        assert_eq!(value["data"]["stamp"]["revision"], 1);
        assert_eq!(value["data"]["complete"], true);
        assert_eq!(value["data"]["next_cursor"], "");
        assert_eq!(value["data"]["batches"][0]["batch_id"], "batch-1");
        assert_eq!(
            fixture.access.host.lock().unwrap().question_reads,
            before + 1
        );
        assert!(value["data"].get("availability").is_none());
        assert!(value["data"].get("unavailable_code").is_none());
        assert!(value["data"].get("payload").is_none());
    }

    #[tokio::test]
    async fn cli_session_observation_ac_21_answer_rejects_stale_batch_without_submission() {
        let fixture = Fixture::paired().await;
        let answers = fixture.directory.path().join("answers.json");
        std::fs::write(&answers, r#"{"answers":[{"id":"q","selected":["A"]}]}"#).unwrap();
        let before = fixture.access.host.lock().unwrap().commands_seen;
        let stale = fixture
            .run(&[
                "session",
                "question",
                "answer",
                "--session",
                "session-1",
                "--batch",
                "batch-1",
                "--epoch",
                "epoch",
                "--version",
                "2",
                "--digest",
                &"07".repeat(32),
                "--phase",
                "QUESTION_CONTINUED",
                "--input",
                answers.to_str().unwrap(),
            ])
            .await;
        assert_eq!(stale.exit, 8, "{stale:?}");
        let value = stale.to_json();
        assert_eq!(value["error"]["code"], "conflict");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("question changed"));
        assert_eq!(fixture.access.host.lock().unwrap().commands_seen, before);
    }
}

#[cfg(test)]
mod history_send_cli_tests {
    use super::coverage_tests::Fixture;
    use super::*;
    use whalerein_core::api::SendOperation;
    use whalerein_core::conversation::ConversationMessage;
    use whalerein_core::history::MessageReference;

    fn wire_message(
        id: &str,
        role: whalerein_proto::MessageRole,
        blocks: Vec<whalerein_proto::ContentBlock>,
    ) -> whalerein_proto::ConversationMessage {
        whalerein_proto::ConversationMessage {
            message_id: id.into(),
            role: role as i32,
            blocks,
            authored_at_unix_ms: 1,
        }
    }

    fn text_block(block_id: &str, text: &str) -> whalerein_proto::ContentBlock {
        whalerein_proto::ContentBlock {
            block_id: block_id.into(),
            content: Some(whalerein_proto::content_block::Content::Text(
                whalerein_proto::TextContent { text: text.into() },
            )),
        }
    }

    fn reasoning_block(block_id: &str, text: &str) -> whalerein_proto::ContentBlock {
        whalerein_proto::ContentBlock {
            block_id: block_id.into(),
            content: Some(whalerein_proto::content_block::Content::Reasoning(
                whalerein_proto::ReasoningContent { text: text.into() },
            )),
        }
    }

    fn reference_token(session: &str, message: &str) -> String {
        MessageReference::new(
            SessionKey::new("host-1", session).unwrap(),
            message,
            format!("locator:{message}"),
            "projection-1",
        )
        .unwrap()
        .to_token()
    }

    fn operation(host: &str, session: &str) -> SendOperation {
        SendOperation {
            operation_id: "op-1".into(),
            target: SessionKey::new(host, session).unwrap(),
            receipt: None,
        }
    }

    #[tokio::test]
    async fn history_latest_page_first_and_continuations_walk_older_without_gaps() {
        let f = Fixture::paired().await;
        {
            let mut host = f.access.host.lock().unwrap();
            host.history_messages = (1..=5)
                .map(|index| {
                    wire_message(
                        &format!("m-{index}"),
                        whalerein_proto::MessageRole::Assistant,
                        vec![text_block("text", &format!("message {index}"))],
                    )
                })
                .collect();
            host.history_page_size = 2;
        }
        let first = f
            .run(&["session", "history", "session-1", "--max-messages", "2"])
            .await;
        assert_eq!(first.exit, 0, "{first:?}");
        let value = first.to_json();
        assert_eq!(value["command"], "session.history");
        assert_eq!(value["data"]["source"], "online");
        assert_eq!(value["data"]["projection"], "projection-1");
        assert_eq!(value["data"]["messages"][0]["message_id"], "m-4");
        assert_eq!(value["data"]["messages"][1]["message_id"], "m-5");
        assert_eq!(value["data"]["complete"], false);
        assert!(first.human.contains("Source: online"), "{}", first.human);
        let continuation = value["data"]["continuation"].as_str().unwrap().to_owned();

        let second = f
            .run(&[
                "session",
                "history",
                "session-1",
                "--continuation",
                &continuation,
            ])
            .await;
        assert_eq!(second.exit, 0, "{second:?}");
        let value = second.to_json();
        assert_eq!(value["data"]["messages"][0]["message_id"], "m-2");
        assert_eq!(value["data"]["messages"][1]["message_id"], "m-3");
        let continuation = value["data"]["continuation"].as_str().unwrap().to_owned();
        let third = f
            .run(&[
                "session",
                "history",
                "session-1",
                "--continuation",
                &continuation,
            ])
            .await;
        assert_eq!(third.exit, 0, "{third:?}");
        let value = third.to_json();
        assert_eq!(value["data"]["messages"][0]["message_id"], "m-1");
        assert_eq!(value["data"]["complete"], true);
        assert!(value["data"]["continuation"].is_null());
        assert!(third.human.contains("End of history"), "{}", third.human);

        // One traversal sees every message exactly once, oldest page last.
        let mut ids: Vec<String> = [&first, &second, &third]
            .iter()
            .flat_map(|outcome| {
                outcome.to_json()["data"]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|message| message["message_id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["m-1", "m-2", "m-3", "m-4", "m-5"]);
    }

    #[tokio::test]
    async fn history_cache_read_is_explicit_and_never_creates_a_watch_lease() {
        let f = Fixture::paired().await;
        let cached = f.run(&["session", "history", "session-1", "--cache"]).await;
        assert_eq!(cached.exit, 0, "{cached:?}");
        let value = cached.to_json();
        assert_eq!(value["data"]["source"], "cache");
        assert_eq!(value["data"]["messages"], serde_json::json!([]));
        assert_eq!(value["data"]["complete"], true);
        assert!(cached.human.contains("Source: cache"), "{}", cached.human);
        // A page read never acquires an observation lease and is repeatable.
        let again = f.run(&["session", "history", "session-1", "--cache"]).await;
        assert_eq!(again.exit, 0, "{again:?}");
        assert_eq!(again.to_json()["data"]["source"], "cache");
    }

    #[tokio::test]
    async fn history_exact_read_and_stale_or_wrong_scope_failures_are_actionable() {
        let f = Fixture::paired().await;
        let token = reference_token("session-1", "message-1");
        let exact = f
            .run(&["session", "history", "session-1", "--message", &token])
            .await;
        assert_eq!(exact.exit, 0, "{exact:?}");
        let value = exact.to_json();
        assert_eq!(value["data"]["source"], "online");
        assert_eq!(value["data"]["message"]["message_id"], "message-1");
        let returned = value["data"]["reference"].as_str().unwrap();
        let parsed: MessageReference = returned.parse().unwrap();
        assert_eq!(
            parsed.session(),
            &SessionKey::new("host-1", "session-1").unwrap()
        );
        assert_eq!(parsed.message_id(), "message-1");
        assert!(
            exact.human.contains("Message: message-1"),
            "{}",
            exact.human
        );

        // A reference for another session is rejected before any read.
        let foreign = reference_token("session-2", "message-1");
        let wrong_scope = f
            .run(&["session", "history", "session-1", "--message", &foreign])
            .await;
        assert_eq!(wrong_scope.to_json()["error"]["code"], "invalid_argument");
        assert!(
            wrong_scope.to_json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("refresh history"),
            "{wrong_scope:?}"
        );

        let garbage = f
            .run(&[
                "session",
                "history",
                "session-1",
                "--continuation",
                "whr1.not-a-token.invalid",
            ])
            .await;
        assert_eq!(garbage.exit, 8, "{garbage:?}");
        let value = garbage.to_json();
        assert_eq!(value["error"]["code"], "snapshot_required");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("without --continuation"),
            "{value}"
        );

        // A real continuation from session-1 cannot be replayed for session-2.
        {
            let mut host = f.access.host.lock().unwrap();
            host.history_messages = vec![
                wire_message(
                    "message-1",
                    whalerein_proto::MessageRole::Assistant,
                    vec![text_block("text", "one")],
                ),
                wire_message(
                    "message-2",
                    whalerein_proto::MessageRole::Assistant,
                    vec![text_block("text", "two")],
                ),
            ];
            host.history_page_size = 1;
        }
        let first = f
            .run(&["session", "history", "session-1", "--max-messages", "1"])
            .await;
        let continuation = first.to_json()["data"]["continuation"]
            .as_str()
            .unwrap()
            .to_owned();
        let wrong_session = f
            .run(&[
                "session",
                "history",
                "session-2",
                "--continuation",
                &continuation,
            ])
            .await;
        assert_eq!(
            wrong_session.to_json()["error"]["code"],
            "snapshot_required"
        );
    }

    #[tokio::test]
    async fn history_reasoning_is_hidden_by_default_and_opt_in_in_human_and_json() {
        let f = Fixture::paired().await;
        f.access.host.lock().unwrap().history_messages = vec![wire_message(
            "m-1",
            whalerein_proto::MessageRole::Assistant,
            vec![
                reasoning_block("reasoning", "private reasoning"),
                text_block("text", "visible answer"),
            ],
        )];
        let hidden = f.run(&["session", "history", "session-1"]).await;
        assert_eq!(hidden.exit, 0, "{hidden:?}");
        assert!(
            !hidden.human.contains("private reasoning"),
            "{}",
            hidden.human
        );
        let value = hidden.to_json();
        assert!(!value.to_string().contains("private reasoning"), "{value}");
        assert!(value.to_string().contains("visible answer"), "{value}");

        let shown = f
            .run(&["--show-reasoning", "session", "history", "session-1"])
            .await;
        assert_eq!(shown.exit, 0, "{shown:?}");
        assert!(
            shown.human.contains("[reasoning] private reasoning"),
            "{}",
            shown.human
        );
        let value = shown.to_json();
        assert_eq!(
            value["data"]["messages"][0]["blocks"][0]["type"],
            "reasoning"
        );
        assert_eq!(
            value["data"]["messages"][0]["blocks"][0]["text"],
            "private reasoning"
        );
    }

    #[tokio::test]
    async fn send_confirmed_reply_is_read_once_and_never_resubmitted() {
        let f = Fixture::paired().await;
        let sent = f
            .run(&["session", "send", "session-1", "hello", "--timeout", "5"])
            .await;
        assert_eq!(sent.exit, 0, "{sent:?}");
        let value = sent.to_json();
        assert_eq!(value["data"]["reply"]["state"], "committed");
        assert_eq!(value["data"]["message"]["message_id"], "message-1");
        assert_eq!(value["data"]["read_error"], serde_json::Value::Null);
        assert!(sent.human.contains("Confirmed committed message message-1"));
        assert_eq!(f.access.host.lock().unwrap().commands_seen, 1);
    }

    #[tokio::test]
    async fn a_later_content_read_failure_keeps_the_confirmation_and_shows_the_exact_command() {
        let f = Fixture::paired().await;
        f.access.host.lock().unwrap().history_messages.clear();
        let submitted = f
            .run(&["session", "send", "session-1", "hello", "--timeout", "5"])
            .await;
        assert_eq!(submitted.exit, 0, "{submitted:?}");
        let value = submitted.to_json();
        assert_eq!(value["data"]["reply"]["state"], "committed");
        assert!(value["data"]["message"].is_null());
        assert_eq!(value["data"]["read_error"], "not_found");
        assert!(
            submitted.human.contains("could not be read"),
            "{}",
            submitted.human
        );
        assert!(
            submitted.human.contains("Do not resend"),
            "{}",
            submitted.human
        );
        assert!(
            submitted.human.contains("--message") && submitted.human.contains("session history"),
            "{}",
            submitted.human
        );
        // Reading content failed; the original send was not repeated.
        assert_eq!(f.access.host.lock().unwrap().commands_seen, 1);
    }

    #[test]
    fn pending_input_guidance_is_scoped_to_the_original_host_and_session() {
        let target = SessionKey::new("host-1", "session-1").unwrap();
        let follow = SendFollowUp {
            reply: SendReply::InputRequired {
                kinds: vec![InputKind::Approval, InputKind::Question],
                operation: "op-1".into(),
                session: target.clone(),
            },
            message: None,
            read_error: None,
        };
        let human = send_followup_human(&operation("host-1", "session-1"), &follow, false);
        assert!(
            human.contains("whalerein session approval list --session session-1 --host host-1"),
            "{human}"
        );
        assert!(
            human.contains("whalerein session question list --session session-1 --host host-1"),
            "{human}"
        );
        assert!(human.contains("Do not resubmit"), "{human}");
    }

    #[test]
    fn pending_and_stale_guidance_never_resubmits_and_quotes_hostile_scope() {
        let target = SessionKey::new("h'; rm -rf /", "s$(id)").unwrap();
        let operation = operation("h'; rm -rf /", "s$(id)");
        let pending = send_followup_human(
            &operation,
            &SendFollowUp {
                reply: SendReply::Pending,
                message: None,
                read_error: None,
            },
            false,
        );
        assert!(pending.contains("Do not resend"), "{pending}");
        assert!(pending.contains("session watch 's$(id)'"), "{pending}");
        assert!(pending.contains("--host 'h'\\''; rm -rf /'"), "{pending}");
        assert!(!pending.contains("--host h'; rm"), "{pending}");

        let reference =
            MessageReference::new(target.clone(), "m-1", "locator", "projection-1").unwrap();
        let stale = send_followup_human(
            &operation,
            &SendFollowUp {
                reply: SendReply::Committed { reference },
                message: None,
                read_error: Some(CoreError::Api {
                    code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
                    message: "stale".into(),
                }),
            },
            false,
        );
        assert!(stale.contains("stale"), "{stale}");
        assert!(stale.contains("Do not resend"), "{stale}");
        assert!(stale.contains("--message"), "{stale}");
        assert!(stale.contains(r"--host 'h'\''; rm -rf /'"), "{stale}");
    }

    #[test]
    fn send_json_keeps_typed_reply_fields_and_reasoning_policy() {
        use whalerein_core::conversation::content_block::{Content, ReasoningContent, TextContent};
        use whalerein_core::conversation::{ContentBlock, MessageRole};
        let operation = operation("host-1", "session-1");
        let message = ConversationMessage {
            message_id: "m-1".into(),
            role: MessageRole::Assistant,
            blocks: vec![
                ContentBlock {
                    block_id: "reasoning".into(),
                    content: Some(Content::Reasoning(ReasoningContent {
                        text: "private reasoning".into(),
                    })),
                },
                ContentBlock {
                    block_id: "text".into(),
                    content: Some(Content::Text(TextContent {
                        text: "answer".into(),
                    })),
                },
            ],
            ..Default::default()
        };
        let follow = SendFollowUp {
            reply: SendReply::Committed {
                reference: MessageReference::new(
                    operation.target.clone(),
                    "m-1",
                    "locator",
                    "projection-1",
                )
                .unwrap(),
            },
            message: Some(message),
            read_error: None,
        };
        let hidden = send_followup_data(&operation, &follow, false);
        assert_eq!(hidden["reply"]["state"], "committed");
        assert!(hidden["reply"]["reference"].is_string());
        assert_eq!(hidden["message"]["blocks"].as_array().unwrap().len(), 1);
        assert_eq!(hidden["message"]["blocks"][0]["type"], "text");
        assert!(!hidden.to_string().contains("private reasoning"));
        let shown = send_followup_data(&operation, &follow, true);
        assert_eq!(shown["message"]["blocks"][0]["type"], "reasoning");
        assert!(shown.to_string().contains("private reasoning"));
        assert_eq!(shown["read_error"], serde_json::Value::Null);
        assert!(shown["reference"].is_string());
        let human_hidden = send_followup_human(&operation, &follow, false);
        assert!(
            !human_hidden.contains("private reasoning"),
            "{human_hidden}"
        );
        let human_shown = send_followup_human(&operation, &follow, true);
        assert!(
            human_shown.contains("[reasoning] private reasoning"),
            "{human_shown}"
        );
        assert!(human_shown.contains("[ASSISTANT]"), "{human_shown}");
    }

    #[tokio::test]
    async fn verbose_history_keeps_one_stdout_envelope_and_emits_stderr_stages() {
        let f = Fixture::paired().await;
        let outcome = f
            .run(&["--verbose", "session", "history", "session-1"])
            .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        // Fixture disables capture; the binary-level test verifies stream
        // isolation. Here the command still returns exactly one envelope.
        let value = outcome.to_json();
        assert_eq!(value["schema"], "whalerein/client/v1");
        assert_eq!(value["command"], "session.history");
    }
}

#[cfg(test)]
mod download_cli_tests {
    use super::coverage_tests::Fixture;
    use super::*;
    use whalerein_core::api::test_support::FakeAttachmentObject;

    fn attachment(reference: &str, name: &str, bytes: &[u8]) -> FakeAttachmentObject {
        FakeAttachmentObject::new("session-1", reference, name, bytes.to_vec())
    }

    async fn run_download(
        fixture: &Fixture,
        reference: &str,
        destination: &std::path::Path,
        extra: &[&str],
    ) -> CliOutcome {
        let destination = destination.to_str().unwrap();
        let mut args = vec![
            "session",
            "download",
            reference,
            "--to",
            destination,
            "--session",
            "session-1",
            "--host",
            "host-1",
        ];
        args.extend_from_slice(extra);
        fixture.run(&args).await
    }

    #[test]
    fn download_reference_preserves_scope_and_rejects_an_empty_value() {
        let target = SessionKey::new("host-1", "session-1").unwrap();
        let request = download_request(
            &target,
            "attachment-1",
            std::path::PathBuf::from("downloads"),
            true,
            Some("chosen.bin"),
        )
        .unwrap();
        assert_eq!(request.reference.session, target);
        assert_eq!(request.reference.attachment_ref, "attachment-1");
        assert_eq!(request.destination, std::path::PathBuf::from("downloads"));
        assert_eq!(request.file_name.as_deref(), Some("chosen.bin"));
        assert!(request.overwrite);
        let suggested = download_request(
            &target,
            "attachment-2",
            std::path::PathBuf::from("d"),
            false,
            None,
        )
        .unwrap();
        assert!(suggested.file_name.is_none());
        assert!(!suggested.overwrite);
        for empty in ["", " ", "\t"] {
            let error =
                download_request(&target, empty, std::path::PathBuf::from("d"), false, None)
                    .unwrap_err();
            assert_eq!(error.code(), "invalid_argument");
        }
    }

    #[test]
    fn download_limits_follow_the_overall_deadline() {
        assert_eq!(download_limits(None).unwrap(), AttachmentLimits::default());
        let short = download_limits(Some("0.5")).unwrap();
        assert_eq!(short.overall_timeout, Duration::from_millis(500));
        assert_eq!(short.idle_timeout, Duration::from_millis(500));
        assert!(download_limits(Some("0")).is_err());
        assert!(download_limits(Some("-1")).is_err());
        assert!(download_limits(Some("soon")).is_err());
    }

    #[tokio::test]
    async fn download_saves_verified_bytes_and_maps_each_failure_without_partial_files() {
        let f = Fixture::paired().await;
        let directory = f.directory.path().join("downloads");
        std::fs::create_dir(&directory).unwrap();
        f.access.host.lock().unwrap().put_attachment(attachment(
            "attachment-1",
            "hello.txt",
            b"hello",
        ));

        let saved = run_download(&f, "attachment-1", &directory, &[]).await;
        assert_eq!(saved.exit, 0, "{saved:?}");
        let value = saved.to_json();
        let path = directory.join("hello.txt");
        assert_eq!(value["data"]["reference"], "attachment-1");
        assert_eq!(value["data"]["name"], "hello.txt");
        assert_eq!(value["data"]["bytes"], 5);
        assert_eq!(value["data"]["verified"], true);
        assert_eq!(value["data"]["path"], path.to_str().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(saved.human.contains("Verified: yes"));
        assert!(!saved.human.contains("attachment-1"));

        // A collision preserves the existing bytes until --force is explicit.
        std::fs::write(&path, b"preserved").unwrap();
        let collision = run_download(&f, "attachment-1", &directory, &[]).await;
        assert_eq!(collision.exit, 8, "{collision:?}");
        assert_eq!(collision.to_json()["error"]["code"], "conflict");
        assert!(collision.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--force"));
        assert_eq!(std::fs::read(&path).unwrap(), b"preserved");
        let forced = run_download(&f, "attachment-1", &directory, &["--force"]).await;
        assert_eq!(forced.exit, 0, "{forced:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");

        // An explicit name replaces only the user-selected final component.
        let named = run_download(
            &f,
            "attachment-1",
            &directory,
            &["--name", "renamed.bin", "--force"],
        )
        .await;
        assert_eq!(named.exit, 0, "{named:?}");
        assert_eq!(
            std::fs::read(directory.join("renamed.bin")).unwrap(),
            b"hello"
        );

        // Unsupported and oversize reads fail before any staging file exists.
        f.access.host.lock().unwrap().attachment_unsupported = true;
        let unsupported = run_download(&f, "attachment-1", &directory, &[]).await;
        assert_eq!(unsupported.exit, 9, "{unsupported:?}");
        assert_eq!(
            unsupported.to_json()["error"]["code"],
            "unsupported_capability"
        );
        assert!(unsupported.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session history"));
        f.access.host.lock().unwrap().attachment_unsupported = false;

        let mut oversized = attachment("attachment-big", "big.bin", b"abcdef");
        oversized.policy.max_total_bytes = 2;
        f.access.host.lock().unwrap().put_attachment(oversized);
        let over = run_download(&f, "attachment-big", &directory, &[]).await;
        assert_eq!(over.exit, 5, "{over:?}");
        assert_eq!(over.to_json()["error"]["code"], "payload_too_large");
        assert!(!directory.join("big.bin").exists());

        // The reference belongs to one session; reuse from another session fails
        // closed with no local write.
        let wrong_scope = f
            .run(&[
                "session",
                "download",
                "attachment-1",
                "--to",
                directory.to_str().unwrap(),
                "--session",
                "session-2",
                "--host",
                "host-1",
            ])
            .await;
        assert_eq!(wrong_scope.exit, 4, "{wrong_scope:?}");
        assert_eq!(wrong_scope.to_json()["error"]["code"], "forbidden");
        assert!(wrong_scope.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("whalerein status"));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);

        // A Source-suggested unsafe name is rejected before staging and names --name.
        f.access.host.lock().unwrap().put_attachment(attachment(
            "attachment-unsafe",
            "../escape.txt",
            b"escape",
        ));
        let unsafe_name = run_download(&f, "attachment-unsafe", &directory, &[]).await;
        assert_eq!(unsafe_name.exit, 2, "{unsafe_name:?}");
        assert_eq!(unsafe_name.to_json()["error"]["code"], "invalid_argument");
        assert!(unsafe_name.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--name"));
        assert!(!directory.join("escape.txt").exists());
        assert!(!f.directory.path().join("escape.txt").exists());

        // Missing destination and empty references are local argument failures.
        let missing =
            run_download(&f, "attachment-1", &f.directory.path().join("missing"), &[]).await;
        assert_eq!(missing.exit, 2, "{missing:?}");
        assert!(missing.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--to <DIR>"));
        let empty = run_download(&f, " ", &directory, &[]).await;
        assert_eq!(empty.exit, 2, "{empty:?}");
        assert_eq!(empty.to_json()["error"]["code"], "invalid_argument");

        // A short overall deadline fails with no completed file.
        f.access.host.lock().unwrap().attachment_open_delay_ms = 500;
        let timed_out = run_download(&f, "attachment-1", &directory, &["--timeout", "0.1"]).await;
        assert_eq!(timed_out.exit, 7, "{timed_out:?}");
        assert_eq!(timed_out.to_json()["error"]["code"], "timeout");
        assert!(timed_out.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fresh transfer"));
        f.access.host.lock().unwrap().attachment_open_delay_ms = 0;
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn interrupted_download_cancels_core_and_removes_staging() {
        let f = Fixture::paired().await;
        let directory = f.directory.path().join("downloads");
        std::fs::create_dir(&directory).unwrap();
        f.access.host.lock().unwrap().put_attachment(attachment(
            "attachment-1",
            "hello.txt",
            b"hello",
        ));
        f.access.host.lock().unwrap().attachment_open_delay_ms = 500;

        let cancelled = Box::pin(f.run_with_interrupt(
            &[
                "session",
                "download",
                "attachment-1",
                "--to",
                directory.to_str().unwrap(),
                "--session",
                "session-1",
                "--host",
                "host-1",
            ],
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
        ))
        .await;
        assert_eq!(cancelled.exit, 2, "{cancelled:?}");
        assert_eq!(cancelled.to_json()["error"]["code"], "cancelled");
        assert!(cancelled.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fresh transfer"));
        assert!(!directory.join("hello.txt").exists());
        assert_eq!(
            std::fs::read_dir(&directory).unwrap().count(),
            0,
            "staging data must be removed"
        );
        assert_eq!(f.access.host.lock().unwrap().attachment_close_calls, 0);
    }
}
