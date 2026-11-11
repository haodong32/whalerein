//! WhaleRein CLI entry point. The binary is intentionally thin: arguments,
//! stable output and exit codes only.

mod cli;
mod output;
mod run;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let parsed = Cli::parse();
    let json = parsed.json;
    let outcome = run::run(parsed).await;

    if json {
        match serde_json::to_string(&outcome.to_json()) {
            Ok(line) => println!("{line}"),
            Err(_) => eprintln!("error: output could not be encoded (internal)"),
        }
    } else if outcome.error.is_some() {
        eprint!("{}", outcome.render());
    } else if !outcome.human.is_empty() {
        print!("{}", outcome.human);
    }

    ExitCode::from(outcome.exit)
}

fn init_tracing() {
    let filter = std::env::var("WHALEREIN_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "warn".to_owned());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false);
    let _ = subscriber.try_init();
}
