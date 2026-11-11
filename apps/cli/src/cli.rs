//! Command-line arguments. The CLI only parses user input; all protocol work
//! lives in `whalerein-core`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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

    /// List hosts authorized for this device.
    Hosts,

    /// List sessions on one host.
    Sessions {
        #[arg(long, value_name = "HOST_ID")]
        host: String,
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

    /// Fill the local event cache from the host (gap fill or snapshot).
    Sync {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
    },

    /// Send a user message to a session.
    Send {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
        /// Message text. It is sent, never logged or echoed.
        text: String,
    },

    /// List pending tool approvals for a session.
    Approvals {
        #[arg(long, value_name = "SESSION_ID")]
        session: String,
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

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pair { .. } => "pair",
            Self::Hosts => "hosts",
            Self::Sessions { .. } => "sessions",
            Self::Tail { .. } => "tail",
            Self::Sync { .. } => "sync",
            Self::Send { .. } => "send",
            Self::Approvals { .. } => "approvals",
            Self::Approve { .. } => "approve",
            Self::Reject { .. } => "reject",
            Self::Interrupt { .. } => "interrupt",
            Self::Revoke => "revoke",
            Self::Forget { .. } => "forget",
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
            Command::Send { session, text } if session == "s" && text == "hello"
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
