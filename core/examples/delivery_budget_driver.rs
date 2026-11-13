//! Test-only Core driver for bounded delivery acceptance without command-line payloads.
use std::path::PathBuf;
use std::sync::Arc;
use whalerein_core::api::PendingQueueAction;
use whalerein_core::{Core, CoreOptions, FileSecretStore};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 && args.len() != 7 {
        std::process::exit(2);
    }
    let size: usize = args[5].parse().expect("fixture size");
    let core = Core::new(CoreOptions {
        data_dir: PathBuf::from(&args[1]),
        secrets: Arc::new(FileSecretStore::new(PathBuf::from(&args[2]))),
    })
    .expect("fixture Core");
    if let Err(error) = core.sessions(&args[3]).await {
        println!("{{\"error\":\"{}\"}}", error.code());
        std::process::exit(1);
    }
    let text = "x".repeat(size);
    let result = if args.len() == 7 {
        core.update_pending_queue(&args[4], &args[6], PendingQueueAction::Edit, &text)
            .await
    } else {
        core.send_message(&args[4], &text).await
    };
    match result {
        Ok(handle) => println!(
            "{{\"status\":\"{}\",\"has_delivery_id\":{}}}",
            handle.status.as_str(),
            handle.delivery_id.is_some()
        ),
        Err(error) => {
            println!("{{\"error\":\"{}\"}}", error.code());
            std::process::exit(1);
        }
    }
}
