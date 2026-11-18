use serde_json::json;
use whalerein_core::api::{AccountState, ClientService, Core, CoreResult, DeviceAccess};

use crate::cli::Command;
use crate::output::{render_account_hosts, CliOutcome};

pub(crate) async fn run(core: &Core, command: &Command) -> CoreResult<CliOutcome> {
    Ok(match command {
        Command::Login => {
            let account = core
                .login(|prompt| {
                    eprintln!("Open {} in your browser.", prompt.verification_uri);
                    eprintln!("Enter the code shown here: {}", prompt.user_code);
                })
                .await?;
            CliOutcome::success(
                command,
                json!({"account": account.account, "workspace": account.workspace}),
                "Signed in. Run `whalerein host list` to find a Host.",
            )
        }
        Command::AccountStatus => {
            let status = core.status().await?;
            let label = match status.service {
                ClientService::WhaleReinCloud => "WhaleRein Cloud",
                ClientService::Standalone => "Standalone",
            };
            let account_state = match status.account_state {
                AccountState::NotApplicable => "not applicable",
                AccountState::SignedIn => "signed in",
                AccountState::SignedOut => "signed out",
            };
            let device_access = match status.device_access {
                DeviceAccess::Unpaired => "unpaired",
                DeviceAccess::Ready => "ready",
                DeviceAccess::RevocationPending => "revocation pending",
                DeviceAccess::Revoked => "revoked",
                DeviceAccess::Unavailable => "unavailable",
            };
            let environment_suffix = status
                .testing_environment
                .map_or_else(String::new, |value| format!(" ({value}, testing)"));
            let human = format!(
                "Configuration: {}\nData directory: {}\nService: {label}{environment_suffix}\nAccount: {account_state}\nDevice access: {device_access}\nDefault Host: {}",
                status.config.display(),
                status.data_dir.display(),
                status.default_host.as_deref().unwrap_or("none"),
            );
            let data = json!(status);
            CliOutcome::success(command, data, human)
        }
        Command::Hosts => {
            let hosts = core.account_hosts().await?;
            CliOutcome::success(
                command,
                json!({"hosts": hosts}),
                render_account_hosts(&hosts),
            )
        }
        Command::Logout => {
            core.logout().await?;
            CliOutcome::success(command, json!({"signed_out": true}), "Signed out.")
        }
        _ => unreachable!("only account commands are dispatched here"),
    })
}
