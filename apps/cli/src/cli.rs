//! Command-line arguments. The CLI only parses user input; all protocol work
//! lives in `whalerein-core`.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Delivery {
    StartIfIdle,
    EnqueueAfterRun,
    Steer,
}

impl From<Delivery> for whalerein_core::api::DeliveryMode {
    fn from(value: Delivery) -> Self {
        match value {
            Delivery::StartIfIdle => Self::StartIfIdle,
            Delivery::EnqueueAfterRun => Self::EnqueueAfterRun,
            Delivery::Steer => Self::Steer,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "whalerein",
    version,
    about = "WhaleRein remote-control client",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Emit the stable `{schema:"whalerein/v1", command, data|error}` object.
    #[arg(long, global = true)]
    pub json: bool,

    /// Answer yes to pairing and diagnostic-export confirmation prompts.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Override the local data directory (also WHALEREIN_DATA_DIR).
    #[arg(long, global = true, value_name = "PATH")]
    pub data_dir: Option<PathBuf>,

    /// Scope session operations to one authorized Host (placed before the command).
    #[arg(long, value_name = "HOST_ID")]
    pub host: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Pair with a host using a whalerein://pair invitation.
    Pair {
        /// Pairing URI. The one-time token is never logged or echoed.
        uri: String,
        /// Require this host fingerprint (hex, spaces allowed).
        #[arg(long, value_name = "HEX")]
        fingerprint: Option<String>,
        /// Name shown on the host. Defaults to this computer's name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },

    /// Resume a confirmed pairing after a lost response or process restart.
    PairResume,

    /// Show redacted records of committed pairings whose original grants are unavailable.
    PairingHistory,

    /// List hosts authorized for this device.
    Hosts,

    /// List sessions on one host.
    Sessions {
        #[arg(long, value_name = "HOST_ID")]
        host: String,
    },

    /// List currently available source models.
    Models {
        #[arg(long, value_name = "HOST_ID")]
        host: String,
    },

    /// Inspect or change a session's model.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },

    /// Create or reuse a session according to the host's DSH rules.
    SessionCreate {
        #[arg(long, value_name = "HOST_ID")]
        host: String,
        #[arg(long, conflicts_with = "workspace")]
        cwd: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
    },

    /// List unfinished command identities available for recovery.
    CommandsPending,

    /// Recover a previously submitted command without repeating the action.
    #[command(name = "command-recover")]
    Recover {
        #[arg(long, value_name = "REQUEST_ID")]
        request: String,
    },

    /// Fork the latest completed turn; the child shares the working directory.
    SessionFork {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
    },

    /// Stream session output events.
    Tail {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        /// Exit after the first event.
        #[arg(long)]
        once: bool,
        /// Stop after this many seconds. `--once` defaults to 15 seconds.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
    },

    /// Wait for native session quiescence or pending input.
    Wait {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        /// Overall deadline in seconds, with millisecond precision.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<String>,
        /// Return exit code 7 when the observation times out.
        #[arg(long)]
        fail_on_timeout: bool,
    },

    /// Fill the local event cache from the host (gap fill or snapshot).
    Sync {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
    },

    /// Send a user message to a session.
    Send {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        /// Delivery mode. start-if-idle checks a sampled state and is not atomic.
        #[arg(long, value_enum, default_value = "enqueue-after-run")]
        delivery: Delivery,
        /// Message text. It is sent, never logged or echoed.
        text: String,
    },

    /// List pending tool approvals for a session.
    Approvals {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
    },

    /// List pending human questions for a session.
    Questions {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        #[arg(long, default_value = "", value_name = "CURSOR")]
        cursor: String,
    },

    /// Submit a complete structured answer from a file or standard input (-).
    Answer {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        #[arg(long, value_name = "BATCH_ID")]
        batch: String,
        #[arg(long, value_name = "EPOCH")]
        epoch: String,
        #[arg(long, value_name = "VERSION")]
        version: u64,
        #[arg(long, value_name = "DIGEST")]
        digest: String,
        #[arg(long, value_name = "PHASE")]
        phase: String,
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
        /// Stable command identity for explicit cancellation and recovery.
        #[arg(long, value_name = "COMMAND_ID")]
        command_id: Option<String>,
    },

    /// Request cancellation of an in-flight open question answer.
    QuestionCancel {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        #[arg(long, value_name = "COMMAND_ID")]
        command: String,
    },

    /// Approve a pending tool request.
    Approve {
        #[arg(long, value_name = "APPROVAL_ID")]
        request: String,
        /// SHA-256 digest shown by the host event.
        #[arg(long, value_name = "DIGEST")]
        digest: String,
        #[arg(long, value_name = "VERSION")]
        version: i64,
    },

    /// Reject a pending tool request.
    Reject {
        #[arg(long, value_name = "APPROVAL_ID")]
        request: String,
        #[arg(long, value_name = "DIGEST")]
        digest: String,
        #[arg(long, value_name = "VERSION")]
        version: i64,
    },

    /// Inspect or change pending source work.
    Queue {
        #[command(subcommand)]
        action: QueueCommand,
    },

    /// Interrupt a session.
    Interrupt {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
    },

    /// Revoke this device and clear local authorization material.
    Revoke,

    /// Forget this local identity after revoking its active pairing.
    Forget {
        /// Discard local identity even if server revocation cannot be verified.
        /// The old device must then be removed separately in host settings.
        #[arg(long)]
        local_only: bool,
    },

    /// Manage local service profiles and the active selection.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Show the paired Access address and local unfinished command counts.
    Doctor {
        /// Remove the last failure's local diagnostic metadata.
        #[arg(long, conflicts_with = "export")]
        clear_diagnostics: bool,
        /// Write a redacted diagnostic bundle after confirmation.
        #[arg(long, value_name = "FILE")]
        export: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ProfileAction {
    /// List local profiles; testing-only staging stays hidden unless asked for.
    List {
        /// Include testing-only staging profiles.
        #[arg(long)]
        include_staging: bool,
    },
    /// Show the active profile and selected service.
    Current,
    /// Add a profile without activating it.
    Add {
        /// Local profile name.
        name: String,
        /// Add official staging; requires --enable-testing.
        #[arg(long)]
        staging: bool,
        /// Explicitly accept the testing-only staging environment.
        #[arg(long)]
        enable_testing: bool,
        /// Custom account API origin.
        #[arg(long, value_name = "URL")]
        api_origin: Option<String>,
        /// Custom OIDC issuer.
        #[arg(long, value_name = "URL")]
        issuer: Option<String>,
        /// Custom public native client registration.
        #[arg(long, value_name = "ID")]
        client_id: Option<String>,
        /// Optional custom Access URL.
        #[arg(long, value_name = "URL")]
        access_url: Option<String>,
    },
    /// Publish a new active profile after old client activity has stopped.
    Use {
        /// Local profile name.
        name: String,
    },
    /// Remove a managed profile's local state; never claims remote revocation.
    Remove {
        /// Local profile name.
        name: String,
    },
}

impl ProfileAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::List { .. } => "profile-list",
            Self::Current => "profile-current",
            Self::Add { .. } => "profile-add",
            Self::Use { .. } => "profile-use",
            Self::Remove { .. } => "profile-remove",
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum ModelAction {
    /// Read the recorded session choice and source fallback.
    Get {
        #[arg(long)]
        session: String,
    },
    /// Select a session model and attempt to update the source default.
    Set {
        #[arg(long)]
        session: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        effort: Option<String>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum QueueCommand {
    /// Read the current source queue without activating a cold session.
    List {
        #[arg(long)]
        session: String,
    },
    /// Replace the entire content of an item; attachments are removed.
    Edit {
        #[arg(long)]
        session: String,
        #[arg(long)]
        item: String,
        text: String,
    },
    /// Withdraw one queued item.
    Remove {
        #[arg(long)]
        session: String,
        #[arg(long)]
        item: String,
    },
    /// Promote an eligible item into the currently running turn.
    Steer {
        #[arg(long)]
        session: String,
        #[arg(long)]
        item: String,
    },
}

impl Command {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::SessionFork { session }
            | Self::Sync { session }
            | Self::Send { session, .. }
            | Self::Approvals { session }
            | Self::Questions { session, .. }
            | Self::Answer { session, .. }
            | Self::QuestionCancel { session, .. }
            | Self::Interrupt { session } => Some(session),
            Self::Model {
                action: ModelAction::Get { session } | ModelAction::Set { session, .. },
            } => Some(session),
            Self::Queue {
                action:
                    QueueCommand::List { session }
                    | QueueCommand::Edit { session, .. }
                    | QueueCommand::Remove { session, .. }
                    | QueueCommand::Steer { session, .. },
            } => Some(session),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Pair { .. } => "pair",
            Self::PairResume => "pair-resume",
            Self::PairingHistory => "pairing-history",
            Self::Hosts => "hosts",
            Self::Sessions { .. } => "sessions",
            Self::Models { .. } => "models",
            Self::Model {
                action: ModelAction::Get { .. },
            } => "model-get",
            Self::Model {
                action: ModelAction::Set { .. },
            } => "model-set",
            Self::SessionCreate { .. } => "session-create",
            Self::SessionFork { .. } => "session-fork",
            Self::CommandsPending => "commands-pending",
            Self::Recover { .. } => "command-recover",
            Self::Tail { .. } => "tail",
            Self::Wait { .. } => "wait",
            Self::Sync { .. } => "sync",
            Self::Send { .. } => "send",
            Self::Approvals { .. } => "approvals",
            Self::Questions { .. } => "questions",
            Self::Answer { .. } => "answer",
            Self::QuestionCancel { .. } => "question-cancel",
            Self::Approve { .. } => "approve",
            Self::Reject { .. } => "reject",
            Self::Interrupt { .. } => "interrupt",
            Self::Queue { .. } => "queue",
            Self::Revoke => "revoke",
            Self::Forget { .. } => "forget",
            Self::Profile { action } => action.name(),
            Self::Doctor { .. } => "doctor",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parse")
    }

    #[test]
    fn profile_actions_have_stable_names_and_explicit_testing_flags() {
        let cli = parse(&["whalerein", "profile", "list", "--include-staging"]);
        assert_eq!(cli.command.name(), "profile-list");
        assert!(matches!(
            cli.command,
            Command::Profile {
                action: ProfileAction::List {
                    include_staging: true
                }
            }
        ));
        let cli = parse(&["whalerein", "profile", "current"]);
        assert_eq!(cli.command.name(), "profile-current");
        let cli = parse(&[
            "whalerein",
            "profile",
            "add",
            "official-staging",
            "--staging",
            "--enable-testing",
        ]);
        assert_eq!(cli.command.name(), "profile-add");
        assert!(matches!(
            cli.command,
            Command::Profile {
                action: ProfileAction::Add {
                    staging: true,
                    enable_testing: true,
                    ..
                }
            }
        ));
        let cli = parse(&["whalerein", "profile", "use", "official-staging"]);
        assert_eq!(cli.command.name(), "profile-use");
        let cli = parse(&["whalerein", "profile", "remove", "official-staging"]);
        assert_eq!(cli.command.name(), "profile-remove");
        assert!(Cli::try_parse_from(["whalerein", "profile"]).is_err());
        assert!(Cli::try_parse_from(["whalerein", "profile", "use"]).is_err());
    }

    #[test]
    fn session_host_scope_does_not_replace_host_scoped_commands() {
        let cli = parse(&[
            "whalerein",
            "--host",
            "host-2",
            "send",
            "--session",
            "shared",
            "fixture",
        ]);
        assert_eq!(cli.host.as_deref(), Some("host-2"));
        assert!(matches!(cli.command, Command::Send { .. }));
        let cli = parse(&["whalerein", "sessions", "--host", "host-1"]);
        assert!(cli.host.is_none());
        assert!(matches!(cli.command, Command::Sessions { host } if host == "host-1"));
    }

    #[test]
    fn parses_model_commands_without_acknowledgement() {
        assert!(matches!(
            parse(&["whalerein", "models", "--host", "h"]).command,
            Command::Models { .. }
        ));
        assert!(matches!(
            parse(&["whalerein", "model", "get", "--session", "s"]).command,
            Command::Model {
                action: ModelAction::Get { .. }
            }
        ));
        assert!(matches!(
            parse(&[
                "whalerein",
                "model",
                "set",
                "--session",
                "s",
                "--provider",
                "p",
                "--model",
                "m"
            ])
            .command,
            Command::Model {
                action: ModelAction::Set { effort: None, .. }
            }
        ));
        assert!(Cli::try_parse_from([
            "whalerein",
            "model",
            "set",
            "--session",
            "s",
            "--provider",
            "p",
            "--model",
            "m",
            "--acknowledge-default-effect"
        ])
        .is_err());
    }

    #[test]
    fn session_create_arguments_leave_remote_paths_unmodified_and_conflict() {
        let cli = parse(&[
            "whalerein",
            "session-create",
            "--host",
            "h",
            "--cwd",
            "relative",
            "--session-id",
            "id",
        ]);
        assert!(
            matches!(cli.command, Command::SessionCreate { cwd: Some(cwd), session_id: Some(id), .. } if cwd == "relative" && id == "id")
        );
        assert!(Cli::try_parse_from([
            "whalerein",
            "session-create",
            "--host",
            "h",
            "--cwd",
            "/a",
            "--workspace",
            "w"
        ])
        .is_err());
    }

    #[test]
    fn fork_rejects_explicit_native_cut() {
        assert!(matches!(
            parse(&["whalerein", "session-fork", "--session", "s"]).command,
            Command::SessionFork { .. }
        ));
        assert!(Cli::try_parse_from([
            "whalerein",
            "session-fork",
            "--session",
            "s",
            "--at-seq",
            "2"
        ])
        .is_err());
    }

    #[test]
    fn parses_all_commands() {
        assert!(matches!(
            parse(&["whalerein", "hosts"]).command,
            Command::Hosts
        ));
        assert!(matches!(
            parse(&["whalerein", "pair", "whalerein://pair?v=1"]).command,
            Command::Pair { name: None, .. }
        ));
        assert!(matches!(
            parse(&[
                "whalerein",
                "pair",
                "--name",
                "办公室的笔记本",
                "whalerein://pair?v=1"
            ])
            .command,
            Command::Pair {
                name: Some(name),
                ..
            } if name == "办公室的笔记本"
        ));
        assert!(matches!(
            parse(&["whalerein", "sessions", "--host", "h"]).command,
            Command::Sessions { host } if host == "h"
        ));
        assert!(matches!(
            parse(&["whalerein", "tail", "--session", "s", "--once"]).command,
            Command::Tail { session, once: true, timeout: None } if session == "s"
        ));
        assert!(matches!(
            parse(&["whalerein", "send", "--session", "s", "hello"]).command,
            Command::Send { session, text, .. } if session == "s" && text == "hello"
        ));
        assert!(matches!(
            parse(&["whalerein", "send", "--session", "s", "hello"]).command,
            Command::Send {
                delivery: Delivery::EnqueueAfterRun,
                ..
            }
        ));
        assert!(matches!(
            parse(&[
                "whalerein",
                "send",
                "--session",
                "s",
                "--delivery",
                "steer",
                "hello"
            ])
            .command,
            Command::Send {
                delivery: Delivery::Steer,
                ..
            }
        ));
        assert!(matches!(
            parse(&[
                "whalerein",
                "send",
                "--session",
                "s",
                "--delivery",
                "start-if-idle",
                "hello"
            ])
            .command,
            Command::Send {
                delivery: Delivery::StartIfIdle,
                ..
            }
        ));
        assert!(matches!(
            parse(&[
                "whalerein",
                "approve",
                "--request",
                "r",
                "--digest",
                "00",
                "--version",
                "2"
            ])
            .command,
            Command::Approve { request, digest, version: 2 } if request == "r" && digest == "00"
        ));
        assert!(matches!(
            parse(&[
                "whalerein",
                "reject",
                "--request",
                "r",
                "--digest",
                "00",
                "--version",
                "2"
            ])
            .command,
            Command::Reject { request, digest, version: 2 } if request == "r" && digest == "00"
        ));
        assert!(matches!(
            parse(&["whalerein", "interrupt", "--session", "s"]).command,
            Command::Interrupt { session } if session == "s"
        ));
        assert!(matches!(
            parse(&["whalerein", "revoke"]).command,
            Command::Revoke
        ));
        assert!(Cli::try_parse_from(["whalerein", "revoke", "--device", "d"]).is_err());
        assert!(matches!(
            parse(&["whalerein", "doctor"]).command,
            Command::Doctor {
                clear_diagnostics: false,
                export: None
            }
        ));
        assert!(matches!(
            parse(&["whalerein", "doctor", "--export", "out.json"]).command,
            Command::Doctor {
                clear_diagnostics: false,
                export: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn accepts_global_flags_before_and_after_subcommands() {
        let cli = parse(&[
            "whalerein",
            "--json",
            "--yes",
            "--data-dir",
            "/tmp/wr",
            "hosts",
        ]);
        assert!(cli.json);
        assert!(cli.yes);
        assert_eq!(cli.data_dir.unwrap(), PathBuf::from("/tmp/wr"));
    }

    #[test]
    fn exposes_stable_names_for_every_cli_command() {
        assert_eq!(parse(&["whalerein", "hosts"]).command.name(), "hosts");
        assert_eq!(parse(&["whalerein", "doctor"]).command.name(), "doctor");
    }

    #[test]
    fn missing_required_arguments_are_usage_errors() {
        assert!(Cli::try_parse_from(["whalerein", "sessions"]).is_err());
        assert!(Cli::try_parse_from(["whalerein", "send", "--session", "s"]).is_err());
        assert!(Cli::try_parse_from(["whalerein", "approve", "--request", "r"]).is_err());
        assert!(Cli::try_parse_from(["whalerein", "unknown"]).is_err());
        assert!(Cli::try_parse_from(["whalerein", "doctor", "--detail"]).is_err());
        assert!(Cli::try_parse_from(["whalerein", "doctor", "--clear"]).is_err());
    }
}
