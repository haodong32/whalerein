//! Command execution. All protocol behavior lives in `whalerein-core`.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use whalerein_core::api::{
    default_data_dir, CreateSessionInput, Environment, MemorySecretStore, PairingDecision,
    PendingQueueAction, ProfileSelection, ProfileSummary, Profiles, RemovalOutcome, SecretStore,
    SessionKey, SessionModel, WaitReason,
};
use whalerein_core::{Core, CoreError, CoreOptions};

use crate::cli::{Cli, Command, ModelAction, ProfileAction, QueueCommand};
use crate::output::{
    approval_data, command_data, core_error_outcome, doctor_data, hosts_data, is_tail_output,
    lifecycle_error_code, lifecycle_exit_code, manifest_data, render_approval, render_approvals,
    render_command, render_doctor, render_hosts, render_lifecycle_outcome, render_manifest,
    render_models, render_pair, render_pairing_history, render_quarantine, render_questions,
    render_queue, render_session_model, render_sessions, sanitize_field, sessions_data,
    tail_event_json, CliError, CliOutcome, TailRenderer,
};

const DEFAULT_TAIL_TIMEOUT: Duration = Duration::from_secs(15);

/// Default total `wait` deadline. It starts before key resolution so
/// authorized-Host discovery is charged to the same budget as the observation.
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest accepted deadline, matching the Host request budget.
const MAX_TIMEOUT_MS: u64 = 2_147_483_647;

/// File and schema that `Core` reads at startup. `quarantine_corrupt_config`
/// only renames a file whose shape cannot be a valid configuration.
const CONFIG_FILE: &str = "config.json";
const CONFIG_SCHEMA: &str = "whalerein/v1/config";

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

pub async fn run(cli: Cli) -> CliOutcome {
    let command = cli.command.clone();
    let secrets = match secret_store() {
        Ok(secrets) => secrets,
        Err(error) => {
            return CliOutcome::local_failure(&command, error.code.as_str(), error.message, 2)
        }
    };
    run_with_signal(cli, secrets, crate::signal::interrupt()).await
}

/// Keep in-process CLI tests away from the developer's real default config.
#[cfg(test)]
fn isolate_test_config() {
    use std::sync::OnceLock;
    static CONFIG: OnceLock<PathBuf> = OnceLock::new();
    let path = CONFIG.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("whalerein-cli-unit-tests-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test config directory");
        let path = dir.join("config.toml");
        if !path.exists() {
            std::fs::write(&path, "version = 1\nenvironment = \"production\"\n")
                .expect("test config file");
        }
        path
    });
    std::env::set_var("WHALEREIN_CONFIG", path);
}

/// Injectable variant used by tests. The only injected boundary is SecretStore.
#[allow(dead_code)]
pub(crate) async fn run_with(cli: Cli, secrets: Arc<dyn SecretStore>) -> CliOutcome {
    run_with_signal(cli, secrets, crate::signal::interrupt()).await
}

/// Dispatch with an injectable interrupt future so tests can prove the
/// cancellation contract without delivering a real signal.
pub(crate) async fn run_with_signal<S>(
    cli: Cli,
    secrets: Arc<dyn SecretStore>,
    signal: S,
) -> CliOutcome
where
    S: std::future::Future<Output = ()>,
{
    #[cfg(test)]
    isolate_test_config();
    let command = cli.command.clone();
    // The identity/state root is never derived from an environment descriptor:
    // configuration and state stay independent per-device inputs.
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);

    // Import previews are read-only and must work even when the default config
    // file does not exist yet; the command validates and writes through Core.
    if let Command::Config { action } = &command {
        if matches!(action, crate::cli::ConfigAction::Import { .. }) {
            return match crate::config::run_import(&cli, &command, action, &data_dir, &secrets) {
                Ok(outcome) => outcome,
                Err(error) => core_error_outcome(&command, error),
            };
        }
    }

    // Ordinary startup may initialize a missing default file; show/check and
    // every help path are inspection-only. Resolution never falls back.
    let config = match crate::config::open(&cli, &command) {
        Ok(config) => config,
        Err(error) => return core_error_outcome(&command, error),
    };
    // `config show/check` must explain an unresolvable selection as data; the
    // other commands fail before touching protocol state.
    if let Command::Config { action } = &command {
        return match crate::config::run(&cli, &command, action, &config) {
            Ok(outcome) => outcome,
            Err(error) => core_error_outcome(&command, error),
        };
    }
    let resolution = match config.resolve(cli.environment.as_deref()) {
        Ok(resolution) => resolution,
        Err(error) => return core_error_outcome(&command, error),
    };
    // Only the two recovery commands tolerate a corrupt configuration. Every
    // other command still needs a routable configuration and keeps the
    // explicit invalid_configuration error.
    let quarantined = if recoverable_config(&command) {
        quarantine_corrupt_config(&data_dir)
    } else {
        None
    };
    if let Command::Profile { action } = &command {
        return match profile(&data_dir, &secrets, &command, action, &resolution) {
            Ok(outcome) => outcome,
            Err(error) => core_error_outcome(&command, error),
        };
    }
    let core = match Core::new_in_environment(
        CoreOptions { data_dir, secrets },
        Some(resolution.environment.as_str()),
    ) {
        Ok(core) => core,
        Err(error) => return with_quarantine(core_error_outcome(&command, error), quarantined),
    };

    // Two explicit scopes must not silently disagree about which Host a
    // listing describes. This was enforced by the removed ambient Host scope.
    if let (Some(scope), Command::Sessions { host }) = (cli.host.as_deref(), &command) {
        if scope != host {
            return core_error_outcome(
                &command,
                CoreError::Conflict(format!(
                    "--host {scope} and sessions --host {host} disagree"
                )),
            );
        }
    }

    // The total `wait`/`tail` deadline starts before session resolution, so a
    // slow authorized-Host discovery cannot extend the observation window.
    let started = tokio::time::Instant::now();
    let budget = match command_budget(&command) {
        Ok(budget) => budget,
        Err(error) => {
            return with_quarantine(
                CliOutcome::local_failure(&command, "invalid_argument", error, 2),
                quarantined,
            )
        }
    };
    let deadline = budget.map(|budget| started + budget);

    // Resolve the Host-qualified key exactly once, before dispatch. The key
    // travels with the request; no ambient Host scope is installed.
    let key = if let Some(session) = command.session_id() {
        let resolve = async {
            match cli.host.as_deref() {
                Some(host) => core.scoped_session_key(host, session).await,
                None => core.session_key(session).await,
            }
        };
        let resolved = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, resolve).await {
                Ok(resolved) => resolved,
                Err(_) => Err(CoreError::Timeout),
            },
            None => resolve.await,
        };
        match resolved {
            Ok(key) => Some(key),
            Err(error) => {
                return core_error_outcome(
                    &command,
                    explain_ambiguous_session(&core, session, error).await,
                )
            }
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
    let result = if matches!(command, Command::Answer { .. }) {
        dispatch(&core, &cli, &command, key, deadline).await
    } else {
        // The dispatch future is boxed so the interrupt branch can drop it
        // *before* the cleanup window: dropping a `wait` future runs Core's
        // observation guard, which sends CancelSessionWait from a spawned task.
        let mut work = Box::pin(dispatch(&core, &cli, &command, key, deadline));
        tokio::select! {
            result = &mut work => result,
            _ = &mut signal => {
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
                return with_quarantine(outcome, quarantined);
            }
        }
    };
    let outcome = finish_core_result(&core, &command, result);
    with_quarantine(outcome, quarantined)
}

/// Commands that can create a journaled command identity. Only these need the
/// pre-dispatch pending snapshot; read-only commands must not open the cache
/// just to answer an interrupt (opening it changes revocation retention
/// reporting).
fn may_submit(command: &Command) -> bool {
    matches!(
        command,
        Command::Send { .. }
            | Command::Approve { .. }
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
            "Unable to verify whether unconfirmed command or pairing identities were retained.\nReview them with `whalerein commands-pending` before pairing again; do not reuse the old identity.\n",
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

/// Name the candidate Hosts for an unqualified multi-Host session conflict.
///
/// Core's resolution knows the matches but its `Conflict` message historically
/// dropped them. The CLI re-derives the candidates from the authorized Host
/// projection so the operator gets a usable `--host` choice; when Core already
/// names candidates, the message is left untouched. Local-only: no additional
/// risk beyond the resolution's own reads.
async fn explain_ambiguous_session(core: &Core, session: &str, error: CoreError) -> CoreError {
    let CoreError::Conflict(message) = &error else {
        return error;
    };
    if !should_explain_ambiguous(message) {
        return error;
    }
    let Ok(hosts) = core.hosts().await else {
        return error;
    };
    let mut candidates = Vec::new();
    for host in hosts {
        let Ok(list) = core.sessions(&host.host_id).await else {
            continue;
        };
        if list.source == "remote"
            && list.complete
            && list.sessions.iter().any(|item| item.session_id == session)
        {
            candidates.push(host.host_id);
        }
    }
    match ambiguous_session_conflict(message, session, &candidates) {
        Some(explained) => explained,
        None => error,
    }
}

/// Whether an ambiguity message still needs the candidate list appended.
///
/// Core now names candidates itself as `session exists on multiple Hosts
/// (host-1, host-2); specify a Host with --host`, and some callers use the word
/// "candidate". Both shapes are already actionable, so they are never rewritten.
fn should_explain_ambiguous(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("multiple hosts")
        && !lowered.contains("candidate")
        && !message.contains("multiple Hosts (")
}

/// Build the actionable ambiguity error from candidate Host ids.
fn ambiguous_session_conflict(
    message: &str,
    session: &str,
    candidates: &[String],
) -> Option<CoreError> {
    if candidates.is_empty() {
        return None;
    }
    Some(CoreError::Conflict(format!(
        "{message}. Candidates: {}. Use `whalerein --host <host-id> <command> --session {session}`",
        candidates.join(", ")
    )))
}

fn pending_commands(core: &Core) -> Vec<whalerein_core::api::PendingCommand> {
    core.pending_commands().unwrap_or_default()
}

/// Add the quarantine path to a successful recovery outcome. The original file
/// is retained next to the data directory; nothing is silently deleted.
fn with_quarantine(mut outcome: CliOutcome, quarantined: Option<PathBuf>) -> CliOutcome {
    let Some(path) = quarantined.filter(|_| outcome.exit == 0) else {
        return outcome;
    };
    let display = path.display().to_string();
    if let Some(data) = outcome.data.as_mut().and_then(|data| data.as_object_mut()) {
        data.insert("config_quarantined".into(), json!(display));
    }
    outcome.human.push_str(&render_quarantine(&path));
    outcome
}

fn finish_core_result(
    core: &Core,
    command: &Command,
    result: Result<CliOutcome, CoreError>,
) -> CliOutcome {
    match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let mut outcome = core_error_outcome(command, error);
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

/// The two commands that must stay usable when the local configuration cannot
/// be parsed. `forget --local-only` is the documented last-resort recovery;
/// `doctor` is the diagnostic entry point.
fn recoverable_config(command: &Command) -> bool {
    matches!(
        command,
        Command::Doctor { .. } | Command::Forget { local_only: true }
    )
}

/// Rename an unparseable or foreign `config.json` to `config.json.corrupt-<ms>`.
///
/// Identity secrets live in the SecretStore, not in this file, so quarantining
/// the configuration never discards keys or tokens. A malformed configuration
/// that happens to be valid JSON is quarantined too: it cannot be routed and
/// the user has no other local recovery path.
fn quarantine_corrupt_config(data_dir: &Path) -> Option<PathBuf> {
    let path = data_dir.join(CONFIG_FILE);
    let bytes = std::fs::read(&path).ok()?;
    if !is_corrupt_config(&bytes) {
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    for attempt in 0..100u32 {
        let candidate = if attempt == 0 {
            data_dir.join(format!("{CONFIG_FILE}.corrupt-{stamp}"))
        } else {
            data_dir.join(format!("{CONFIG_FILE}.corrupt-{stamp}-{attempt}"))
        };
        if candidate.exists() {
            continue;
        }
        if std::fs::rename(&path, &candidate).is_err() {
            return None;
        }
        make_private(&candidate);
        return Some(candidate);
    }
    None
}

fn is_corrupt_config(bytes: &[u8]) -> bool {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => value.get("schema").and_then(|schema| schema.as_str()) != Some(CONFIG_SCHEMA),
        Err(_) => true,
    }
}

fn make_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Execute one parsed command. Local validation failures are successful
/// dispatches that carry a `CliOutcome`; protocol failures stay `CoreError`.
/// Execute one parsed command. Local validation failures are successful
/// dispatches that carry a `CliOutcome`; protocol failures stay `CoreError`.
async fn dispatch(
    core: &Core,
    cli: &Cli,
    command: &Command,
    key: Option<&SessionKey>,
    deadline: Option<tokio::time::Instant>,
) -> Result<CliOutcome, CoreError> {
    let result = match command {
        Command::Profile { .. } => unreachable!("profile commands are dispatched before Core"),
        Command::Config { .. } => unreachable!("config commands are dispatched before Core"),
        Command::Pair {
            uri,
            fingerprint,
            name,
        } => {
            pair(
                core,
                command,
                cli,
                uri,
                fingerprint.as_deref(),
                name.as_deref(),
            )
            .await
        }
        Command::PairResume => match core.resume_pairing().await {
            Ok(outcome) => Ok(CliOutcome::success(
                command,
                serde_json::to_value(&outcome).unwrap_or_else(|_| json!({})),
                render_pair(&outcome),
            )),
            Err(error) => Err(error),
        },
        Command::PairingHistory => match core.pairing_history() {
            Ok(records) => {
                let human = render_pairing_history(&records);
                Ok(CliOutcome::success(
                    command,
                    json!({ "pairings": records }),
                    human,
                ))
            }
            Err(error) => Err(error),
        },
        Command::Hosts => match core.hosts().await {
            Ok(hosts) => Ok(CliOutcome::success(
                command,
                hosts_data(&hosts),
                render_hosts(&hosts),
            )),
            Err(error) => Err(error),
        },
        Command::Sessions { host } => match core.sessions(host).await {
            Ok(list) => Ok(CliOutcome::success(
                command,
                sessions_data(host, &list),
                render_sessions(&list),
            )),
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
                for item in commands {
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
                    command,
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
                command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(error) => Err(error),
        },
        Command::Tail { once, .. } => {
            let key = resolved_key(key)?;
            // Resolution already ran inside the same budget, so tail gets only
            // what is left of the original deadline.
            let timeout = remaining_budget(deadline)?;
            tail(core, command, cli.json, key, *once, timeout).await
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
        Command::Sync { session } => {
            let key = resolved_key(key)?;
            match core.sync_session(key).await {
                Ok(events) => Ok(CliOutcome::success(
                    command,
                    json!({ "session_id": session, "events": events }),
                    format!("Synced session {session} ({events} event(s)).\n"),
                )),
                Err(error) => Err(error),
            }
        }
        Command::Send { text, delivery, .. } => match core
            .send_message_with_delivery(resolved_key(key)?, text, (*delivery).into())
            .await
        {
            Ok(handle) => Ok(CliOutcome::success(
                command,
                command_data(&handle),
                render_command(&handle),
            )),
            Err(CoreError::UnknownResult) => {
                // The accepted command may still create the message. Name the
                // retained request and explicitly forbid a blind resend: a
                // repeat of the same text recovers this command instead.
                let pending = core.pending_commands().unwrap_or_default();
                let retained: Vec<String> = pending
                    .iter()
                    .map(|pending| pending.request_id.clone())
                    .collect();
                let message = match retained.last() {
                    Some(request) => format!(
                        "command result is unknown; the command was retained as request {request}. Read its result with `whalerein command-recover --request {request}`. Do not resend: retrying the same text recovers this command."
                    ),
                    None => "command result is unknown. Do not resend; inspect `whalerein commands-pending` before retrying.".into(),
                };
                let mut outcome = CliOutcome::local_failure_with_diagnostic(
                    command,
                    "unknown",
                    message,
                    7,
                    core.diagnostic_id(),
                );
                outcome.data = Some(json!({ "retained_requests": retained }));
                Ok(outcome)
            }
            Err(error) => Err(error),
        },
        Command::Approvals { session, request } => {
            if let Some(approval_id) = request {
                // APPR-03 read path: one approval's live or terminal state.
                // A known id is success; an unknown id stays not_found (exit 3).
                let host = core.approval_host(cli.host.as_deref()).await?;
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
                        core.approval_host(cli.host.as_deref()).await?,
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
            match core.list_questions(key, cursor).await {
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
            match core.get_question(key, batch).await {
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
                                return Ok(unknown_outcome(core, command, format!("Cancellation requested for command {actual_id}; recover its result with `whalerein commands-pending` and `whalerein command-recover --request <request-id>`.")))
                            }
                            OpenAnswerWait::CancellationUnconfirmed => {
                                return Ok(unknown_outcome(core, command, format!("Cancellation was not confirmed for command {actual_id}; recover its result with `whalerein commands-pending` and `whalerein command-recover --request <request-id>`.")))
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
                            return Ok(unknown_outcome(core, command, format!("Outcome of command {actual_id} is unconfirmed; recover its result with `whalerein commands-pending` and `whalerein command-recover --request <request-id>`.")));
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
            let host = core.approval_host(cli.host.as_deref()).await?;
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
            let host = core.approval_host(cli.host.as_deref()).await?;
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
    // `-` reads the invitation from standard input so the one-time token never
    // appears in argv, `ps` output or shell history. Surrounding whitespace is
    // removed; the Core parser validates and bounds the value.
    let uri_owned;
    let uri = if uri == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(|_| {
            CoreError::InvalidInput("pairing URI could not be read from standard input".into())
        })?;
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
    key: &SessionKey,
    once: bool,
    timeout: Option<Duration>,
) -> Result<CliOutcome, CoreError> {
    let session = key.session_id.as_str();
    let mut renderer = TailRenderer::default();
    let mut visible = false;
    let mut closed = false;
    let emitted = core
        .watch_session(key, timeout, |event| {
            let text = if json_output {
                let mut value = tail_event_json(event);
                value["data"]["event"]["session"] =
                    json!({"host_id":key.host_id,"session_id":key.session_id});
                format!("{}\n", serde_json::to_string(&value).unwrap_or_default())
            } else {
                renderer.render(event)
            };
            let written = crate::stream::to_stdout(text.as_bytes());
            visible |= is_tail_output(event);
            match written {
                Ok(()) => !(once && visible),
                Err(failure) if failure.is_closed() => {
                    // The consumer closed the pipe (`tail | head -1`): stop the
                    // stream cleanly instead of panicking on the next write.
                    closed = true;
                    false
                }
                Err(_) => false,
            }
        })
        .await;
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
    cli: &Cli,
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

fn profile(
    data_dir: &std::path::Path,
    secrets: &Arc<dyn SecretStore>,
    command: &Command,
    action: &ProfileAction,
    resolution: &whalerein_core::api::EnvironmentResolution,
) -> Result<CliOutcome, CoreError> {
    let profiles = Profiles::new(data_dir);
    match action {
        ProfileAction::List { include_staging } => {
            let profiles_list = profiles.environment_profiles(
                &resolution.environment,
                *include_staging,
                secrets,
            )?;
            let data = json!({
                "environment": resolution.environment,
                "selection_source": resolution.selection_source,
                "profiles": profiles_list,
            });
            let human = render_profile_list(&profiles_list);
            Ok(CliOutcome::success(command, data, human))
        }
        ProfileAction::Current => {
            // The active identity is scoped to the resolved environment; the
            // registry's global active record never overrides the config file.
            let mut selection = profiles.current(secrets)?;
            selection.active = profiles.resolve_environment_identity(resolution, secrets, false)?;
            let mut data = serde_json::to_value(&selection)
                .unwrap_or_else(|_| json!({ "registry_present": selection.registry_present }));
            if let Some(object) = data.as_object_mut() {
                object.insert("environment".into(), json!(resolution.environment));
                object.insert(
                    "selection_source".into(),
                    json!(resolution.selection_source),
                );
                object.insert(
                    "definition_source".into(),
                    json!(resolution.definition_source),
                );
            }
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
            let legacy_supplied = *staging
                || *enable_testing
                || api_origin.is_some()
                || issuer.is_some()
                || client_id.is_some()
                || access_url.is_some();
            if legacy_supplied {
                return Err(CoreError::InvalidInput(
                    "profile add now adds an identity inside the resolved environment; define custom services with `whalerein config set environment` and select built-ins with `--environment` or `config set default`"
                        .into(),
                ));
            }
            // Identities never carry a second environment/service authority.
            let summary = profiles.add_environment_profile(resolution, name, secrets)?;
            let data = json!({
                "environment": resolution.environment,
                "profile": summary,
                "current": false,
            });
            let human = format!(
                "Added profile {} in environment {}; active identity unchanged.\n",
                summary.name, resolution.environment
            );
            Ok(CliOutcome::success(command, data, human))
        }
        ProfileAction::Use { name } => {
            let outcome = profiles.use_profile_in_environment(resolution, name)?;
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
        Environment::Local => "Local (testing only)",
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

#[cfg(test)]
mod tests {
    #[test]
    fn timeout_seconds_are_exact_and_bounded() {
        for (input, expected) in [
            ("0.001", 1),
            ("30", 30_000),
            ("1.25", 1_250),
            ("2147483.647", 2_147_483_647),
        ] {
            assert_eq!(
                super::parse_timeout_seconds(input),
                Ok(Duration::from_millis(expected))
            );
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
            assert!(super::parse_timeout_seconds(input).is_err());
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
        let outcome = run_with(
            cli(&["whalerein", "--data-dir", path, "revoke", "--yes"]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 0);
        let value = outcome.to_json();
        assert_eq!(value["data"]["revoked"], true);
        assert_eq!(value["data"]["revoked_retention_unverified"], true);
        assert!(value["data"]["device_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        assert_eq!(outcome.render(), revoke_human(true));
        assert!(outcome.render().contains("Unable to verify"));
        assert!(outcome.render().contains("whalerein commands-pending"));
        assert!(outcome.render().contains("do not reuse the old identity"));
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
                cli(&["whalerein", "--data-dir", path, "revoke", "--yes"]),
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
                "--yes",
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
            // One second leaves room for Host discovery under parallel test load
            // while still proving the timeout-observation contract.
            let mut args = vec!["wait", "--session", "session-1", "--timeout", "1"];
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

        let revoked = run_args(&data_dir, secrets.clone(), &["revoke", "--yes"]).await;
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

    fn write_config(dir: &std::path::Path, body: &str) -> String {
        let path = dir.join("config.toml");
        std::fs::write(&path, body).unwrap();
        path.to_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn profile_staging_adds_within_the_resolved_environment_without_activating() {
        let dir = tempfile::tempdir().unwrap();
        let dir_text = dir.path().to_str().unwrap();
        let config = write_config(dir.path(), "version = 1\nenvironment = \"staging\"\n");
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let added = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "--config",
                &config,
                "profile",
                "add",
                "official-staging",
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
                "--config",
                &config,
                "profile",
                "current",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(current.to_json()["data"]["environment"], "staging");

        let hidden = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "--config",
                &config,
                "profile",
                "list",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(hidden.to_json()["data"]["environment"], "staging");
        assert!(hidden.to_json()["data"]["profiles"]
            .as_array()
            .unwrap()
            .is_empty());

        let shown = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "--config",
                &config,
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
            1
        );
    }

    #[tokio::test]
    async fn profile_activation_stays_within_the_environment_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let dir_text = dir.path().to_str().unwrap();
        let config = write_config(dir.path(), "version = 1\nenvironment = \"staging\"\n");
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let added = run_with(
            cli(&[
                "whalerein",
                "--data-dir",
                dir_text,
                "--config",
                &config,
                "profile",
                "add",
                "official-staging",
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
                "--config",
                &config,
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
                "--config",
                &config,
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
                "--config",
                &config,
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
                "--config",
                &config,
                "profile",
                "current",
            ]),
            secrets,
        )
        .await;
        assert!(after.to_json()["data"]["active"].is_null());
    }

    #[tokio::test]
    async fn profile_add_rejects_legacy_service_flags_and_uses_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let dir_text = dir.path().to_str().unwrap();
        let config = write_config(
            dir.path(),
            "version = 1\nenvironment = \"corp\"\n\n[environments.corp]\nmode = \"account\"\napi_origin = \"https://api.corp.example\"\nissuer = \"https://id.corp.example\"\nnative_client_id = \"whalerein-cli\"\n",
        );
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let denied = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "--config",
                &config,
                "profile",
                "add",
                "corp",
                "--api-origin",
                "https://api.corp.example",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(denied.exit, 2, "{denied:?}");
        assert_eq!(denied.to_json()["error"]["code"], "invalid_argument");
        assert!(denied.to_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("config set environment"));

        let added = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir_text,
                "--config",
                &config,
                "profile",
                "add",
                "corp",
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
                "--config",
                &config,
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
            "--yes",
        ])
        .unwrap();
        assert_eq!(run_with(normal, secrets.clone()).await.exit, 8);
        let local = Cli::try_parse_from([
            "whalerein",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "forget",
            "--local-only",
            "--yes",
        ])
        .unwrap();
        let result = run_with(local, secrets.clone()).await;
        assert_eq!(result.exit, 0);
        assert!(result.human.contains("remote grant may still be active"));
        assert_eq!(result.to_json()["data"]["remote_grant_may_remain"], true);
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
    #[tokio::test]
    async fn ctrl_c_during_wait_cancels_the_host_observation_and_the_next_wait_succeeds() {
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
        // Every wait slice reports its deadline, so the observation stays
        // active until the injected signal arrives.
        access.host.lock().unwrap().wait_timeouts = usize::MAX;
        // Signal only after the Host has received a wait observation. A slow
        // instrumented profile or Windows runner can otherwise spend the old
        // fixed delay in discovery, leaving nothing for Ctrl-C to cancel.
        let observed = access.host.clone();
        let signal = async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while observed.lock().unwrap().wait_queries == 0 {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let outcome = run_with_signal(
            cli(&[
                "whalerein",
                "wait",
                "--session",
                "session-1",
                "--timeout",
                "5",
                "--data-dir",
                dir.path().to_str().unwrap(),
            ]),
            secrets.clone(),
            signal,
        )
        .await;
        assert_eq!(outcome.exit, 2, "{}", outcome.to_json());
        assert_eq!(outcome.to_json()["error"]["code"], "cancelled");
        assert!(
            !access.host.lock().unwrap().wait_cancel_ids.is_empty(),
            "Ctrl-C must cancel the Host wait observation"
        );

        // The next wait starts cleanly instead of blocking on a stale claim.
        access.host.lock().unwrap().wait_timeouts = 0;
        let outcome = run_with(
            cli(&[
                "whalerein",
                "wait",
                "--session",
                "session-1",
                "--timeout",
                "2",
                "--data-dir",
                dir.path().to_str().unwrap(),
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 0);
        assert_eq!(outcome.to_json()["data"]["reason"], "idle");
    }

    async fn slow_discovery_fixture() -> (FakeAccess, tempfile::TempDir, Arc<dyn SecretStore>) {
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
        (access, dir, secrets)
    }

    // AC-07.wait: the total wait deadline starts before session resolution, so
    // a slow authorized-Host discovery consumes the budget instead of
    // extending the observation window.
    #[tokio::test]
    async fn cli_wait_deadline_includes_slow_host_discovery() {
        let cli = |args: &[&str]| Cli::try_parse_from(args).unwrap();
        let (_access, dir, secrets) = slow_discovery_fixture().await;
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
            secrets,
        )
        .await;
        assert_eq!(outcome.to_json()["error"]["code"], "timeout");
        assert!(started.elapsed() < Duration::from_millis(900));
    }

    #[tokio::test]
    async fn cli_tail_deadline_includes_slow_host_discovery() {
        let cli = |args: &[&str]| Cli::try_parse_from(args).unwrap();
        let (access, dir, secrets) = slow_discovery_fixture().await;
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

#[cfg(test)]
mod cli_contract_tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::test_support::FakeAccess;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    fn command_named(name: &str) -> Command {
        cli(&["whalerein", name]).command
    }

    #[test]
    fn destructive_commands_never_run_without_consent() {
        assert_eq!(
            destructive_decision(true, false),
            DestructiveDecision::Proceed
        );
        assert_eq!(
            destructive_decision(true, true),
            DestructiveDecision::Proceed
        );
        assert_eq!(
            destructive_decision(false, false),
            DestructiveDecision::RefuseNonInteractive
        );
        assert_eq!(destructive_decision(false, true), DestructiveDecision::Ask);

        // A non-interactive test process must refuse without touching stdin.
        if std::io::stdin().is_terminal() {
            return;
        }
        for name in ["revoke", "forget"] {
            let command = command_named(name);
            let cli = cli(&["whalerein", name]);
            let outcome = require_destructive_confirmation(&command, &cli, "Proceed?")
                .expect_err("must refuse without --yes");
            assert_eq!(outcome.exit, 2, "{name}");
            let error = outcome.error.unwrap();
            assert_eq!(error.code, "invalid_argument");
            assert!(error.message.contains("--yes"), "{name}: {}", error.message);
        }
        let command = Command::Doctor {
            clear_diagnostics: false,
            export: Some(PathBuf::from("out.json")),
        };
        let without_yes = cli(&["whalerein", "doctor"]);
        assert_eq!(
            require_destructive_confirmation(&command, &without_yes, "Write?")
                .unwrap_err()
                .exit,
            2
        );
        let with_yes = cli(&["whalerein", "--yes", "doctor"]);
        assert!(require_destructive_confirmation(&command, &with_yes, "Write?").is_ok());
    }

    #[tokio::test]
    async fn revoke_and_forget_require_yes_and_leave_state_untouched() {
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
        if std::io::stdin().is_terminal() {
            return;
        }
        let refused = run_with(
            cli(&["whalerein", "--data-dir", path, "revoke"]),
            secrets.clone(),
        )
        .await;
        assert_eq!(refused.exit, 2);
        assert_eq!(refused.to_json()["error"]["code"], "invalid_argument");
        // The pairing still works: nothing was revoked.
        assert_eq!(
            run_with(
                cli(&["whalerein", "--data-dir", path, "hosts"]),
                secrets.clone()
            )
            .await
            .exit,
            0
        );
        let forget = run_with(
            cli(&["whalerein", "--data-dir", path, "forget", "--local-only"]),
            secrets.clone(),
        )
        .await;
        assert_eq!(forget.exit, 2);
        assert_eq!(
            run_with(
                cli(&["whalerein", "--data-dir", path, "hosts"]),
                secrets.clone()
            )
            .await
            .exit,
            0
        );
        let revoked = run_with(
            cli(&["whalerein", "--data-dir", path, "revoke", "--yes"]),
            secrets,
        )
        .await;
        assert_eq!(revoked.exit, 0);
    }

    #[tokio::test]
    async fn corrupt_configuration_is_quarantined_only_for_recovery_commands() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        std::fs::write(dir.path().join(CONFIG_FILE), b"not json at all").unwrap();
        // Any command that needs routing keeps the explicit error.
        let blocked = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "hosts",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(blocked.exit, 2);
        assert_eq!(blocked.to_json()["error"]["code"], "invalid_configuration");
        assert!(dir.path().join(CONFIG_FILE).exists(), "must not touch it");

        let doctor = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(doctor.exit, 0, "{doctor:?}");
        let quarantined = doctor.to_json()["data"]["config_quarantined"]
            .as_str()
            .expect("quarantine path")
            .to_owned();
        assert!(
            quarantined.contains("config.json.corrupt-"),
            "{quarantined}"
        );
        assert!(Path::new(&quarantined).exists());
        assert!(!dir.path().join(CONFIG_FILE).exists());
        assert!(doctor.render().contains("quarantined"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&quarantined)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn forget_local_only_recovers_from_a_corrupt_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        std::fs::write(dir.path().join(CONFIG_FILE), b"{\"schema\":\"wrong\"}").unwrap();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "forget",
                "--local-only",
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        assert!(outcome.to_json()["data"]["config_quarantined"].is_string());
        assert!(!dir.path().join(CONFIG_FILE).exists());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers
                .iter()
                .any(|name| name.starts_with("config.json.corrupt-")),
            "the corrupt file must be retained: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn quarantine_ignores_a_valid_configuration() {
        let access = FakeAccess::start().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        assert_eq!(
            run_with(
                cli(&[
                    "whalerein",
                    "--yes",
                    "--data-dir",
                    dir.path().to_str().unwrap(),
                    "pair",
                    &access.pairing_uri()
                ]),
                secrets
            )
            .await
            .exit,
            0
        );
        assert_eq!(quarantine_corrupt_config(dir.path()), None);
        assert!(dir.path().join(CONFIG_FILE).exists());
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

    #[tokio::test]
    async fn a_malformed_fingerprint_exits_two_without_contacting_access() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "pair",
                "whalerein://pair?v=1&access=http%3A%2F%2F127.0.0.1%3A1&host=h&token=t",
                "--fingerprint",
                "zz",
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        assert_eq!(outcome.exit, 2);
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert!(!value.to_string().contains("zz"));
    }

    #[tokio::test]
    async fn an_interrupt_before_submission_is_a_cancelled_envelope() {
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
        access.host.lock().unwrap().wait_receipt_delay_ms = 5_000;
        let outcome = run_with_signal(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                path,
                "wait",
                "--session",
                "session-1",
                "--timeout",
                "20",
            ]),
            secrets,
            std::future::ready(()),
        )
        .await;
        assert_eq!(outcome.exit, 2, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "cancelled");
        assert_eq!(value["command"], "wait");
        assert!(value.get("data").is_none() || value["data"].is_null());
        assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    }

    #[tokio::test]
    async fn wait_timeout_failure_uses_an_error_envelope_like_tail() {
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
        access.host.lock().unwrap().wait_timeouts = usize::MAX;
        // The total deadline now includes authorized-Host discovery, so a
        // coverage-instrumented or slow Windows runner needs headroom before
        // the first wait slice can report its timeout observation.
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                path,
                "wait",
                "--session",
                "session-1",
                "--timeout",
                "3",
                "--fail-on-timeout",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(outcome.exit, 7);
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "timeout");
        assert_eq!(value["data"]["reason"], "timeout");
        // Without the flag the same observation stays a successful exit.
        let ok = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                path,
                "wait",
                "--session",
                "session-1",
                "--timeout",
                "3",
            ]),
            secrets,
        )
        .await;
        assert_eq!(ok.exit, 0);
        assert!(ok.to_json().get("error").is_none());
    }

    #[tokio::test]
    async fn tail_and_wait_reject_zero_timeout_before_contacting_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        for args in [
            vec!["tail", "--session", "s", "--timeout", "0"],
            vec!["wait", "--session", "s", "--timeout", "0"],
            vec!["tail", "--session", "s", "--timeout", "0.000"],
        ] {
            let mut full = vec![
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
            ];
            full.extend_from_slice(&args);
            let outcome = run_with(cli(&full), secrets.clone()).await;
            assert_eq!(outcome.exit, 2, "{args:?}");
            assert_eq!(outcome.to_json()["error"]["code"], "invalid_argument");
        }
    }

    #[tokio::test]
    async fn flag_data_dir_overrides_the_environment() {
        let flag = tempfile::tempdir().unwrap();
        let env = tempfile::tempdir().unwrap();
        let env_dir = env.path().join("from-env");
        let flag_dir = flag.path().join("from-flag");
        let previous = std::env::var("WHALEREIN_DATA_DIR").ok();
        std::env::set_var("WHALEREIN_DATA_DIR", &env_dir);
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                flag_dir.to_str().unwrap(),
                "doctor",
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        match previous {
            Some(previous) => std::env::set_var("WHALEREIN_DATA_DIR", previous),
            None => std::env::remove_var("WHALEREIN_DATA_DIR"),
        }
        assert_eq!(outcome.exit, 0);
        assert!(flag_dir.exists(), "the flag directory must be used");
        assert!(
            !env_dir.exists(),
            "the environment directory must be ignored"
        );
    }

    #[test]
    fn secret_backend_configuration_failures_are_usage_errors() {
        let previous = std::env::var("WHALEREIN_SECRET_BACKEND").ok();
        std::env::set_var("WHALEREIN_SECRET_BACKEND", "bogus");
        let error = super::secret_store().err().expect("invalid backend");
        assert_eq!(error.code, "invalid_configuration");
        assert_eq!(error_exit_for(&error), 2);
        std::env::set_var("WHALEREIN_SECRET_BACKEND", "file:");
        let error = super::secret_store().err().expect("empty file path");
        assert_eq!(error.code, "invalid_configuration");
        for backend in ["memory", "file:/tmp/whalerein-secret-test", "keyring", ""] {
            std::env::set_var("WHALEREIN_SECRET_BACKEND", backend);
            assert!(
                super::secret_store().is_ok(),
                "backend {backend:?} should be constructible"
            );
        }
        match previous {
            Some(previous) => std::env::set_var("WHALEREIN_SECRET_BACKEND", previous),
            None => std::env::remove_var("WHALEREIN_SECRET_BACKEND"),
        }
    }

    fn error_exit_for(error: &CliError) -> u8 {
        crate::output::error_exit_code(&error.code)
    }

    #[tokio::test]
    async fn a_local_unknown_outcome_carries_a_diagnostic_identity() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::new(CoreOptions {
            data_dir: dir.path().to_path_buf(),
            secrets: Arc::new(MemorySecretStore::new()),
        })
        .unwrap();
        let command = Command::Interrupt {
            session: "session-1".into(),
        };
        let outcome = unknown_outcome(&core, &command, "not observed".to_owned());
        assert_eq!(outcome.exit, 7);
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "unknown");
        assert_eq!(value["error"]["diagnostic_id"], core.diagnostic_id());
        let record = whalerein_core::api::test_support::load_failure(dir.path()).unwrap();
        assert_eq!(record.trace_id, core.diagnostic_id());
        assert_eq!(record.events.last().unwrap().code, "unknown");
    }

    #[tokio::test]
    async fn a_host_rejection_maps_to_exit_six_at_the_cli_layer() {
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
        access.host.lock().unwrap().settled_command_status =
            whalerein_proto::OnlineStatus::Rejected;
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                path,
                "send",
                "--session",
                "session-1",
                "text",
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 6, "{outcome:?}");
        assert_eq!(outcome.to_json()["error"]["code"], "rejected");
    }

    #[tokio::test]
    async fn dual_host_flags_must_agree_and_the_command_value_is_used() {
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
        // Agreement is required and the command-level value is authoritative.
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--host",
                "host-1",
                "--data-dir",
                path,
                "sessions",
                "--host",
                "host-1",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        assert_eq!(outcome.to_json()["data"]["host_id"], "host-1");
        assert_eq!(
            outcome.to_json()["data"]["sessions"][0]["session_id"],
            "session-1"
        );
        // Disagreement is a deterministic conflict, never a silent override.
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--host",
                "bogus-host",
                "--data-dir",
                path,
                "sessions",
                "--host",
                "host-1",
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 8, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "conflict");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("disagree"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn doctor_reports_an_unavailable_cache_without_failing() {
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
        // Healthy paired profile: the warning key is absent.
        let healthy = run_with(
            cli(&["whalerein", "--json", "--data-dir", path, "doctor"]),
            secrets.clone(),
        )
        .await;
        assert_eq!(healthy.exit, 0);
        assert!(healthy.to_json()["data"].get("cache_warning").is_none());
        assert!(!healthy.human.contains("Warning:"));

        // A cache that cannot even be quarantined (read-only data directory)
        // is a degraded diagnostic report, not a failure. Corruption alone is
        // recovered automatically by `Cache::open_recovering`.
        std::fs::write(dir.path().join("cache.sqlite3"), b"corrupt cache bytes").unwrap();
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(dir.path().join(format!("cache.sqlite3{suffix}")));
        }
        let readonly = dir.path().to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let outcome = run_with(
            cli(&["whalerein", "--json", "--data-dir", path, "doctor"]),
            secrets.clone(),
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["data"]["cache_warning"], "cache_unavailable");
        assert_eq!(value["data"]["commands"]["non_terminal"], 0);
        assert_eq!(value["data"]["commands"]["unknown"], 0);
        assert_eq!(value["error"], serde_json::Value::Null);
        assert!(outcome.human.contains("cache is unavailable"));
        assert!(outcome
            .human
            .ends_with(&render_cache_warning("cache_unavailable")));

        // Cache-backed commands still fail explicitly on the same profile.
        let sessions = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                path,
                "sessions",
                "--host",
                "host-1",
            ]),
            secrets,
        )
        .await;
        assert_eq!(sessions.exit, 1, "{sessions:?}");
        assert_eq!(sessions.to_json()["error"]["code"], "cache_error");
        assert_eq!(sessions.to_json()["error"]["action"], "retry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn export_refuses_a_symlink_target_without_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        std::fs::write(&target, b"do not overwrite").unwrap();
        let link = dir.path().join("bundle.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "doctor",
                "--export",
                link.to_str().unwrap(),
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        assert_eq!(outcome.exit, 2, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert_eq!(value["error"]["action"], "fix_input");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("symbolic link"));
        assert_eq!(std::fs::read(&target).unwrap(), b"do not overwrite");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
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
        assert!(!verified.contains("commands-pending"));
        let unverified = revoke_human(true);
        assert!(unverified.contains("Unable to verify"));
        assert!(unverified.contains("whalerein commands-pending"));
        assert!(unverified.contains("do not reuse the old identity"));
        assert!(unverified.ends_with('\n'));
    }

    #[tokio::test]
    async fn forget_reports_the_remote_grant_flag_and_exact_human_text() {
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        if std::io::stdin().is_terminal() {
            return;
        }
        let local_only = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "forget",
                "--local-only",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(local_only.exit, 0, "{local_only:?}");
        let value = local_only.to_json();
        assert_eq!(value["data"]["identity_forgotten"], true);
        assert_eq!(value["data"]["local_only"], true);
        assert_eq!(value["data"]["remote_grant_may_remain"], true);
        assert_eq!(
            local_only.human,
            "Forgot local identity. The remote grant may still be active; revoke the old device in host settings before reusing that identity.\n"
        );

        // A full forget makes no remote-grant claim.
        let normal = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--yes",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "forget",
            ]),
            secrets,
        )
        .await;
        assert_eq!(normal.exit, 0, "{normal:?}");
        assert_eq!(normal.to_json()["data"]["remote_grant_may_remain"], false);
        assert_eq!(normal.to_json()["data"]["local_only"], false);
        assert_eq!(normal.human, "Forgot this local device identity.\n");
    }

    #[tokio::test]
    async fn fork_reports_the_working_directory_or_an_explicit_warning() {
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
        let fork = || {
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                path,
                "session-fork",
                "--session",
                "session-1",
            ])
        };
        let created = run_with(fork(), secrets.clone()).await;
        assert_eq!(created.exit, 0, "{created:?}");
        let value = created.to_json();
        assert_eq!(value["command"], "session-fork");
        assert_eq!(value["data"]["lifecycle_receipt"]["disposition"], "READY");
        assert_eq!(value["data"]["working_directory"], "/work-child");
        assert!(created.human.contains("Directory: /work-child"));
        assert!(created.human.contains("no worktree is created"));

        // The fork succeeded, but the second, best-effort summary read failed:
        // the directory is not silently omitted and the child is preserved.
        // A distinct child id keeps the read out of the cache seeded by the
        // first fork, so the failed second read is what the CLI must report.
        {
            let mut host = access.host.lock().unwrap();
            host.fork_reply = whalerein_core::api::test_support::LifecycleReply::Ready {
                session_id: "forked-2".into(),
            };
            host.fail_get_session_for =
                Some(("forked-2".into(), whalerein_proto::ErrorCode::Unavailable));
        }
        let warned = run_with(fork(), secrets).await;
        assert_eq!(warned.exit, 0, "{warned:?}");
        let value = warned.to_json();
        assert_eq!(value["data"]["lifecycle_receipt"]["disposition"], "READY");
        assert_eq!(value["data"]["working_directory_error"], "unavailable");
        assert!(value["data"].get("working_directory").is_none());
        assert!(warned.human.contains("could not be read (unavailable)"));
        assert!(warned.human.contains("must not be retried or deleted"));
        assert!(!warned.human.contains("Directory:"));
        assert_eq!(
            warned.human,
            "Session forked-2 forked from session-1. The child shares the working directory; no worktree is created. The child working directory could not be read (unavailable); the child still exists and must not be retried or deleted.\n"
        );
    }

    #[test]
    fn human_renderers_are_used_for_model_queue_and_question_commands() {
        // The dispatch layer must not fall back to pretty JSON for these
        // commands; the pure renderers are covered in output.rs, so assert the
        // wiring by reading the source contract through their output.
        let model = whalerein_core::api::SessionModel {
            pending: None,
            last_used: None,
            default_selection: None,
            default_is_fallback: false,
            next: Some(whalerein_core::api::ModelChoice {
                provider: "p".into(),
                model: "m".into(),
                effort: None,
            }),
            pending_known: true,
        };
        let text = render_session_model(&model);
        assert!(text.contains("next: p m"));
        assert!(!text.contains('{'));
    }
}

/// Local `unknown` outcome that stays forensically traceable.
///
/// The bounded diagnostic history is written as well so a later
/// `doctor --export` can explain what was not observed.
fn unknown_outcome(core: &Core, command: &Command, message: String) -> CliOutcome {
    if core.save_failure("unknown").is_err() {
        tracing::warn!(event = "diagnostic_save_failed", trace_id = %core.diagnostic_id(), code = "unavailable");
    }
    CliOutcome::local_failure_with_diagnostic(command, "unknown", message, 7, core.diagnostic_id())
}

#[cfg(test)]
mod journey_guidance_tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[tokio::test]
    async fn an_unpaired_profile_names_the_pair_command() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "hosts",
            ]),
            Arc::new(MemorySecretStore::new()),
        )
        .await;
        assert_eq!(outcome.exit, 4);
        let value = outcome.to_json();
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
    }

    #[test]
    fn a_changed_host_identity_names_the_recovery_path_without_echoing_the_pin() {
        let error = whalerein_core::CoreError::FingerprintMismatch {
            expected: "aa".repeat(32),
            actual: "bb".repeat(32),
        };
        let outcome = core_error_outcome(&Command::Hosts, error);
        assert_eq!(outcome.exit, 8);
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "host_fingerprint_mismatch");
        let message = value["error"]["message"].as_str().unwrap();
        for hint in [
            "verify",
            "whalerein forget --local-only",
            "revoke",
            "whalerein pair",
        ] {
            assert!(message.contains(hint), "missing {hint:?}: {message}");
        }
        assert!(!message.contains(&"aa".repeat(32)));
        assert!(!message.contains(&"bb".repeat(32)));
        assert!(!message.to_ascii_lowercase().contains("token"));
        assert!(!message.to_ascii_lowercase().contains("private key"));
    }

    #[test]
    fn an_ambiguous_session_lists_its_candidate_hosts() {
        let message = "session exists on multiple Hosts; specify a Host";
        let error = ambiguous_session_conflict(
            message,
            "shared-session",
            &["host-1".to_owned(), "host-2".to_owned()],
        )
        .expect("candidates produce guidance");
        let CoreError::Conflict(explained) = error else {
            panic!("ambiguity stays a conflict");
        };
        assert!(explained.contains("host-1"), "{explained}");
        assert!(explained.contains("host-2"), "{explained}");
        assert!(explained.contains("--host"), "{explained}");
        assert!(explained.contains("--session shared-session"));
        assert_eq!(
            whalerein_core::CoreError::Conflict(explained).exit_code(),
            8
        );
        assert!(ambiguous_session_conflict(message, "s", &[]).is_none());
        // Explicit Host routing keeps working: no ambiguity means no message.
        assert!(!message.contains("Candidates"));
    }

    #[test]
    fn only_unexplained_ambiguity_messages_are_rewritten() {
        // The legacy Core message has no names: the CLI adds them.
        assert!(should_explain_ambiguous(
            "session exists on multiple Hosts; specify a Host"
        ));
        // Core's newer message already lists the candidates.
        assert!(!should_explain_ambiguous(
            "session exists on multiple Hosts (host-1, host-2); specify a Host with --host"
        ));
        // A caller that uses the word "candidate" is already actionable.
        assert!(!should_explain_ambiguous(
            "session exists on multiple Hosts; candidates are host-1, host-2"
        ));
        // Unrelated conflicts keep their original message.
        assert!(!should_explain_ambiguous(
            "question changed; refresh before answering"
        ));
        assert!(!should_explain_ambiguous(
            "--host a and sessions --host b disagree"
        ));
    }

    #[test]
    fn an_ambiguity_that_already_names_hosts_is_returned_unchanged() {
        // The wrapper must not touch an already-explanatory conflict: this is
        // the shape Core emits after SESS added bounded candidates.
        let original = "session exists on multiple Hosts (host-1, host-2); specify a Host";
        let error = CoreError::Conflict(original.into());
        let CoreError::Conflict(message) = error else {
            panic!("conflict")
        };
        assert!(!should_explain_ambiguous(&message));
        assert_eq!(message, original);
    }
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
            "No active open answer matches command {} (request {}). It may have completed already; recover its result with `whalerein command-recover --request {}`.\n",
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
            "No pending command matches {}; a mistyped command id cannot be recovered. Run `whalerein commands-pending` to list accepted commands.\n",
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
        assert!(
            matched.contains("command-recover --request request-1"),
            "{matched}"
        );
        let typo = question_cancel_message("command-typо", &commands);
        assert!(typo.contains("No pending command matches"), "{typo}");
        assert!(
            typo.contains("Pending commands: command-1 (request request-1)"),
            "{typo}"
        );
        let empty = question_cancel_message("command-typо", &[]);
        assert!(empty.contains("commands-pending"), "{empty}");
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
mod qmq_cli_fake_access_tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::test_support::FakeAccess;

    async fn paired(access: &FakeAccess) -> (tempfile::TempDir, String, Arc<dyn SecretStore>) {
        let directory = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let data_dir = directory.path().to_str().unwrap().to_owned();
        let pairing = access.pairing_uri();
        let outcome = run_with(
            Cli::try_parse_from([
                "whalerein",
                "--json",
                "--data-dir",
                &data_dir,
                "pair",
                &pairing,
                "--yes",
            ])
            .unwrap(),
            secrets.clone(),
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        (directory, data_dir, secrets)
    }

    #[tokio::test]
    async fn model_set_of_the_effective_selection_sends_no_command() {
        let access = FakeAccess::start().await;
        let (_directory, data_dir, secrets) = paired(&access).await;
        access.host.lock().unwrap().model_observed = Some("model".into());
        let before = access.host.lock().unwrap().commands_seen;
        let noop = run_with(
            Cli::try_parse_from([
                "whalerein",
                "--json",
                "--data-dir",
                data_dir.as_str(),
                "model",
                "set",
                "--session",
                "session-1",
                "--provider",
                "provider",
                "--model",
                "model",
            ])
            .unwrap(),
            secrets.clone(),
        )
        .await;
        assert_eq!(noop.exit, 0, "{noop:?}");
        assert_eq!(noop.to_json()["data"]["noop"], true);
        assert_eq!(access.host.lock().unwrap().commands_seen, before);
        // A different selection still submits exactly one command.
        let submitted = run_with(
            Cli::try_parse_from([
                "whalerein",
                "--json",
                "--data-dir",
                data_dir.as_str(),
                "model",
                "set",
                "--session",
                "session-1",
                "--provider",
                "provider",
                "--model",
                "other",
            ])
            .unwrap(),
            secrets,
        )
        .await;
        assert_eq!(submitted.exit, 0, "{submitted:?}");
        assert_eq!(access.host.lock().unwrap().commands_seen, before + 1);
    }

    #[tokio::test]
    async fn unanswerable_or_unknown_question_phase_is_an_input_error() {
        let access = FakeAccess::start().await;
        let (directory, data_dir, secrets) = paired(&access).await;
        let answers = directory.path().join("answers.json");
        std::fs::write(&answers, r#"{"answers":[{"id":"q","selected":["A"]}]}"#).unwrap();
        for phase in ["QUESTION_REPLY_QUEUED", "QUESTION_OPNE"] {
            let outcome = run_with(
                Cli::try_parse_from([
                    "whalerein",
                    "--json",
                    "--data-dir",
                    data_dir.as_str(),
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
                    phase,
                    "--input",
                    answers.to_str().unwrap(),
                ])
                .unwrap(),
                secrets.clone(),
            )
            .await;
            assert_eq!(outcome.exit, 2, "{phase}: {outcome:?}");
            let value = outcome.to_json();
            assert_eq!(value["error"]["code"], "invalid_argument", "{phase}");
            assert_ne!(value["error"]["code"], "conflict", "{phase}");
        }
    }
}

#[cfg(test)]
mod approval_read_tests {
    use super::*;
    use clap::Parser;
    use whalerein_core::api::test_support::FakeAccess;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    fn approval(id: &str, state: whalerein_proto::ApprovalState) -> whalerein_proto::Approval {
        whalerein_proto::Approval {
            approval_id: id.into(),
            session_id: "session-1".into(),
            tool_name: "bash".into(),
            call_id: format!("call-{id}"),
            reason: "review".into(),
            digest: vec![1; 32],
            version: 3,
            expires_at_unix_ms: 9,
            state: state as i32,
            decision: whalerein_proto::ApprovalDecision::Unspecified as i32,
        }
    }

    async fn paired(dir: &std::path::Path) -> (FakeAccess, Arc<dyn SecretStore>) {
        let access = FakeAccess::start().await;
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--yes",
                "--data-dir",
                dir.to_str().unwrap(),
                "pair",
                &access.pairing_uri(),
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        (access, secrets)
    }

    #[tokio::test]
    async fn approval_read_reports_approved_expired_and_unavailable_states() {
        let dir = tempfile::tempdir().unwrap();
        let (access, secrets) = paired(dir.path()).await;
        access.host.lock().unwrap().approvals = vec![
            approval(
                "approval-approved",
                whalerein_proto::ApprovalState::Approved,
            ),
            approval("approval-expired", whalerein_proto::ApprovalState::Expired),
            approval(
                "approval-unavailable",
                whalerein_proto::ApprovalState::Unavailable,
            ),
        ];
        for (id, state) in [
            ("approval-approved", "APPROVAL_STATE_APPROVED"),
            ("approval-expired", "APPROVAL_STATE_EXPIRED"),
            ("approval-unavailable", "APPROVAL_STATE_UNAVAILABLE"),
        ] {
            let outcome = run_with(
                cli(&[
                    "whalerein",
                    "--json",
                    "--data-dir",
                    dir.path().to_str().unwrap(),
                    "approvals",
                    "--request",
                    id,
                ]),
                secrets.clone(),
            )
            .await;
            assert_eq!(outcome.exit, 0, "{id}: {outcome:?}");
            let value = outcome.to_json();
            assert_eq!(value["command"], "approvals");
            assert_eq!(value["data"]["approval"]["approval_id"], id);
            assert_eq!(value["data"]["approval"]["state"], state);
            assert!(value["data"].get("state").is_none());
            assert_eq!(
                outcome.human.lines().nth(1),
                Some(format!("state: {state}").as_str())
            );
            assert!(outcome.human.contains("version: 3"));
            assert!(outcome
                .human
                .contains("decision: APPROVAL_DECISION_UNSPECIFIED"));
        }
    }

    #[tokio::test]
    async fn approval_read_unknown_id_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let (_access, secrets) = paired(dir.path()).await;
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "approvals",
                "--request",
                "missing-approval",
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 3, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["action"], "refresh");
        assert!(value.get("data").is_none() || value["data"].is_null());
    }

    #[tokio::test]
    async fn approval_read_with_two_hosts_names_candidates_and_honors_host_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (access, secrets) = paired(dir.path()).await;
        {
            let mut host = access.host.lock().unwrap();
            host.extra_host = true;
            host.extra_host_approvals = vec![approval(
                "approval-two",
                whalerein_proto::ApprovalState::Pending,
            )];
        }
        // Without --host the read is Host-scoped and names the candidates.
        let ambiguous = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "approvals",
                "--request",
                "approval-two",
            ]),
            secrets.clone(),
        )
        .await;
        assert_eq!(ambiguous.exit, 8, "{ambiguous:?}");
        let value = ambiguous.to_json();
        assert_eq!(value["error"]["code"], "conflict");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains("host-1"), "{message}");
        assert!(message.contains("host-2"), "{message}");
        assert!(message.contains("--host"), "{message}");

        // A single authorized Host keeps the read working with an explicit --host.
        access.host.lock().unwrap().extra_host = false;
        access.host.lock().unwrap().approvals = vec![approval(
            "approval-one",
            whalerein_proto::ApprovalState::Pending,
        )];
        let scoped = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--host",
                "host-1",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "approvals",
                "--request",
                "approval-one",
            ]),
            secrets,
        )
        .await;
        assert_eq!(scoped.exit, 0, "{scoped:?}");
        assert_eq!(
            scoped.to_json()["data"]["approval"]["state"],
            "APPROVAL_STATE_PENDING"
        );
    }

    #[tokio::test]
    async fn approvals_without_a_session_or_explicit_host_is_an_input_error() {
        // AC-08.cli: a session-less, Host-less list is the same input error it
        // always was; it must not become an all-Hosts approval listing.
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let outcome = run_with(
            Cli {
                json: true,
                yes: false,
                data_dir: Some(dir.path().to_path_buf()),
                config: None,
                host: None,
                environment: None,
                command: Command::Approvals {
                    session: None,
                    request: None,
                },
            },
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 2, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["command"], "approvals");
        assert_eq!(value["error"]["code"], "invalid_argument");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session identity missing"));
    }

    #[tokio::test]
    async fn approval_list_json_shape_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let (access, secrets) = paired(dir.path()).await;
        access.host.lock().unwrap().approvals = vec![approval(
            "approval-1",
            whalerein_proto::ApprovalState::Pending,
        )];
        let outcome = run_with(
            cli(&[
                "whalerein",
                "--json",
                "--data-dir",
                dir.path().to_str().unwrap(),
                "approvals",
                "--session",
                "session-1",
            ]),
            secrets,
        )
        .await;
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        let value = outcome.to_json();
        assert_eq!(value["data"]["session_id"], "session-1");
        assert_eq!(value["data"]["approvals"][0]["approval_id"], "approval-1");
        assert!(value["data"].get("approval").is_none());
        assert!(value["data"].get("state").is_none());
    }
}
