use std::io::ErrorKind;

use super::*;
use crate::api::Environment;
use crate::configuration::writer;

const DEFAULT_HOST: &str = "default-host.json";

impl Core {
    pub fn default_host(&self) -> CoreResult<Option<String>> {
        let path = self.data_dir().join(DEFAULT_HOST);
        match std::fs::read(path) {
            Ok(bytes) => {
                let host: String = serde_json::from_slice(&bytes)
                    .map_err(|_| CoreError::Config("default Host record is invalid".into()))?;
                if host.trim().is_empty() {
                    return Err(CoreError::Config("default Host record is invalid".into()));
                }
                Ok(Some(host))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err(CoreError::Config("could not read default Host".into())),
        }
    }

    pub async fn select_default_host(&self, host: &str) -> CoreResult<()> {
        self.ensure_authorized_host(host)?;
        // Only publish a target after its own round trip succeeds.
        self.host_capabilities(host).await?;
        self.set_default_host(host)
    }

    pub(crate) fn set_default_host(&self, host: &str) -> CoreResult<()> {
        self.ensure_instance_state()?;
        let bytes = serde_json::to_vec(host)
            .map_err(|_| CoreError::Internal("could not encode default Host".into()))?;
        let path = self.data_dir().join(DEFAULT_HOST);
        let _lock = writer::ConfigLock::acquire(&writer::lock_path(&path))?;
        writer::write_atomically(&path, &bytes)?;
        Ok(())
    }

    pub async fn resolve_operation_host(&self, explicit: Option<&str>) -> CoreResult<String> {
        if let Some(host) = explicit {
            self.ensure_authorized_host(host)?;
            return Ok(host.to_owned());
        }
        if let Some(host) = self.default_host()? {
            self.ensure_authorized_host(&host)
                .map_err(|error| match error {
                    CoreError::NotFound(_) => CoreError::NotFound(
                        "default Host is no longer authorized; select a Host with --host".into(),
                    ),
                    error => error,
                })?;
            return Ok(host);
        }
        let hosts = self.inspect_regional_hosts().await?;
        let mut reachable = hosts
            .iter()
            .filter(|host| host.online && self.ensure_authorized_host(&host.host_id).is_ok());
        let Some(selected) = reachable.next() else {
            return Err(CoreError::Unavailable(
                "no online authorized Host; inspect host list or select --host".into(),
            ));
        };
        if reachable.next().is_some() {
            return Err(CoreError::Conflict(
                "multiple online Hosts; select one with --host".into(),
            ));
        }
        self.ensure_authorized_host(&selected.host_id)?;
        Ok(selected.host_id.clone())
    }

    pub async fn operation_session_key(
        &self,
        explicit: Option<&str>,
        session: &str,
    ) -> CoreResult<SessionKey> {
        let host = self.resolve_operation_host(explicit).await?;
        SessionKey::new(host, session)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SendOperation {
    pub operation_id: String,
    pub target: SessionKey,
    pub receipt: Option<CommandHandle>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SendRecord {
    outcome: SendOperation,
    device_id: String,
    access_url: String,
    host_fingerprint: String,
    intent: String,
}

impl Core {
    fn operation_dir(&self) -> std::path::PathBuf {
        self.data_dir().join("send-operations")
    }

    fn send_record_path(&self, id: &str) -> CoreResult<std::path::PathBuf> {
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CoreError::InvalidInput("invalid operation ID".into()));
        }
        Ok(self.operation_dir().join(format!("{id}.json")))
    }

    fn read_send_record(&self, id: &str) -> CoreResult<SendRecord> {
        let path = self.send_record_path(id)?;
        let bytes = std::fs::read(path).map_err(|error| match error.kind() {
            ErrorKind::NotFound => CoreError::NotFound("send operation does not exist".into()),
            _ => CoreError::Config("could not read operation record".into()),
        })?;
        let record: SendRecord = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Config("invalid operation record".into()))?;
        if record.outcome.operation_id != id {
            return Err(CoreError::Conflict("invalid operation identity".into()));
        }
        Ok(record)
    }

    fn write_send_record(&self, record: &SendRecord) -> CoreResult<()> {
        let bytes = serde_json::to_vec(record)
            .map_err(|_| CoreError::Internal("could not encode send operation".into()))?;
        writer::write_atomically(
            &self.send_record_path(&record.outcome.operation_id)?,
            &bytes,
        )?;
        Ok(())
    }

    pub fn send_operation(&self, id: &str) -> CoreResult<SendOperation> {
        Ok(self.read_send_record(id)?.outcome)
    }

    pub fn send_operations(&self) -> CoreResult<Vec<SendOperation>> {
        let files = match std::fs::read_dir(self.operation_dir()) {
            Ok(files) => files,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(CoreError::Config("could not list send operations".into())),
        };
        let mut outcomes = Vec::new();
        for entry in files {
            let entry = entry.map_err(|_| CoreError::Config("could not read operation".into()))?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let id = path
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str)
                    .ok_or_else(|| CoreError::Config("invalid operation filename".into()))?;
                outcomes.push(self.read_send_record(id)?.outcome);
            }
        }
        outcomes.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
        Ok(outcomes)
    }

    pub async fn submit_message_operation(
        &self,
        operation_id: &str,
        key: &SessionKey,
        text: &str,
        mode: DeliveryMode,
    ) -> CoreResult<SendOperation> {
        self.authorized_session(key)?;
        if text.trim().is_empty() {
            return Err(CoreError::InvalidInput("message is empty".into()));
        }
        let config = self.require_config()?;
        let path = self.send_record_path(operation_id)?;
        let intent = self
            .cache()?
            .send_intent_digest(&format!("{}\n{text}", mode.as_str_name()));
        let mut record = SendRecord {
            outcome: SendOperation {
                operation_id: operation_id.to_owned(),
                target: key.clone(),
                receipt: None,
            },
            device_id: config.device_id,
            access_url: config.access_url,
            host_fingerprint: self.confirmed_host_fingerprint(&key.host_id)?,
            intent,
        };
        self.ensure_instance_state()?;
        let existing = {
            let _lock = writer::ConfigLock::acquire(&writer::lock_path(&path))?;
            match self.read_send_record(operation_id) {
                Ok(existing) => {
                    if existing.outcome.target != record.outcome.target
                        || existing.intent != record.intent
                    {
                        return Err(CoreError::Conflict(
                            "operation ID has different intent".into(),
                        ));
                    }
                    true
                }
                Err(CoreError::NotFound(_)) => {
                    self.write_send_record(&record)?;
                    false
                }
                Err(error) => return Err(error),
            }
        };
        if existing {
            return self.recover_send_operation(operation_id).await;
        }
        let payload = RequestPayload {
            payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                SendMessage {
                    session_id: key.session_id.clone(),
                    text: text.to_owned(),
                    delivery_mode: whalerein_proto::DeliveryMode::from(mode) as i32,
                },
            )),
        };
        record.outcome.receipt = Some(
            self.control_with_identity(
                &key.host_id,
                payload,
                Some(operation_id),
                Some(operation_id),
            )
            .await?,
        );
        let _lock = writer::ConfigLock::acquire(&writer::lock_path(&path))?;
        // A concurrent recovery may already have seen the terminal outcome.
        let existing = self.read_send_record(operation_id)?;
        if existing
            .outcome
            .receipt
            .as_ref()
            .is_some_and(|receipt| receipt.status.is_terminal())
        {
            return Ok(existing.outcome);
        }
        self.write_send_record(&record)?;
        Ok(record.outcome)
    }

    pub async fn recover_send_operation(&self, id: &str) -> CoreResult<SendOperation> {
        let mut record = self.read_send_record(id)?;
        let managed = self.cloud_session_id()?.is_some();
        let config = self.require_config()?;
        let host = &record.outcome.target.host_id;
        if config.device_id != record.device_id
            || (!managed && config.access_url != record.access_url)
            || self.confirmed_host_fingerprint(host)? != record.host_fingerprint
        {
            return Err(CoreError::Conflict(
                "operation belongs to a different regional authority".into(),
            ));
        }
        if record
            .outcome
            .receipt
            .as_ref()
            .is_some_and(|receipt| receipt.status.is_terminal())
        {
            if managed {
                // A saved result is still private to a currently authorized
                // Host and Device, even after a session or route changes.
                let access = self.access_client().await?;
                let listed = self
                    .settle(access.list_hosts().await.map_err(CoreError::from))
                    .await?;
                if !listed.iter().any(|listed| {
                    listed.host_id == *host && listed.fingerprint == record.host_fingerprint
                }) {
                    return Err(CoreError::NotFound(
                        "Host grant could not be confirmed".into(),
                    ));
                }
            }
            return Ok(record.outcome);
        }
        let receipt = self.recover_command(host, id, id).await?;
        let path = self.send_record_path(id)?;
        let _lock = writer::ConfigLock::acquire(&writer::lock_path(&path))?;
        record = self.read_send_record(id)?;
        if record
            .outcome
            .receipt
            .as_ref()
            .is_some_and(|receipt| receipt.status.is_terminal())
        {
            return Ok(record.outcome);
        }
        record.outcome.receipt = Some(receipt);
        self.write_send_record(&record)?;
        Ok(record.outcome)
    }
}

/// Proof-backed outcome of following one submitted message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SendReply {
    /// No association or settlement evidence yet.
    Pending,
    /// The Source associated the exact submission with a canonical message.
    Committed {
        reference: crate::history::MessageReference,
    },
    /// The owning active work waits for evidenced kinds.
    InputRequired {
        kinds: Vec<crate::api::types::InputKind>,
        operation: String,
        session: SessionKey,
    },
    /// Evidence cannot bind the submission to one reply.
    Uncertain,
}

/// Previous public name for the typed send follow-up result.
pub type MessageReply = SendReply;

fn input_kind(value: i32) -> Option<crate::api::types::InputKind> {
    use crate::api::types::InputKind;
    use whalerein_proto::InputKind as Wire;
    match Wire::try_from(value).ok()? {
        Wire::InputApproval => Some(InputKind::Approval),
        Wire::InputQuestion => Some(InputKind::Question),
        Wire::InputOther => Some(InputKind::Other),
        Wire::Unspecified => None,
    }
}

impl Core {
    pub async fn follow_send_operation(
        &self,
        mut operation: SendOperation,
        timeout: Duration,
    ) -> CoreResult<SendReply> {
        let follow = async {
            loop {
                let Some(receipt) = &operation.receipt else {
                    return Ok(SendReply::Uncertain);
                };
                let Some(message_id) = receipt.message_id.as_deref().filter(|id| !id.is_empty())
                else {
                    if receipt.status.is_terminal() {
                        return Ok(SendReply::Uncertain);
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    operation = self.recover_send_operation(&operation.operation_id).await?;
                    continue;
                };
                let target = &operation.target;
                let response = self
                    .query(
                        &target.host_id,
                        RequestPayload {
                            payload: Some(
                                whalerein_proto::request_payload::Payload::ReadMessageReply(
                                    whalerein_proto::ReadMessageReply {
                                        session_id: target.session_id.clone(),
                                        submitted_message_id: message_id.to_owned(),
                                    },
                                ),
                            ),
                        },
                    )
                    .await?;
                let Some(whalerein_proto::response_payload::Payload::MessageReply(reply)) =
                    response.payload
                else {
                    return Err(CoreError::Security("invalid message reply".into()));
                };
                if reply.session_id != target.session_id || reply.submitted_message_id != message_id
                {
                    return Err(CoreError::Security("message reply scope mismatch".into()));
                }
                match whalerein_proto::MessageReplyState::try_from(reply.state) {
                    Ok(whalerein_proto::MessageReplyState::MessageReplyCommitted) => {
                        let committed = reply.committed.as_ref().ok_or_else(|| {
                            CoreError::Security("committed reply has no reference".into())
                        })?;
                        let reference =
                            crate::history::MessageReference::from_committed(target, committed)?;
                        self.diagnostics.record_stage(
                            "core",
                            "send_reply_confirmed",
                            "confirmed",
                            "none",
                        );
                        return Ok(SendReply::Committed { reference });
                    }
                    Ok(whalerein_proto::MessageReplyState::MessageReplyInputRequired) => {
                        let kinds: Vec<_> = reply
                            .pending_input
                            .iter()
                            .filter_map(|kind| input_kind(*kind))
                            .collect();
                        if kinds.is_empty() {
                            return Ok(SendReply::Uncertain);
                        }
                        for _ in &kinds {
                            self.diagnostics.record_stage(
                                "core",
                                "send_input",
                                "required",
                                "input_required",
                            );
                        }
                        return Ok(SendReply::InputRequired {
                            kinds,
                            operation: operation.operation_id.clone(),
                            session: target.clone(),
                        });
                    }
                    Ok(whalerein_proto::MessageReplyState::MessageReplyPending) => {}
                    _ => {
                        self.diagnostics.record_stage(
                            "core",
                            "send_reply_confirmed",
                            "unconfirmed",
                            "unknown",
                        );
                        return Ok(SendReply::Uncertain);
                    }
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        };
        match tokio::time::timeout(timeout, follow).await {
            Ok(Err(
                CoreError::UnknownResult
                | CoreError::Timeout
                | CoreError::HostOffline
                | CoreError::Unavailable(_),
            ))
            | Err(_) => {
                self.diagnostics.record_stage(
                    "core",
                    "send_reply_confirmed",
                    "unconfirmed",
                    "timeout",
                );
                Ok(SendReply::Uncertain)
            }
            Ok(result) => result,
        }
    }
}

/// Confirmed send reply together with an optional canonical content read.
///
/// The confirmation is retained even when the later read fails; callers must
/// never resubmit because content was unavailable.
#[derive(Debug)]
pub struct SendFollowUp {
    pub reply: SendReply,
    pub message: Option<crate::conversation::ConversationMessage>,
    pub read_error: Option<CoreError>,
}

impl SendFollowUp {
    pub fn is_confirmed(&self) -> bool {
        matches!(self.reply, SendReply::Committed { .. })
    }

    /// Stable redacted read-failure code, when content could not be read.
    pub fn read_error_code(&self) -> Option<&'static str> {
        self.read_error.as_ref().map(CoreError::code)
    }
}

impl Core {
    /// Follow a submitted message and, for a proven reference, read its
    /// canonical content through the same history interface within the
    /// remaining deadline. A failed read keeps the confirmation intact.
    pub async fn follow_send_operation_with_content(
        &self,
        operation: SendOperation,
        timeout: Duration,
    ) -> CoreResult<SendFollowUp> {
        let started = tokio::time::Instant::now();
        let reply = self.follow_send_operation(operation, timeout).await?;
        let SendReply::Committed { reference } = &reply else {
            return Ok(SendFollowUp {
                reply,
                message: None,
                read_error: None,
            });
        };
        let remaining = timeout.saturating_sub(started.elapsed());
        let read = if remaining.is_zero() {
            Err(CoreError::Timeout)
        } else {
            match tokio::time::timeout(remaining, self.history_message(reference)).await {
                Ok(result) => result,
                Err(_) => Err(CoreError::Timeout),
            }
        };
        match read {
            Ok(message) => {
                self.diagnostics
                    .record_stage("core", "send_reply_read", "succeeded", "none");
                Ok(SendFollowUp {
                    reply,
                    message: Some(message),
                    read_error: None,
                })
            }
            Err(error) => {
                self.diagnostics
                    .record_stage("core", "send_reply_read", "failed", error.code());
                Ok(SendFollowUp {
                    reply,
                    message: None,
                    read_error: Some(error),
                })
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientStatus {
    pub config: std::path::PathBuf,
    pub data_dir: std::path::PathBuf,
    pub service: ClientService,
    pub testing_environment: Option<&'static str>,
    pub instance_id: Option<String>,
    pub account_state: AccountState,
    pub remote_signout_pending: bool,
    pub default_host: Option<String>,
    pub device_access: DeviceAccess,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    NotApplicable,
    SignedOut,
    SignedIn,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceAccess {
    Unpaired,
    Ready,
    RevocationPending,
    Revoked,
    Unavailable,
    SignInRequired,
}

impl Core {
    pub async fn status(&self) -> CoreResult<ClientStatus> {
        let client = self.client_config();
        let account_state = match client.service {
            ClientService::Standalone => AccountState::NotApplicable,
            ClientService::WhaleReinCloud if self.account_status()?.is_some() => {
                let remote = tokio::time::timeout(Duration::from_secs(5), async {
                    self.account_client(false).await?.status().await
                })
                .await;
                if matches!(remote, Ok(Err(CoreError::AuthenticationRequired))) {
                    self.runtime.disconnect_cloud_session();
                    AccountState::SignedOut
                } else {
                    AccountState::SignedIn
                }
            }
            ClientService::WhaleReinCloud => AccountState::SignedOut,
        };
        if matches!(account_state, AccountState::SignedOut) {
            if let Ok(client) = self.account_client(false).await {
                client.retry_remote_signout().await?;
            }
        }
        let remote_signout_pending = matches!(client.service, ClientService::WhaleReinCloud)
            && crate::account::native::AccountClient::local_logout_pending(&self.options.secrets)?;
        let signed_out = matches!(account_state, AccountState::SignedOut);
        let default_host = if signed_out {
            None
        } else {
            self.default_host()?
        };
        let device_access = match (signed_out, self.local_revocation_state()?) {
            (true, _) => DeviceAccess::SignInRequired,
            (_, Some("revoked")) => DeviceAccess::Revoked,
            (_, Some("revocation_pending")) => DeviceAccess::RevocationPending,
            _ if !self.is_paired() => DeviceAccess::Unpaired,
            _ => match tokio::time::timeout(Duration::from_secs(5), self.inspect_regional_hosts())
                .await
            {
                Ok(Ok(hosts))
                    if default_host
                        .as_ref()
                        .is_none_or(|target| hosts.iter().any(|host| host.host_id == *target)) =>
                {
                    DeviceAccess::Ready
                }
                Ok(Err(error))
                    if matches!(account_state, AccountState::NotApplicable)
                        && (matches!(
                            error,
                            CoreError::Revoked | CoreError::RevokedRetentionUnverified
                        ) || error.device_authorization_lost()) =>
                {
                    DeviceAccess::Revoked
                }
                _ => DeviceAccess::Unavailable,
            },
        };
        Ok(ClientStatus {
            config: self.config_path().to_owned(),
            data_dir: self.configured_data_dir().to_owned(),
            service: client.service,
            testing_environment: match client.cloud_environment {
                Some(Environment::Local) => Some("local"),
                Some(Environment::Staging) => Some("staging"),
                _ => None,
            },
            instance_id: self.instance_id()?,
            account_state,
            remote_signout_pending,
            default_host,
            device_access,
        })
    }

    /// Read the regional Host directory without modifying cached or local authorization state.
    pub async fn inspect_regional_hosts(&self) -> CoreResult<Vec<HostInfo>> {
        let access = self.access_client().await?;
        let (hosts, complete) = access.inspect_hosts().await?;
        if !complete {
            return Err(CoreError::Unavailable(
                "regional Host listing is incomplete. Select a Host with --host.".into(),
            ));
        }
        Ok(hosts.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{test_support::FakeAccess, MemorySecretStore};

    async fn paired_instance() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        Core,
        FakeAccess,
        Arc<MemorySecretStore>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets = Arc::new(MemorySecretStore::new());
        let core = Core::open(&path, secrets.clone()).unwrap();
        let access = FakeAccess::start().await;
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        (directory, path, core, access, secrets)
    }

    #[tokio::test]
    async fn operation_recovers_original_send_after_lost_receipt() {
        let (_directory, path, core, access, secrets) = paired_instance().await;
        let target = core.operation_session_key(None, "session-1").await.unwrap();
        let id = Core::new_command_id();
        access.host.lock().unwrap().drop_command_receipts = true;
        assert!(core
            .submit_message_operation(&id, &target, "fixture-input", DeliveryMode::EnqueueAfterRun,)
            .await
            .is_err());
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
        assert_eq!(core.send_operations().unwrap()[0].target, target);
        core.set_default_host("different-fixture-default").unwrap();
        drop(core);

        access.host.lock().unwrap().drop_command_receipts = false;
        let core = Core::open(&path, secrets).unwrap();
        let first = core.recover_send_operation(&id);
        let second = core.recover_send_operation(&id);
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        assert_eq!(first.receipt, second.unwrap().receipt);
        assert_eq!(first.operation_id, id);
        assert_eq!(first.target, target);
        assert_eq!(
            core.default_host().unwrap().as_deref(),
            Some("different-fixture-default")
        );
        let reply = core
            .follow_send_operation(first, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(
            reply,
            MessageReply::Committed { reference }
                if reference.message_id() == "message-1"
        ));
        assert!(matches!(
            core.submit_message_operation(
                &id,
                &target,
                "different fixture",
                DeliveryMode::EnqueueAfterRun
            )
            .await,
            Err(CoreError::Conflict(_))
        ));
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    }

    #[tokio::test]
    async fn committed_follow_resolves_the_canonical_message_through_history() {
        let (_directory, _path, core, _access, _secrets) = paired_instance().await;
        let target = core.operation_session_key(None, "session-1").await.unwrap();
        let id = Core::new_command_id();
        let sent = core
            .submit_message_operation(&id, &target, "fixture-input", DeliveryMode::EnqueueAfterRun)
            .await
            .unwrap();
        let followed = core
            .follow_send_operation_with_content(sent, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(followed.is_confirmed());
        assert!(followed.read_error_code().is_none());
        let message = followed.message.expect("canonical content");
        assert_eq!(message.message_id, "message-1");
        assert_eq!(crate::conversation::message_text(&message), "x");
        assert_eq!(core.send_operation(&id).unwrap().target, target);
    }

    #[tokio::test]
    async fn content_read_failure_keeps_the_confirmation_and_records_redacted_stages() {
        use crate::diagnostics::journal::CapturingSink;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let sink = Arc::new(CapturingSink::default());
        let core = Core::open_with_diagnostic_sink(
            &path,
            Arc::new(MemorySecretStore::new()),
            sink.clone(),
        )
        .unwrap();
        let access = FakeAccess::start().await;
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        let target = core.operation_session_key(None, "session-1").await.unwrap();
        let id = Core::new_command_id();
        let sent = core
            .submit_message_operation(&id, &target, "fixture-input", DeliveryMode::EnqueueAfterRun)
            .await
            .unwrap();
        access.host.lock().unwrap().history_messages.clear();
        let followed = core
            .follow_send_operation_with_content(sent, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(followed.is_confirmed());
        assert!(followed.message.is_none());
        assert_eq!(followed.read_error_code(), Some("not_found"));
        // Recovery never resubmits the message.
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
        let events = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(events.contains("send_reply_confirmed"));
        assert!(events.contains("send_reply_read"));
        assert!(events.contains("not_found"));
        // No body, token, locator, path or credential reaches diagnostics.
        assert!(!events.contains("fixture-input"));
        assert!(!events.contains("whr1."));
        assert!(!events.contains("locator:"));
        assert!(!events.contains("cache.sqlite3"));
    }

    #[tokio::test]
    async fn pending_input_reports_every_evidenced_kind_for_the_original_operation() {
        let (_directory, _path, core, access, _secrets) = paired_instance().await;
        let target = core.operation_session_key(None, "session-1").await.unwrap();
        let id = Core::new_command_id();
        let sent = core
            .submit_message_operation(&id, &target, "fixture-input", DeliveryMode::EnqueueAfterRun)
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.message_reply_state =
                Some(whalerein_proto::MessageReplyState::MessageReplyInputRequired);
            host.message_reply_input = vec![
                whalerein_proto::InputKind::InputQuestion,
                whalerein_proto::InputKind::InputOther,
            ];
        }
        let reply = core
            .follow_send_operation(sent, Duration::from_secs(5))
            .await
            .unwrap();
        match reply {
            MessageReply::InputRequired {
                kinds,
                operation,
                session,
            } => {
                assert_eq!(
                    kinds,
                    vec![
                        crate::api::InputKind::Question,
                        crate::api::InputKind::Other
                    ]
                );
                assert_eq!(operation, id);
                assert_eq!(session, target);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn operation_record_failure_prevents_a_regional_send() {
        let (_directory, _path, core, access, _secrets) = paired_instance().await;
        let target = core.operation_session_key(None, "session-1").await.unwrap();
        std::fs::write(core.operation_dir(), b"fixture").unwrap();
        let error = core
            .submit_message_operation(
                &Core::new_command_id(),
                &target,
                "fixture-input",
                DeliveryMode::EnqueueAfterRun,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "unreadable_config");
        assert_eq!(access.host.lock().unwrap().commands_seen, 0);
    }

    #[tokio::test]
    async fn operation_host_selection_keeps_explicit_host_when_other_evidence_fails() {
        let (_directory, _path, core, access, _secrets) = paired_instance().await;
        assert_eq!(core.resolve_operation_host(None).await.unwrap(), "host-1");

        core.set_default_host("stale-fixture-host").unwrap();
        let stale = core.resolve_operation_host(None).await.unwrap_err();
        assert!(matches!(stale, CoreError::NotFound(_)));
        assert!(stale.to_string().contains("--host"));

        std::fs::remove_file(core.data_dir().join(DEFAULT_HOST)).unwrap();
        let mut config = core.current_config().unwrap();
        config
            .host_fingerprints
            .insert("host-2".into(), config.host_fingerprint.clone().unwrap());
        core.config_store().save(&config).unwrap();
        *core.config.write().unwrap() = Some(config);
        {
            let mut host = access.host.lock().unwrap();
            host.extra_host = true;
            host.extra_host_online = false;
        }
        assert_eq!(core.resolve_operation_host(None).await.unwrap(), "host-1");
        access.host.lock().unwrap().extra_host_online = true;
        assert!(matches!(
            core.resolve_operation_host(None).await,
            Err(CoreError::Conflict(_))
        ));

        access.fail_hosts(true);
        assert!(core.resolve_operation_host(None).await.is_err());
        let target = core
            .operation_session_key(Some("host-1"), "session-1")
            .await
            .unwrap();
        assert_eq!(
            core.get_session(&target).await.unwrap().session_id,
            "session-1"
        );
        assert!(core.default_host().unwrap().is_none());
    }

    #[tokio::test]
    async fn instance_status_observes_regional_access_without_mutating_state() {
        let (_directory, _path, core, access, _secrets) = paired_instance().await;
        let expected = std::fs::read(core.data_dir().join(DEFAULT_HOST)).unwrap();
        let regional = core.config_store().load().unwrap();
        let account = core.status().await.unwrap();
        assert!(matches!(account.account_state, AccountState::NotApplicable));
        assert!(matches!(account.device_access, DeviceAccess::Ready));
        access.fail_hosts(true);
        let unavailable = core.status().await.unwrap();
        assert!(matches!(
            unavailable.device_access,
            DeviceAccess::Unavailable
        ));

        access.fail_hosts(false);
        access.revoke();
        let revoked = core.status().await.unwrap();
        assert!(matches!(revoked.device_access, DeviceAccess::Revoked));
        assert_eq!(
            std::fs::read(core.data_dir().join(DEFAULT_HOST)).unwrap(),
            expected
        );
        assert_eq!(
            core.config_store().load().unwrap().unwrap().device_id,
            regional.unwrap().device_id,
        );
        assert!(core.local_revocation_state().unwrap().is_none());
    }

    #[tokio::test]
    async fn message_follow_reports_pending_input_and_deadline_for_the_original_send() {
        let (_directory, _path, core, access, _secrets) = paired_instance().await;
        let target = core.operation_session_key(None, "session-1").await.unwrap();
        let id = Core::new_command_id();
        let sent = core
            .submit_message_operation(&id, &target, "fixture-input", DeliveryMode::EnqueueAfterRun)
            .await
            .unwrap();
        access.host.lock().unwrap().message_reply_state =
            Some(whalerein_proto::MessageReplyState::MessageReplyInputRequired);
        let input = core
            .follow_send_operation(sent.clone(), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(matches!(
            input,
            MessageReply::InputRequired { kinds, operation, session }
                if kinds == vec![crate::api::InputKind::Approval]
                    && operation == id
                    && session == target
        ));

        access.host.lock().unwrap().message_reply_state =
            Some(whalerein_proto::MessageReplyState::MessageReplyPending);
        let timed_out = core
            .follow_send_operation(sent.clone(), Duration::from_millis(50))
            .await
            .unwrap();
        assert!(matches!(timed_out, MessageReply::Uncertain));
        assert_eq!(core.send_operation(&id).unwrap().target, target);
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
    }
}
