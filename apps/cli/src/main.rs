//! WhaleRein CLI entry point. The binary is intentionally thin: arguments,
//! stable output and exit codes only.
//!
//! Parsing happens here instead of `Cli::parse()` so a usage error can still
//! produce the `whalerein/client/v1` envelope when `--json` appears anywhere in argv.
//! Plain (non-JSON) invocations keep clap's own help and error text.

mod account;
mod bench;
mod cli;
mod diagnostics;
mod output;
mod run;
mod signal;
mod stream;

use std::borrow::Cow;
use std::process::ExitCode;
use std::time::Instant;

use clap::error::ErrorKind;
use clap::Parser;
use serde_json::json;
use whalerein_core::api::DiagnosticContext;

use crate::cli::Cli;
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
        Err(error) => return ExitCode::from(finish_parse_error(error, wants_json)),
    };
    let json = parsed.json;
    let (diagnostics, completion) = diagnostics::CliDiagnosticSink::start(parsed.verbose, json);
    // One process outlet is shared by Core facts and CLI write stamps. `off`
    // and `aggregate` compose a disabled context and create no writer.
    let bench = bench::BenchSetup::from_env();
    let context = DiagnosticContext::new(diagnostics.clone()).with_bench(bench.core_context());
    let trace_id = context.trace_id().to_owned();
    let started = Instant::now();
    let outcome = run::run(parsed, &diagnostics, context).await;
    let exit = finish(json, &outcome);
    diagnostics.cli_event(&trace_id, || {
        json!({
            "event": "command_completed",
            "command": outcome.command,
            "exit_code": exit,
            "outcome_exit_code": outcome.exit,
            "code": outcome.error.as_ref().map(|error| &error.code),
            "elapsed_ms": started.elapsed().as_millis() as u64,
        })
    });
    diagnostics.finish(completion);
    // Flush and fsync the observation outlet with a bounded shutdown. A
    // stalled writer cannot change the CLI exit code.
    bench.finish();
    ExitCode::from(exit)
}

/// `--json` is global, so it may appear before or after the subcommand. Only
/// the argument region matters: after a bare `--` everything is message text.
fn argv_requests_json(args: &[String]) -> bool {
    args.iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument == "--json")
}

/// Write one outcome and map a failed write to an exit code.
fn finish(json: bool, outcome: &CliOutcome) -> u8 {
    let (text, to_stdout) = if json {
        match serde_json::to_string(&outcome.to_json()) {
            Ok(line) => (Cow::Owned(format!("{line}\n")), true),
            Err(_) => {
                let _ = stream::to_stderr(b"error: output could not be encoded (internal)\n");
                return 1;
            }
        }
    } else if outcome.error.is_some() {
        (Cow::Owned(outcome.render()), false)
    } else if !outcome.human.is_empty() {
        (Cow::Borrowed(outcome.human.as_str()), true)
    } else {
        return outcome.exit;
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

fn exit_for(exit: u8, written: &Result<(), stream::WriteFailure>, to_stdout: bool) -> u8 {
    match written {
        Ok(()) => exit,
        // A closed consumer (`head -1`) ends the stream without a panic. It is
        // success for stdout; a closed stderr keeps the command's own failure.
        Err(failure) if failure.is_closed() => {
            if to_stdout {
                0
            } else {
                exit
            }
        }
        Err(_) if to_stdout => 1,
        Err(_) => exit,
    }
}

/// Render a clap parse result. Help and version are successful local output;
/// every other kind is an `invalid_argument` usage error (exit 2).
fn finish_parse_error(error: clap::Error, json: bool) -> u8 {
    let kind = error.kind();
    let text = error.to_string();
    match kind {
        ErrorKind::DisplayVersion => {
            let version = format!("whalerein {}", env!("CARGO_PKG_VERSION"));
            if json {
                return finish(
                    true,
                    &CliOutcome {
                        command: "cli".to_owned(),
                        data: Some(json!({ "version": version })),
                        error: None,
                        human: version,
                        exit: 0,
                    },
                );
            }
            let _ = stream::to_stdout(text.as_bytes());
            0
        }
        ErrorKind::DisplayHelp => {
            if json {
                return finish(
                    true,
                    &CliOutcome {
                        command: "cli".to_owned(),
                        data: Some(json!({ "help": text })),
                        error: None,
                        human: text,
                        exit: 0,
                    },
                );
            }
            let _ = stream::to_stdout(text.as_bytes());
            0
        }
        _ => {
            let message = usage_message(&error);
            if json {
                return finish(
                    true,
                    &CliOutcome::cli_failure("invalid_argument", message, 2),
                );
            }
            let _ = stream::to_stderr(format!("error: {message}\n").as_bytes());
            2
        }
    }
}

fn usage_message(error: &clap::Error) -> String {
    if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        return "Run `whalerein --help` for usage.".to_owned();
    }
    "Invalid command or arguments. Run the command with `--help` for usage.".to_owned()
}
