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
    after_help = "Create ~/.whalerein/config.toml or select a file with --config:\n  version = 1\n  service = \"whalerein_cloud\"\n  data_dir = \"./cloud\"\n\nCloud: login → host list → host connect HOST → session list → session send ID TEXT.\nStandalone: use service = \"standalone\" and a fresh data_dir, then run host connect --invite-stdin."
)]
pub struct Cli {
    /// Emit the stable `{schema:"whalerein/client/v1", command, data|error}` object.
    #[arg(long, global = true)]
    pub json: bool,

    /// Show detailed, redacted diagnostics on stderr.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Render model reasoning blocks in live, history and send output.
    /// Reasoning is hidden by default; this flag never changes authorization.
    #[arg(long, global = true)]
    pub show_reasoning: bool,

    /// Show complete tool result text in human output.
    /// The default truncates long tool results; JSON stays complete.
    #[arg(long, global = true)]
    pub full: bool,

    /// Consent to confirmation prompts (pairing, revoke, forget, diagnostic export).
    #[arg(long, global = true)]
    pub yes: bool,

    /// Select the client TOML file (default ~/.whalerein/config.toml).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Store credentials in a private local directory. Defaults to the OS keyring.
    #[cfg(unix)]
    #[arg(long, global = true, value_name = "PATH")]
    pub secrets_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: AccountArgs,
}

#[derive(Debug, Subcommand)]
enum AccountArgs {
    /// Sign in to the selected WhaleRein Cloud environment in a browser.
    Login,
    /// Clear this instance's Cloud sign-in.
    Logout,
    /// Inspect configuration, sign-in and Device access.
    Status,
    /// Show the paired Access address and local unfinished command counts.
    Doctor {
        /// Remove the last failure's local diagnostic metadata.
        #[arg(long, conflicts_with = "export")]
        clear_diagnostics: bool,
        /// Write a redacted diagnostic bundle after confirmation.
        #[arg(long, value_name = "FILE")]
        export: Option<PathBuf>,
    },
    Account {
        #[command(subcommand)]
        action: CloudAccountArgs,
    },
    Host {
        #[command(subcommand)]
        action: HostArgs,
    },
    Session {
        #[command(subcommand)]
        action: SessionArgs,
    },
    Device {
        #[command(subcommand)]
        action: DeviceArgs,
    },
    Operation {
        #[command(subcommand)]
        action: OperationArgs,
    },
}

impl AccountArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::Login => (Command::Login, None),
            Self::Logout => (Command::Logout, None),
            Self::Status => (Command::AccountStatus, None),
            Self::Doctor {
                clear_diagnostics,
                export,
            } => (
                Command::Doctor {
                    clear_diagnostics,
                    export,
                },
                None,
            ),
            Self::Account { action } => (
                match action {
                    CloudAccountArgs::Sessions => Command::AccountSessions,
                    CloudAccountArgs::RevokeAllSessions => Command::AccountRevokeAllSessions,
                    CloudAccountArgs::RevokeSession { id } => Command::AccountRevokeSession { id },
                },
                None,
            ),
            Self::Host { action } => action.into_command(),
            Self::Session { action } => action.into_command(),
            Self::Device { action } => action.into_command(),
            Self::Operation { action } => action.into_command(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum CloudAccountArgs {
    /// List Cloud sign-ins for your account.
    Sessions,
    /// Sign out every Cloud session for this account in the selected environment.
    RevokeAllSessions,
    /// Sign out a Cloud session belonging to your account.
    RevokeSession {
        #[arg(value_name = "SESSION_ID")]
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum HostArgs {
    /// List Hosts for the selected Cloud or Standalone profile.
    List,
    /// Connect a Cloud Host by ID or read a Standalone invitation from stdin.
    Connect {
        #[arg(
            required_unless_present = "invite_stdin",
            conflicts_with = "invite_stdin"
        )]
        host: Option<String>,
        #[arg(long)]
        invite_stdin: bool,
        #[arg(long, requires = "invite_stdin", value_name = "HEX")]
        fingerprint: Option<String>,
        #[arg(long, requires = "invite_stdin")]
        name: Option<String>,
    },
    /// Select a connected Host as the default.
    Default { host: String },
    /// Probe one Host and report its authoritative device connection.
    Status {
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
}

impl HostArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::List => (Command::Hosts, None),
            Self::Connect {
                host,
                invite_stdin,
                fingerprint,
                name,
            } => (
                Command::Connect {
                    host,
                    invite_stdin,
                    fingerprint,
                    name,
                },
                None,
            ),
            Self::Default { host } => (Command::DefaultHost { host }, None),
            Self::Status { host } => (Command::HostStatus { host }, None),
        }
    }
}

#[derive(Debug, Subcommand)]
enum SessionArgs {
    /// List sessions on one host.
    List {
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        /// Include sessions the source reports as archived.
        #[arg(long)]
        all: bool,
    },
    /// Create or reuse a session according to the host's DSH rules.
    Create {
        #[arg(long, conflicts_with = "workspace")]
        cwd: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Fill the local event cache from the host (gap fill or snapshot).
    Show {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Read one bounded page of committed history, newest page first.
    History {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        /// Read the eligible encrypted local cache instead of the Source.
        #[arg(long)]
        cache: bool,
        /// Continue toward older history with a token from a previous page.
        /// Combine with --cache to continue a cache-sourced page.
        #[arg(long, value_name = "TOKEN", conflicts_with = "message")]
        continuation: Option<String>,
        /// Resolve one exact committed-message reference shown by send or history.
        #[arg(
            long,
            value_name = "REFERENCE",
            conflicts_with_all = ["continuation", "cache", "max_messages", "max_bytes"]
        )]
        message: Option<String>,
        /// Bound the number of messages in this page (1-1000).
        #[arg(long, value_name = "N")]
        max_messages: Option<u32>,
        /// Bound the encoded content bytes in this page.
        #[arg(long, value_name = "BYTES")]
        max_bytes: Option<u32>,
    },
    /// Download one verified attachment reference shown by `session history`.
    Download {
        /// Opaque attachment reference shown by `session history`.
        #[arg(value_name = "REFERENCE")]
        reference: String,
        /// Destination directory for the verified file.
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
        /// Explicitly replace an existing destination file.
        #[arg(long)]
        force: bool,
        /// Override the Source-suggested display name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Overall download deadline in seconds (positive, decimals allowed).
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<String>,
        /// Select the Host that owns the attachment.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        /// Session identity that owns the attachment (required by the save action).
        #[arg(long, value_name = "SESSION_ID", required = true)]
        session: String,
    },
    /// Fork the latest completed turn; the child shares the working directory.
    Fork {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Send a user message to a session.
    Send {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Delivery mode. start-if-idle checks a sampled state and is not atomic.
        #[arg(long, value_enum, default_value = "enqueue-after-run")]
        delivery: Delivery,
        /// Message text. It is sent, never logged or echoed. A text that starts
        /// with `-` is accepted; use `--` if it could also be read as a flag.
        #[arg(allow_negative_numbers = true)]
        text: String,
        /// Return the submission receipt without waiting for a reply.
        #[arg(long)]
        detach: bool,
        /// Deadline in seconds for observing the matching reply.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<String>,
        /// Optional operation ID for retrying this exact intent.
        #[arg(long, value_name = "ID")]
        operation_id: Option<String>,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Stream session output events.
    Watch {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Exit after the first visible output (text or committed message).
        #[arg(long)]
        once: bool,
        /// Stop after this many seconds (decimals allowed, must be non-zero).
        /// `--once` defaults to 15 seconds.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<String>,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Wait for native session quiescence or pending input.
    Wait {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Overall deadline in seconds, with millisecond precision.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<String>,
        /// Return exit code 7 when the observation times out.
        #[arg(long)]
        fail_on_timeout: bool,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Interrupt a session.
    Interrupt {
        #[arg(value_name = "SESSION_ID")]
        session: String,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// List currently available source models.
    Models {
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Inspect or change a session's model.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Inspect or change pending source work.
    Queue {
        #[command(subcommand)]
        action: QueueCommand,
    },
    Approval {
        #[command(subcommand)]
        action: ApprovalArgs,
    },
    Question {
        #[command(subcommand)]
        action: QuestionArgs,
    },
}

impl SessionArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::List { host, all } => (
                Command::Sessions {
                    host: String::new(),
                    all,
                },
                host,
            ),
            Self::Create {
                cwd,
                workspace,
                preset,
                session_id,
                host,
            } => (
                Command::SessionCreate {
                    host: String::new(),
                    cwd,
                    workspace,
                    preset,
                    session_id,
                },
                host,
            ),
            Self::Show { session, host } => (Command::Sync { session }, host),
            Self::History {
                session,
                cache,
                continuation,
                message,
                max_messages,
                max_bytes,
                host,
            } => (
                Command::History {
                    session,
                    cache,
                    continuation,
                    message,
                    max_messages,
                    max_bytes,
                },
                host,
            ),
            Self::Download {
                reference,
                to,
                force,
                name,
                timeout,
                host,
                session,
            } => (
                Command::SessionDownload {
                    reference,
                    to,
                    force,
                    name,
                    timeout,
                    session,
                },
                host,
            ),
            Self::Fork { session, host } => (Command::SessionFork { session }, host),
            Self::Send {
                session,
                delivery,
                text,
                detach,
                timeout,
                operation_id,
                host,
            } => (
                Command::Send {
                    session,
                    delivery,
                    text,
                    detach,
                    timeout,
                    operation_id,
                },
                host,
            ),
            Self::Watch {
                session,
                once,
                timeout,
                host,
            } => (
                Command::Tail {
                    session,
                    once,
                    timeout,
                },
                host,
            ),
            Self::Wait {
                session,
                timeout,
                fail_on_timeout,
                host,
            } => (
                Command::Wait {
                    session,
                    timeout,
                    fail_on_timeout,
                },
                host,
            ),
            Self::Interrupt { session, host } => (Command::Interrupt { session }, host),
            Self::Models { host } => (
                Command::Models {
                    host: String::new(),
                },
                host,
            ),
            Self::Model { action } => {
                let (action, host) = action.take_host();
                (Command::Model { action }, host)
            }
            Self::Queue { action } => {
                let (action, host) = action.take_host();
                (Command::Queue { action }, host)
            }
            Self::Approval { action } => action.into_command(),
            Self::Question { action } => action.into_command(),
        }
    }
}

#[derive(Debug, Subcommand)]
enum ApprovalArgs {
    /// List pending tool approvals for a session, or read one approval's state.
    List {
        /// List pending approvals for this session (the default read mode).
        #[arg(long, value_name = "SESSION_ID", required_unless_present = "request")]
        session: Option<String>,
        /// Read one approval's live or terminal state by id.
        #[arg(long, value_name = "APPROVAL_ID", conflicts_with = "session")]
        request: Option<String>,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
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
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Reject a pending tool request.
    Reject {
        #[arg(long, value_name = "APPROVAL_ID")]
        request: String,
        #[arg(long, value_name = "DIGEST")]
        digest: String,
        #[arg(long, value_name = "VERSION")]
        version: i64,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
}

impl ApprovalArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::List {
                session,
                request,
                host,
            } => (Command::Approvals { session, request }, host),
            Self::Approve {
                request,
                digest,
                version,
                host,
            } => (
                Command::Approve {
                    request,
                    digest,
                    version,
                },
                host,
            ),
            Self::Reject {
                request,
                digest,
                version,
                host,
            } => (
                Command::Reject {
                    request,
                    digest,
                    version,
                },
                host,
            ),
        }
    }
}

#[derive(Debug, Subcommand)]
enum QuestionArgs {
    /// List pending human questions for a session.
    List {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        #[arg(long, default_value = "", value_name = "CURSOR")]
        cursor: String,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
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
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Request cancellation of an in-flight open question answer.
    Cancel {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        #[arg(long, value_name = "COMMAND_ID")]
        command: String,
        /// Select the Host for this operation.
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
}

impl QuestionArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::List {
                session,
                cursor,
                host,
            } => (Command::Questions { session, cursor }, host),
            Self::Answer {
                session,
                batch,
                epoch,
                version,
                digest,
                phase,
                input,
                command_id,
                host,
            } => (
                Command::Answer {
                    session,
                    batch,
                    epoch,
                    version,
                    digest,
                    phase,
                    input,
                    command_id,
                },
                host,
            ),
            Self::Cancel {
                session,
                command,
                host,
            } => (Command::QuestionCancel { session, command }, host),
        }
    }
}

#[derive(Debug, Subcommand)]
enum DeviceArgs {
    /// Revoke this device and clear local authorization material.
    Revoke {
        /// Select the Cloud Host to revoke (WhaleRein Cloud profiles only;
        /// rejected on a Standalone profile).
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
    },
    /// Forget this local identity after revoking its active pairing.
    Forget {
        /// Discard local identity even if server revocation cannot be verified.
        /// The old device must then be removed separately in host settings.
        #[arg(long)]
        local_only: bool,
    },
}

impl DeviceArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::Revoke { host } => (Command::Revoke, host),
            Self::Forget { local_only } => (Command::Forget { local_only }, None),
        }
    }
}

#[derive(Debug, Subcommand)]
enum OperationArgs {
    /// List unfinished command identities available for recovery.
    List,
    /// Recover a previously submitted command without repeating the action.
    Recover {
        #[arg(value_name = "REQUEST_ID")]
        request: String,
    },
}

impl OperationArgs {
    fn into_command(self) -> (Command, Option<String>) {
        match self {
            Self::List => (Command::CommandsPending, None),
            Self::Recover { request } => (Command::Recover { request }, None),
        }
    }
}

impl Cli {
    pub(crate) fn into_command(self) -> (Command, Option<String>) {
        self.command.into_command()
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Sign in to the selected WhaleRein Cloud environment in a browser.
    Login,
    /// Verify the account signed in to the selected environment.
    AccountStatus,
    AccountSessions,
    AccountRevokeAllSessions,
    AccountRevokeSession {
        id: String,
    },
    /// Clear this instance's Cloud sign-in.
    Logout,

    /// List Hosts for the selected Cloud or Standalone profile.
    Hosts,
    /// Probe one Host and report its authoritative device connection.
    HostStatus {
        host: Option<String>,
    },

    /// List sessions on one host.
    Sessions {
        host: String,
        all: bool,
    },

    /// List currently available source models.
    Models {
        host: String,
    },

    /// Inspect or change a session's model.
    Model {
        action: ModelAction,
    },

    /// Create or reuse a session according to the host's DSH rules.
    SessionCreate {
        host: String,
        cwd: Option<String>,
        workspace: Option<String>,
        preset: Option<String>,
        session_id: Option<String>,
    },

    /// List unfinished command identities available for recovery.
    CommandsPending,

    /// Recover a previously submitted command without repeating the action.
    Recover {
        request: String,
    },

    /// Fork the latest completed turn; the child shares the working directory.
    SessionFork {
        session: String,
    },

    /// Stream session output events.
    Tail {
        session: String,
        /// Exit after the first visible output (text or committed message).
        once: bool,
        /// Stop after this many seconds (decimals allowed, must be non-zero).
        /// `--once` defaults to 15 seconds.
        timeout: Option<String>,
    },

    /// Wait for native session quiescence or pending input.
    Wait {
        session: String,
        /// Overall deadline in seconds, with millisecond precision.
        timeout: Option<String>,
        /// Return exit code 7 when the observation times out.
        fail_on_timeout: bool,
    },

    /// Fill the local event cache from the host (gap fill or snapshot).
    Sync {
        session: String,
    },

    /// Read one bounded page of committed history, newest page first.
    History {
        session: String,
        cache: bool,
        continuation: Option<String>,
        message: Option<String>,
        max_messages: Option<u32>,
        max_bytes: Option<u32>,
    },

    /// Download one verified attachment reference shown by `session history`.
    SessionDownload {
        reference: String,
        to: PathBuf,
        force: bool,
        name: Option<String>,
        timeout: Option<String>,
        session: String,
    },

    /// Send a user message to a session.
    Send {
        session: String,
        /// Delivery mode. start-if-idle checks a sampled state and is not atomic.
        delivery: Delivery,
        /// Message text. It is sent, never logged or echoed. A text that starts
        /// with `-` is accepted; use `--` if it could also be read as a flag.
        text: String,
        detach: bool,
        timeout: Option<String>,
        operation_id: Option<String>,
    },

    /// List pending tool approvals for a session, or read one approval's state.
    Approvals {
        /// List pending approvals for this session (the default read mode).
        session: Option<String>,
        /// Read one approval's live or terminal state by id.
        request: Option<String>,
    },

    /// List pending human questions for a session.
    Questions {
        session: String,
        cursor: String,
    },

    /// Submit a complete structured answer from a file or standard input (-).
    Answer {
        session: String,
        batch: String,
        epoch: String,
        version: u64,
        digest: String,
        phase: String,
        input: PathBuf,
        /// Stable command identity for explicit cancellation and recovery.
        command_id: Option<String>,
    },

    /// Request cancellation of an in-flight open question answer.
    QuestionCancel {
        session: String,
        command: String,
    },

    /// Approve a pending tool request.
    Approve {
        request: String,
        /// SHA-256 digest shown by the host event.
        digest: String,
        version: i64,
    },

    /// Reject a pending tool request.
    Reject {
        request: String,
        digest: String,
        version: i64,
    },

    /// Inspect or change pending source work.
    Queue {
        action: QueueCommand,
    },

    /// Interrupt a session.
    Interrupt {
        session: String,
    },

    /// Revoke this device and clear local authorization material.
    Revoke,

    /// Forget this local identity after revoking its active pairing.
    Forget {
        /// Discard local identity even if server revocation cannot be verified.
        /// The old device must then be removed separately in host settings.
        local_only: bool,
    },

    /// Show the paired Access address and local unfinished command counts.
    Doctor {
        /// Remove the last failure's local diagnostic metadata.
        clear_diagnostics: bool,
        /// Write a redacted diagnostic bundle after confirmation.
        export: Option<PathBuf>,
    },
    Connect {
        host: Option<String>,
        invite_stdin: bool,
        fingerprint: Option<String>,
        name: Option<String>,
    },
    DefaultHost {
        host: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ModelAction {
    /// Read the recorded session choice and source fallback.
    Get {
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        #[arg(long)]
        session: String,
    },
    /// Select a session model and attempt to update the source default.
    Set {
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
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

impl ModelAction {
    fn take_host(mut self) -> (Self, Option<String>) {
        let host = match &mut self {
            Self::Get { host, .. } => host.take(),
            Self::Set { host, .. } => host.take(),
        };
        (self, host)
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum QueueCommand {
    /// Read the current source queue without activating a cold session.
    List {
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        #[arg(long)]
        session: String,
    },
    /// Replace the entire content of an item; attachments are removed.
    Edit {
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        item: String,
        /// Replacement text. Hyphen-leading text is accepted; use `--` when it
        /// could also be read as a flag.
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
    /// Withdraw one queued item.
    Remove {
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        item: String,
    },
    /// Promote an eligible item into the currently running turn.
    Steer {
        #[arg(long, value_name = "HOST_ID")]
        host: Option<String>,
        #[arg(long)]
        session: String,
        #[arg(long)]
        item: String,
    },
}

impl QueueCommand {
    fn take_host(mut self) -> (Self, Option<String>) {
        let host = match &mut self {
            Self::List { host, .. } => host.take(),
            Self::Edit { host, .. } => host.take(),
            Self::Remove { host, .. } => host.take(),
            Self::Steer { host, .. } => host.take(),
        };
        (self, host)
    }
}

impl Command {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::SessionDownload { session, .. }
            | Self::SessionFork { session }
            | Self::Tail { session, .. }
            | Self::Wait { session, .. }
            | Self::Sync { session }
            | Self::History { session, .. }
            | Self::Send { session, .. }
            | Self::Questions { session, .. }
            | Self::Answer { session, .. }
            | Self::QuestionCancel { session, .. }
            | Self::Interrupt { session } => Some(session),
            Self::Approvals { session, .. } => session.as_deref(),
            Self::Model {
                action: ModelAction::Get { session, .. } | ModelAction::Set { session, .. },
            } => Some(session),
            Self::Queue {
                action:
                    QueueCommand::List { session, .. }
                    | QueueCommand::Edit { session, .. }
                    | QueueCommand::Remove { session, .. }
                    | QueueCommand::Steer { session, .. },
            } => Some(session),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::AccountStatus => "status",
            Self::AccountSessions => "account.sessions",
            Self::AccountRevokeAllSessions => "account.revoke-all-sessions",
            Self::AccountRevokeSession { .. } => "account.revoke-session",
            Self::Connect { .. } => "host.connect",
            Self::DefaultHost { .. } => "host.default",
            Self::Logout => "logout",
            Self::Hosts => "host.list",
            Self::HostStatus { .. } => "host.status",
            Self::Sessions { .. } => "session.list",
            Self::Models { .. } => "session.models",
            Self::Model {
                action: ModelAction::Get { .. },
            } => "session.model.get",
            Self::Model {
                action: ModelAction::Set { .. },
            } => "session.model.set",
            Self::SessionCreate { .. } => "session.create",
            Self::SessionFork { .. } => "session.fork",
            Self::CommandsPending => "operation.list",
            Self::Recover { .. } => "operation.recover",
            Self::Tail { .. } => "session.watch",
            Self::Wait { .. } => "session.wait",
            Self::Sync { .. } => "session.show",
            Self::History { .. } => "session.history",
            Self::SessionDownload { .. } => "session.download",
            Self::Send { .. } => "session.send",
            Self::Approvals { .. } => "session.approval.list",
            Self::Questions { .. } => "session.question.list",
            Self::Answer { .. } => "session.question.answer",
            Self::QuestionCancel { .. } => "session.question.cancel",
            Self::Approve { .. } => "session.approval.approve",
            Self::Reject { .. } => "session.approval.reject",
            Self::Interrupt { .. } => "session.interrupt",
            Self::Queue { .. } => "session.queue",
            Self::Revoke => "device.revoke",
            Self::Forget { .. } => "device.forget",
            Self::Doctor { .. } => "doctor",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_commands_select_only_a_leaf_host() {
        let cli = Cli::try_parse_from([
            "whalerein",
            "session",
            "send",
            "s",
            "hello",
            "--host",
            "h",
            "--detach",
        ])
        .unwrap();
        let (command, host) = cli.into_command();
        assert_eq!(host.as_deref(), Some("h"));
        assert!(matches!(command, Command::Send { session, detach: true, .. } if session == "s"));
        assert!(Cli::try_parse_from(["whalerein", "--host", "h", "session", "list"]).is_err());
    }

    #[test]
    fn old_startup_and_flat_commands_are_rejected() {
        for command in [
            "config",
            "profile",
            "pair",
            "hosts",
            "send",
            "account-status",
        ] {
            assert!(
                Cli::try_parse_from(["whalerein", command]).is_err(),
                "{command}"
            );
        }
        for flag in ["--environment", "--data-dir"] {
            assert!(
                Cli::try_parse_from(["whalerein", flag, "value", "status"]).is_err(),
                "{flag}"
            );
        }
    }

    #[test]
    fn connection_needs_exactly_one_input() {
        assert!(Cli::try_parse_from(["whalerein", "host", "connect", "--invite-stdin"]).is_ok());
        assert!(Cli::try_parse_from(["whalerein", "host", "connect", "h"]).is_ok());
        assert!(Cli::try_parse_from(["whalerein", "host", "connect"]).is_err());
        assert!(
            Cli::try_parse_from(["whalerein", "host", "connect", "h", "--invite-stdin"]).is_err()
        );
    }

    #[test]
    fn reasoning_is_a_global_opt_in_and_history_selectors_are_exclusive() {
        let (command, _) =
            Cli::try_parse_from(["whalerein", "--show-reasoning", "session", "history", "s"])
                .unwrap()
                .into_command();
        assert!(matches!(command, Command::History { session, .. } if session == "s"));
        assert!(
            Cli::try_parse_from(["whalerein", "session", "history", "s", "--show-reasoning"])
                .is_ok()
        );
        assert!(Cli::try_parse_from([
            "whalerein",
            "session",
            "history",
            "s",
            "--message",
            "m",
            "--continuation",
            "c"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "whalerein",
            "session",
            "history",
            "s",
            "--message",
            "m",
            "--cache"
        ])
        .is_err());
        // A cache-sourced continuation is read with `--cache`; the combination
        // is the only shape that can page older cached history.
        let cached = Cli::try_parse_from([
            "whalerein",
            "session",
            "history",
            "s",
            "--cache",
            "--continuation",
            "c",
        ])
        .unwrap();
        let (command, _) = cached.into_command();
        assert!(matches!(
            command,
            Command::History {
                cache: true,
                continuation: Some(ref token),
                ..
            } if token.as_str() == "c"
        ));
        assert!(Cli::try_parse_from([
            "whalerein",
            "session",
            "download",
            "ref",
            "--to",
            "dir",
            "--force",
            "--session",
            "s",
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["whalerein", "session", "download", "ref"]).is_err());
        // The scoped save action requires the owning session explicitly.
        assert!(
            Cli::try_parse_from(["whalerein", "session", "download", "ref", "--to", "dir",])
                .is_err()
        );
    }

    #[test]
    fn each_group_exposes_the_documented_leaf_commands() {
        let cases: &[(&[&str], &str)] = &[
            (&["login"], "login"),
            (&["logout"], "logout"),
            (&["status"], "status"),
            (&["doctor", "--export", "out.json"], "doctor"),
            (&["host", "list"], "host.list"),
            (&["host", "connect", "host"], "host.connect"),
            (&["host", "default", "host"], "host.default"),
            (&["session", "list", "--host", "host"], "session.list"),
            (
                &["session", "create", "--workspace", "shared"],
                "session.create",
            ),
            (&["session", "show", "id"], "session.show"),
            (&["session", "history", "id"], "session.history"),
            (&["session", "history", "id", "--cache"], "session.history"),
            (
                &[
                    "session",
                    "download",
                    "attachment-ref",
                    "--to",
                    "downloads",
                    "--session",
                    "id",
                ],
                "session.download",
            ),
            (&["session", "fork", "id"], "session.fork"),
            (&["session", "watch", "id", "--once"], "session.watch"),
            (&["session", "wait", "id"], "session.wait"),
            (&["session", "interrupt", "id"], "session.interrupt"),
            (&["session", "models"], "session.models"),
            (
                &["session", "model", "get", "--session", "id"],
                "session.model.get",
            ),
            (
                &[
                    "session",
                    "model",
                    "set",
                    "--session",
                    "id",
                    "--provider",
                    "p",
                    "--model",
                    "m",
                ],
                "session.model.set",
            ),
            (
                &["session", "queue", "list", "--session", "id"],
                "session.queue",
            ),
            (
                &[
                    "session",
                    "queue",
                    "edit",
                    "--session",
                    "id",
                    "--item",
                    "i",
                    "fixture",
                ],
                "session.queue",
            ),
            (
                &[
                    "session",
                    "queue",
                    "remove",
                    "--session",
                    "id",
                    "--item",
                    "i",
                ],
                "session.queue",
            ),
            (
                &[
                    "session",
                    "queue",
                    "steer",
                    "--session",
                    "id",
                    "--item",
                    "i",
                ],
                "session.queue",
            ),
            (
                &["session", "approval", "list", "--session", "id"],
                "session.approval.list",
            ),
            (
                &[
                    "session",
                    "approval",
                    "approve",
                    "--request",
                    "a",
                    "--digest",
                    "ff",
                    "--version",
                    "1",
                ],
                "session.approval.approve",
            ),
            (
                &[
                    "session",
                    "approval",
                    "reject",
                    "--request",
                    "a",
                    "--digest",
                    "ff",
                    "--version",
                    "1",
                ],
                "session.approval.reject",
            ),
            (
                &["session", "question", "list", "--session", "id"],
                "session.question.list",
            ),
            (
                &[
                    "session",
                    "question",
                    "answer",
                    "--session",
                    "id",
                    "--batch",
                    "b",
                    "--epoch",
                    "e",
                    "--version",
                    "1",
                    "--digest",
                    "ff",
                    "--phase",
                    "QUESTION_OPEN",
                    "--input",
                    "answer.json",
                ],
                "session.question.answer",
            ),
            (
                &[
                    "session",
                    "question",
                    "cancel",
                    "--session",
                    "id",
                    "--command",
                    "c",
                ],
                "session.question.cancel",
            ),
            (&["device", "revoke", "--host", "h"], "device.revoke"),
            (&["device", "forget", "--local-only"], "device.forget"),
            (&["operation", "list"], "operation.list"),
            (&["operation", "recover", "id"], "operation.recover"),
        ];
        for (args, expected) in cases {
            let cli =
                Cli::try_parse_from(["whalerein"].into_iter().chain(args.iter().copied())).unwrap();
            let (command, _) = cli.into_command();
            assert_eq!(&command.name(), expected, "{args:?}");
        }
    }
}
