//! Test driver for encrypted command admission, size limits and recovery.
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

    let core = Core::open(
        &PathBuf::from(&args[1]),
        Arc::new(KeyringSecretStore::new("whalerein")),
    )
    .expect("fixture Core");
    if let Err(error) = core.sessions(&args[2]).await {
        println!("{{\"error\":\"{}\"}}", error.code());
        std::process::exit(1);
    }
    let text = if args[4] == "--read-after-ready" {
        use std::io::Write;
        println!("ready");
        std::io::stdout().flush().expect("signal ready");
        let mut text = String::new();
        assert!(std::io::stdin().read_line(&mut text).expect("read fixture") > 0);
        text.trim_end().to_owned()
    } else {
        "x".repeat(args[4].parse().expect("fixture size"))
    };
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
    let mut receipt = operation.receipt.ok_or(CoreError::UnknownResult)?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !receipt.status.is_terminal() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            receipt = core
                .recover_send_operation(&operation_id)
                .await?
                .receipt
                .ok_or(CoreError::UnknownResult)?;
        }
        Ok(receipt)
    })
    .await
    .map_err(|_| CoreError::Timeout)?
}
