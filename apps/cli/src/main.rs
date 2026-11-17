//! WhaleRein CLI entry point. The binary is intentionally thin: arguments,
//! stable output and exit codes only.
//!
//! Parsing happens here instead of `Cli::parse()` so a usage error can still
//! produce the `whalerein/v1` envelope when `--json` appears anywhere in argv.
//! Plain (non-JSON) invocations keep clap's own help and error text.

mod cli;
mod config;
mod env;
mod output;
mod run;
mod signal;
mod stream;

use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::Parser;
use serde_json::json;

use crate::cli::{Cli, Command};
use crate::output::CliOutcome;

fn main() -> ExitCode {
    // Windows gives the main thread a 1 MiB stack while the pairing future is
    // larger than that once the Access client and TLS state are inlined. Run
    // the async entry point on a dedicated thread with the same headroom the
    // other platforms provide instead of overflowing before the first output.
    std::thread::Builder::new()
        .name("whalerein".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(async_main())
        })
        .expect("cli thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

async fn async_main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let wants_json = argv_requests_json(&args);
    // Help and version stay usable even when the environment is misconfigured:
    // they are the documented way to discover the correct variables.
    let parsed = match Cli::try_parse_from(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            init_tracing();
            return finish_parse_error(error, &args, wants_json);
        }
    };
    // Validate every environment-provided value before any command can use it.
    let env_issue = env::validate().err();
    init_tracing();
    if let Some(issue) = env_issue {
        return finish(
            wants_json,
            CliOutcome::cli_failure("invalid_configuration", issue.message(), 2),
        );
    }
    let json = parsed.json;
    finish(json, run::run(parsed).await)
}

/// `--json` is global, so it may appear before or after the subcommand. Only
/// the argument region matters: after a bare `--` everything is message text.
fn argv_requests_json(args: &[String]) -> bool {
    args.iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument == "--json")
}

/// Write one outcome and map a failed write to an exit code.
fn finish(json: bool, outcome: CliOutcome) -> ExitCode {
    if json {
        return match serde_json::to_string(&outcome.to_json()) {
            Ok(line) => {
                let written = stream::to_stdout(format!("{line}\n").as_bytes());
                report_write_error(&written);
                exit_for(outcome.exit, &written, true)
            }
            Err(_) => {
                let _ = stream::to_stderr(b"error: output could not be encoded (internal)\n");
                ExitCode::from(1)
            }
        };
    }
    let (text, to_stdout) = if outcome.error.is_some() {
        (outcome.render(), false)
    } else if !outcome.human.is_empty() {
        (outcome.human.clone(), true)
    } else {
        return ExitCode::from(outcome.exit);
    };
    let written = if to_stdout {
        stream::to_stdout(text.as_bytes())
    } else {
        stream::to_stderr(text.as_bytes())
    };
    report_write_error(&written);
    exit_for(outcome.exit, &written, to_stdout)
}

/// Describe a non-pipe write failure on the other stream. Never panics.
fn report_write_error(written: &Result<(), stream::WriteFailure>) {
    if let Err(stream::WriteFailure::Failed(error)) = written {
        let _ =
            stream::to_stderr(format!("error: output could not be written: {error}\n").as_bytes());
    }
}

fn exit_for(exit: u8, written: &Result<(), stream::WriteFailure>, to_stdout: bool) -> ExitCode {
    match written {
        Ok(()) => ExitCode::from(exit),
        // A closed consumer (`head -1`) ends the stream without a panic. It is
        // success for stdout; a closed stderr keeps the command's own failure.
        Err(failure) if failure.is_closed() => ExitCode::from(if to_stdout { 0 } else { exit }),
        Err(_) if to_stdout => ExitCode::from(1),
        Err(_) => ExitCode::from(exit),
    }
}

/// Render a clap parse result. Help and version are successful local output;
/// every other kind is an `invalid_argument` usage error (exit 2).
fn finish_parse_error(error: clap::Error, args: &[String], json: bool) -> ExitCode {
    let kind = error.kind();
    let text = error.to_string();
    match kind {
        ErrorKind::DisplayVersion => {
            let version = format!("whalerein {}", env!("CARGO_PKG_VERSION"));
            if json {
                return finish(
                    true,
                    CliOutcome {
                        command: "cli".to_owned(),
                        data: Some(json!({ "version": version })),
                        error: None,
                        human: version,
                        exit: 0,
                    },
                );
            }
            let _ = stream::to_stdout(text.as_bytes());
            ExitCode::SUCCESS
        }
        ErrorKind::DisplayHelp => {
            if json {
                return finish(
                    true,
                    CliOutcome {
                        command: "cli".to_owned(),
                        data: Some(json!({ "help": text })),
                        error: None,
                        human: text,
                        exit: 0,
                    },
                );
            }
            let _ = stream::to_stdout(text.as_bytes());
            ExitCode::SUCCESS
        }
        _ => {
            let message = usage_message(&error, args);
            if json {
                return finish(
                    true,
                    CliOutcome::cli_failure("invalid_argument", message, 2),
                );
            }
            let _ = stream::to_stderr(format!("error: {message}\n").as_bytes());
            ExitCode::from(2)
        }
    }
}

/// A `--host` after a session subcommand is rejected by clap with a `-- --host`
/// suggestion. That suggestion would send the flag as message text, so the
/// usage error is rewritten to point at the only correct position.
fn usage_message(error: &clap::Error, args: &[String]) -> String {
    if misplaced_host(args) {
        return "place --host before the command: `whalerein --host HOST_ID <command> ...`. \
A --host placed after the command is not a global option and would otherwise be \
read as message text."
            .to_owned();
    }
    if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        return "a command is required; run `whalerein --help` for usage".to_owned();
    }
    error.to_string().trim().to_owned()
}

fn misplaced_host(args: &[String]) -> bool {
    let end = args
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(args.len());
    let Some((index, name)) = args[..end]
        .iter()
        .enumerate()
        .find_map(|(index, argument)| {
            Command::known_names()
                .contains(&argument.as_str())
                .then_some((index, argument.as_str()))
        })
    else {
        return false;
    };
    // These subcommands own a required `--host` of their own.
    if matches!(name, "sessions" | "models" | "session-create") {
        return false;
    }
    args[index + 1..end]
        .iter()
        .any(|argument| argument == "--host")
}

fn init_tracing() {
    let filter = env::log_filter().unwrap_or_else(|_| "warn".to_owned());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false);
    let _ = subscriber.try_init();
}
