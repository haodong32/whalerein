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
                AccountState::SignedOut if status.remote_signout_pending => {
                    "signed out (remote sign-out pending)"
                }
                AccountState::SignedOut => "signed out",
            };
            let device_access = match status.device_access {
                DeviceAccess::Unpaired => "unpaired",
                DeviceAccess::Ready => "ready",
                DeviceAccess::RevocationPending => "revocation pending",
                DeviceAccess::Revoked => "revoked",
                DeviceAccess::Unavailable => "unavailable",
                DeviceAccess::SignInRequired => "sign in to access Hosts",
            };
            let environment_suffix = status
                .testing_environment
                .map_or_else(String::new, |value| format!(" ({value}, testing)"));
            let signout_notice = if status.remote_signout_pending
                && matches!(status.account_state, AccountState::SignedIn)
            {
                "\nPrevious remote sign-out is pending."
            } else {
                ""
            };
            let human = format!(
                "Configuration: {}\nData directory: {}\nService: {label}{environment_suffix}\nAccount: {account_state}\nDevice access: {device_access}\nDefault Host: {}{signout_notice}",
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
        Command::AccountSessions => {
            let sessions = core.cloud_account_sessions().await?;
            let human = if sessions.is_empty() {
                "No Cloud sessions.".into()
            } else {
                sessions
                    .iter()
                    .map(|session| {
                        format!(
                            "{}  {}  {}",
                            session.session_id,
                            session.instance_id,
                            session.state.as_str()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            CliOutcome::success(command, json!({"sessions": sessions}), human)
        }
        Command::AccountRevokeAllSessions => {
            let state = core.revoke_all_cloud_account_sessions().await?;
            let human = if state == whalerein_core::api::AccountSessionState::Revoked {
                "All Cloud sessions signed out."
            } else {
                "Signed out on this device. To confirm sign-out for all apps, check App sign-ins in the console."
            };
            CliOutcome::success(command, json!({"state": state}), human)
        }
        Command::AccountRevokeSession { id } => {
            let state = core.revoke_cloud_account_session(id).await?;
            let human = if state == whalerein_core::api::AccountSessionState::Revoked {
                "Cloud session signed out."
            } else {
                "Remote sign-out is pending."
            };
            CliOutcome::success(command, json!({"session_id": id, "state": state}), human)
        }
        Command::Logout => {
            let complete = core.logout().await?;
            let message = if complete {
                "Signed out."
            } else {
                "Signed out on this device. Remote sign-out is pending."
            };
            CliOutcome::success(
                command,
                json!({"signed_out": true, "remote_signout_pending": !complete}),
                message,
            )
        }
        _ => unreachable!("only account commands are dispatched here"),
    })
}
