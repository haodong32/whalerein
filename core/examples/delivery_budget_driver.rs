//! Test-only Core driver for bounded delivery acceptance without command-line payloads.
use std::path::PathBuf;
use std::sync::Arc;
use whalerein_core::api::KeyringSecretStore;
use whalerein_core::api::{CommandHandle, CoreError, DeliveryMode, PendingQueueAction, SessionKey};
use whalerein_core::Core;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 && args.len() != 6 {
        std::process::exit(2);
    }
    let size: usize = args[4].parse().expect("fixture size");
    let core = Core::open(
        &PathBuf::from(&args[1]),
        Arc::new(KeyringSecretStore::new("whalerein")),
    )
    .expect("fixture Core");
    if let Err(error) = core.sessions(&args[2]).await {
        println!("{{\"error\":\"{}\"}}", error.code());
        std::process::exit(1);
    }
    let text = "x".repeat(size);
    let key = SessionKey {
        host_id: args[2].clone(),
        session_id: args[3].clone(),
    };
    let result = if args.len() == 6 {
        core.update_pending_queue(&key, &args[5], PendingQueueAction::Edit, &text)
            .await
    } else {
        send(&core, &key, &text).await
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

async fn send(core: &Core, target: &SessionKey, text: &str) -> Result<CommandHandle, CoreError> {
    let operation_id = Core::new_command_id();
    let operation = core
        .submit_message_operation(&operation_id, target, text, DeliveryMode::EnqueueAfterRun)
        .await?;
    let receipt = operation.receipt.ok_or(CoreError::UnknownResult)?;
    if receipt.status.is_terminal() {
        return Ok(receipt);
    }
    core.recover_send_operation(&operation_id)
        .await?
        .receipt
        .ok_or(CoreError::UnknownResult)
}
