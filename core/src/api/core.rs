//! Core orchestration: lifecycle, pairing, snapshot queries, commands, sync
//! and diagnostics. CLI calls this type only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use url::Url;
use whalerein_proto::{
    ApprovalDecision, GetSession, Interrupt, ListSessions, RequestPayload, ResolveApproval,
    ResponsePayload, SendMessage,
};

use super::config::{self, ConfigStore, PersistedConfig};
use super::error::{CoreError, CoreResult};
use super::types::{
    ApprovalInfo, CommandHandle, DoctorReport, ExportManifest, HostInfo, OnlineStatus,
    PairingOutcome, PairingPrompt, PendingCommand, SessionList, SessionSummary,
};
use crate::access::{AccessClient, DeviceMqttClient, MqttConnectionEvent, MqttSettings};
use crate::cache::Cache;
use crate::commands::CommandService;
use crate::diagnostics;
use crate::pairing::{default_capabilities, parse_pairing_uri, PairingDecision, PairingInitiator};
use crate::runtime::channel::ensure_channel;
use crate::runtime::{ConnectionState, Runtime};
use crate::security::{keys, load_device_token, load_or_create_cache_key, SecretStore};
use crate::sync::SyncService;

/// Injected Core options. `data_dir` is a path, not a second dependency seam.
#[derive(Clone)]
pub struct CoreOptions {
    pub data_dir: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for CoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreOptions")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Core {
    options: CoreOptions,
    config: Arc<RwLock<Option<PersistedConfig>>>,
    runtime: Arc<Runtime>,
    journal: Arc<diagnostics::journal::Journal>,
    identity: Arc<Mutex<Option<Arc<crate::security::device::DeviceIdentity>>>>,
    cache: Arc<Mutex<Option<Arc<Cache>>>>,
    watch_active: Arc<AtomicBool>,
}

impl Core {
    pub fn new(options: CoreOptions) -> CoreResult<Self> {
        std::fs::create_dir_all(&options.data_dir)
            .map_err(|_| CoreError::Config("data directory could not be created".into()))?;
        let store = ConfigStore::new(&options.data_dir);
        let config = store.load()?;
        Ok(Self {
            options,
            config: Arc::new(RwLock::new(config)),
            runtime: Arc::new(Runtime::new()),
            journal: Arc::new(diagnostics::journal::Journal::default()),
            identity: Arc::new(Mutex::new(None)),
            cache: Arc::new(Mutex::new(None)),
            watch_active: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Ephemeral operation ID shared with Access and host logs.
    pub fn diagnostic_id(&self) -> &str {
        self.journal.trace_id()
    }

    /// Persist only bounded diagnostic metadata, never the error message.
    pub fn save_failure(&self, code: &str) -> CoreResult<()> {
        self.journal.save_failure(&self.options.data_dir, code)
    }

    /// Remove the previous failure record without contacting Access or DSH.
    pub fn clear_diagnostics(&self) -> CoreResult<()> {
        match std::fs::remove_file(
            self.options
                .data_dir
                .join(diagnostics::journal::FAILURE_FILE),
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(CoreError::Internal(
                "diagnostic file could not be removed".into(),
            )),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.options.data_dir
    }

    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn is_paired(&self) -> bool {
        self.config.read().expect("config lock").is_some()
    }

    pub fn access_address(&self) -> Option<String> {
        self.config
            .read()
            .expect("config lock")
            .as_ref()
            .map(|config| config.access_url.clone())
    }

    pub fn metrics(&self) -> Vec<crate::diagnostics::MetricSample> {
        self.runtime.metrics()
    }

    fn current_config(&self) -> Option<PersistedConfig> {
        self.config.read().expect("config lock").clone()
    }

    fn require_config(&self) -> CoreResult<PersistedConfig> {
        self.current_config().ok_or(CoreError::NotPaired)
    }

    fn config_store(&self) -> ConfigStore {
        ConfigStore::new(&self.options.data_dir)
    }

    fn access_client(&self) -> CoreResult<AccessClient> {
        let config = self.require_config()?;
        let token = load_device_token(&self.options.secrets, &config.device_id)?
            .ok_or(CoreError::NotPaired)?;
        let base = Url::parse(&config.access_url)
            .map_err(|_| CoreError::Config("invalid Access URL".into()))?;
        let ca = config::ca_pem_from_env();
        AccessClient::with_ca(base, Some(token), ca.as_deref())
            .map(|access| access.with_trace_id(self.diagnostic_id()))
            .map_err(Into::into)
    }

    fn identity_lock(&self) -> CoreResult<crate::security::device::ProfileLock> {
        crate::security::device::ProfileLock::acquire(&self.options.data_dir).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock || error.raw_os_error() == Some(32) {
                CoreError::Conflict("another identity operation is in progress".into())
            } else {
                CoreError::Config("profile identity lock is unavailable".into())
            }
        })
    }

    fn device_identity(&self) -> CoreResult<Arc<crate::security::device::DeviceIdentity>> {
        if let Some(identity) = self.identity.lock().expect("identity mutex").as_ref() {
            return Ok(identity.clone());
        }
        let guard = self.identity_lock()?;
        self.device_identity_locked(&guard)
    }

    fn device_identity_locked(
        &self,
        _guard: &crate::security::device::ProfileLock,
    ) -> CoreResult<Arc<crate::security::device::DeviceIdentity>> {
        let mut slot = self.identity.lock().expect("identity mutex");
        if let Some(identity) = slot.as_ref() {
            return Ok(identity.clone());
        }
        let config = self.current_config();
        let identity = Arc::new(crate::security::device::load_or_create(
            &self.options.secrets,
            &self.options.data_dir,
            config.as_ref().map(|config| config.device_id.as_str()),
        )?);
        *slot = Some(identity.clone());
        Ok(identity)
    }

    /// Forget the local identity only after authorization was removed explicitly.
    pub fn forget_device(&self) -> CoreResult<()> {
        self.forget_identity(false)
    }

    /// Explicit recovery when credentials/keys are lost. Remote grants remain.
    pub fn forget_local_device(&self) -> CoreResult<()> {
        self.forget_identity(true)
    }

    fn forget_identity(&self, local_only: bool) -> CoreResult<()> {
        let _guard = self.identity_lock()?;
        if !local_only && self.is_paired() && !self.runtime.is_revoked() {
            return Err(CoreError::Conflict(
                "revoke the active pairing before forgetting this device".into(),
            ));
        }
        self.runtime.revoke();
        if let Some(config) = self.current_config() {
            self.options
                .secrets
                .delete(&keys::device_private_key(&config.device_id))?;
            self.clear_local_pairing_credentials(&config.device_id)?;
        }
        crate::security::device::forget(&self.options.secrets, &self.options.data_dir)?;
        *self.identity.lock().expect("identity mutex") = None;
        Ok(())
    }

    fn cache(&self) -> CoreResult<Arc<Cache>> {
        let config = self.require_config()?;
        let mut slot = self.cache.lock().expect("cache lock");
        if let Some(cache) = slot.as_ref() {
            return Ok(cache.clone());
        }
        let key = match self
            .options
            .secrets
            .load(&keys::cache_key(&config.device_id))?
        {
            Some(bytes) => bytes
                .try_into()
                .map_err(|_| crate::security::SecurityError::InvalidKey)?,
            None => {
                let _guard = self.identity_lock()?;
                load_or_create_cache_key(&self.options.secrets, &config.device_id)?
            }
        };
        let cache = Arc::new(Cache::open(
            &self.options.data_dir.join("cache.sqlite3"),
            key,
        )?);
        *slot = Some(cache.clone());
        Ok(cache)
    }

    fn sync_service(&self) -> CoreResult<SyncService> {
        Ok(SyncService::new(self.cache()?))
    }

    fn command_service(&self) -> CoreResult<CommandService> {
        let access = Arc::new(self.access_client()?);
        Ok(CommandService::new(access, self.cache()?))
    }

    fn cache_or_empty(&self) -> CoreResult<Arc<Cache>> {
        if self.is_paired() {
            self.cache()
        } else {
            Ok(Arc::new(Cache::open_in_memory([0u8; 32])?))
        }
    }

    fn host_for_session(&self, session_id: &str) -> CoreResult<String> {
        Ok(self.session_key(session_id)?.host_id)
    }

    fn confirmed_host_fingerprint(&self, host_id: &str) -> CoreResult<String> {
        let config = self.require_config()?;
        let pin = if host_id == config.host_id {
            config.host_fingerprint
        } else {
            // Other authorized Hosts retain the identity attested by Access.
            self.cache()?
                .load_hosts()?
                .into_iter()
                .find(|host| host.host_id == host_id)
                .map(|host| host.fingerprint)
        };
        pin.ok_or_else(|| {
            CoreError::Conflict("Host fingerprint is missing; explicit pairing is required".into())
        })
    }

    async fn ensure_channel(
        &self,
        host_id: &str,
        force_new: bool,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.journal.record("core", "handshake", "started", "none");
        let opened = async {
            let config = self.require_config()?;
            let identity = self.device_identity()?;
            let access = self.access_client()?;
            let expected = self.confirmed_host_fingerprint(host_id)?;
            ensure_channel(
                &self.runtime,
                &access,
                &identity.key,
                host_id,
                &config.device_id,
                force_new,
                Some(&expected),
            )
            .await
        }
        .await;
        self.journal.record(
            opened
                .as_ref()
                .err()
                .map_or("core", CoreError::source_layer),
            "handshake",
            if opened.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
            opened.as_ref().err().map_or("none", CoreError::code),
        );
        self.settle(opened).await
    }

    async fn query(&self, host_id: &str, payload: RequestPayload) -> CoreResult<ResponsePayload> {
        let result = async {
            let config = self.require_config()?;
            let channel = self.ensure_channel(host_id, false).await?;
            let commands = self.command_service()?;
            match commands
                .query(host_id, &config.device_id, &channel, payload.clone())
                .await
            {
                Err(error)
                    if matches!(error, CoreError::ChannelMismatch)
                        || error.device_authorization_lost() =>
                {
                    // A host reconnect discards its channels. Read-only queries
                    // may negotiate once more; control commands are never resent.
                    self.journal.record(
                        error.source_layer(),
                        "query_channel_recovery",
                        "retrying",
                        error.code(),
                    );
                    let fresh = self.ensure_channel(host_id, true).await?;
                    commands
                        .query(host_id, &config.device_id, &fresh, payload)
                        .await
                }
                other => other,
            }
        }
        .await;
        self.journal.record(
            result
                .as_ref()
                .err()
                .map_or("core", CoreError::source_layer),
            "query",
            if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
            result.as_ref().err().map_or("none", CoreError::code),
        );
        self.settle(result).await
    }

    async fn control(&self, host_id: &str, payload: RequestPayload) -> CoreResult<CommandHandle> {
        let result = async {
            let config = self.require_config()?;
            let channel = self.ensure_channel(host_id, false).await?;
            let commands = self
                .command_service()?
                .with_host_fingerprint(Some(self.confirmed_host_fingerprint(host_id)?));
            match commands
                .control(host_id, &config.device_id, &channel, payload)
                .await
            {
                Ok(handle) => Ok(handle),
                Err(CoreError::StaleCommand {
                    request_id,
                    command_id,
                }) => {
                    self.recover_command(host_id, &request_id, &command_id)
                        .await
                }
                Err(error) => Err(error),
            }
        }
        .await;
        self.settle(result).await
    }

    /// List durable unfinished control identities. Payloads and credentials are excluded.
    pub fn pending_commands(&self) -> CoreResult<Vec<PendingCommand>> {
        self.require_config()?;
        Ok(self.cache()?.pending_commands()?)
    }

    /// Recover a previously submitted control without submitting it again.
    /// This also works after cancellation or reopening the same device profile.
    pub async fn recover_pending_command(&self, request_id: &str) -> CoreResult<CommandHandle> {
        let config = self.require_config()?;
        let pending = self
            .pending_commands()?
            .into_iter()
            .find(|command| command.request_id == request_id)
            .ok_or_else(|| CoreError::NotFound("pending command not found".into()))?;
        if pending.host_id.is_empty()
            || pending.device_id != config.device_id
            || pending.access_url != config.access_url
        {
            return Err(CoreError::Conflict(
                "command recovery identity is unavailable for this profile".into(),
            ));
        }
        if pending.host_fingerprint != self.confirmed_host_fingerprint(&pending.host_id)? {
            return Err(CoreError::Conflict(
                "command belongs to a different confirmed Host identity".into(),
            ));
        }
        match self.access_client()?.get_request(&pending.request_id).await {
            Ok(state) => {
                if state.host_id != pending.host_id
                    || state.device_id != pending.device_id
                    || state.command_id != pending.command_id
                {
                    return Err(CoreError::Security(
                        "command recovery response identity mismatch".into(),
                    ));
                }
                if let Some(
                    status @ (OnlineStatus::HostOffline
                    | OnlineStatus::Rejected
                    | OnlineStatus::Failed),
                ) = OnlineStatus::from_proto(state.status)
                {
                    let code = whalerein_proto::ErrorCode::try_from(state.error_code)
                        .ok()
                        .filter(|code| *code != whalerein_proto::ErrorCode::Unspecified);
                    self.command_service()?.project_status(
                        &pending.request_id,
                        &pending.command_id,
                        status,
                        code,
                    )?;
                    return self
                        .settle(Err(match status {
                            OnlineStatus::HostOffline => CoreError::HostOffline,
                            OnlineStatus::Rejected => CoreError::HostRejected,
                            _ => CoreError::Host {
                                code,
                                message: "command failed".into(),
                            },
                        }))
                        .await;
                }
            }
            Err(error) if error.status == Some(404) => {}
            Err(error) => return self.settle(Err(error.into())).await,
        }
        let result = self
            .recover_command(&pending.host_id, &pending.request_id, &pending.command_id)
            .await;
        self.settle(result).await
    }

    /// After a reconnect, recover the terminal command result over a new
    /// channel using the encrypted `GetCommandResult` query.
    async fn recover_command(
        &self,
        host_id: &str,
        request_id: &str,
        command_id: &str,
    ) -> CoreResult<CommandHandle> {
        self.journal
            .record("host", "command_result_recovery", "started", "none");
        let config = self.require_config()?;
        let mut channel = self.ensure_channel(host_id, true).await?;
        let commands = self.command_service()?;
        for attempt in 0..10u32 {
            match commands
                .get_command_result(host_id, &config.device_id, &channel, command_id)
                .await
            {
                Ok(result) => {
                    return commands.handle_from_command_result(request_id, command_id, &result)
                }
                Err(CoreError::UnknownResult) if attempt < 9 => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error)
                    if attempt < 9
                        && (matches!(error, CoreError::ChannelMismatch)
                            || error.device_authorization_lost()) =>
                {
                    channel = self.ensure_channel(host_id, true).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(CoreError::UnknownResult)
    }

    // -----------------------------------------------------------------
    // Pairing
    // -----------------------------------------------------------------

    /// Run the complete pairing flow. `decide` is called after HELLO with the
    /// Noise-derived fingerprint; rejecting or mismatching never sends msg3.
    pub async fn pair<F>(&self, uri: &str, decide: F) -> CoreResult<PairingOutcome>
    where
        F: FnOnce(&PairingPrompt) -> PairingDecision,
    {
        self.pair_as(uri, decide, "").await
    }

    /// Pair and carry `display_name` inside the Noise HELLO. The name is a
    /// label. An empty name stays empty after sanitizing.
    pub async fn pair_as<F>(
        &self,
        uri: &str,
        decide: F,
        display_name: &str,
    ) -> CoreResult<PairingOutcome>
    where
        F: FnOnce(&PairingPrompt) -> PairingDecision,
    {
        let parsed = parse_pairing_uri(uri)?;
        let guard = self.identity_lock()?;
        let identity = self.device_identity_locked(&guard)?;
        if self.is_paired() {
            let config = self.require_config()?;
            load_or_create_cache_key(&self.options.secrets, &config.device_id)?;
            match self.hosts().await {
                Ok(_) => return Err(CoreError::Conflict("client is already paired".into())),
                Err(CoreError::Revoked) => self.runtime.accept_pairing(),
                Err(CoreError::NotPaired) => {
                    let config = self.require_config()?;
                    self.clear_local_pairing(&config.device_id)?;
                }
                Err(error) => return Err(error),
            }
        }
        let device_id = identity.device_id.clone();
        let (mut session, hello) = PairingInitiator::start_named(
            &parsed,
            &device_id,
            &identity.key,
            default_capabilities(),
            display_name,
        )?;

        let ca = config::ca_pem_from_env();
        let access = AccessClient::with_ca(parsed.access_url.clone(), None, ca.as_deref())?
            .with_trace_id(self.diagnostic_id());
        let hello_response = access.pair_claim(&hello).await?;
        let confirmation = session.accept_hello(&hello_response)?;
        let prompt = PairingPrompt {
            host_id: confirmation.host_id.clone(),
            access_url: confirmation.access_url.clone(),
            fingerprint: confirmation.fingerprint.clone(),
            fingerprint_display: confirmation.fingerprint_display.clone(),
        };
        let decision = decide(&prompt);
        if !decision.approve {
            session.cancel();
            return Err(CoreError::PairingCancelled);
        }
        if let Some(expected) = &decision.expected_fingerprint {
            let normalized = crate::util::normalize_fingerprint(expected);
            if normalized != confirmation.fingerprint {
                session.cancel();
                return Err(CoreError::FingerprintMismatch {
                    expected: normalized,
                    actual: confirmation.fingerprint,
                });
            }
        }
        session.confirm(&decision)?;
        let finish = session.write_finish()?;
        let finish_response = access.pair_claim(&finish).await?;
        let completion = session.complete(&finish_response)?;

        load_or_create_cache_key(&self.options.secrets, &device_id)?;
        if let Err(error) = crate::security::store_device_token(
            &self.options.secrets,
            &device_id,
            &completion.device_token,
        ) {
            let _ = self.options.secrets.delete(&keys::device_token(&device_id));
            return Err(error.into());
        }

        let mqtt_url = config::mqtt_url_from_env().or(parsed.mqtt_url.clone());
        let mut persisted = PersistedConfig::new(
            parsed.access_url.to_string(),
            mqtt_url,
            parsed.host_id.clone(),
            device_id.clone(),
        );
        persisted.host_fingerprint = Some(completion.host_fingerprint.clone());
        if let Err(error) = self.config_store().save(&persisted) {
            let _ = self.options.secrets.delete(&keys::device_token(&device_id));
            return Err(error);
        }
        *self.config.write().expect("config lock") = Some(persisted);
        self.runtime.accept_pairing();

        let _ = self
            .runtime
            .set_state(&parsed.host_id, ConnectionState::Pairing);
        let _ = self
            .runtime
            .set_state(&parsed.host_id, ConnectionState::Disconnected);

        tracing::info!(trace_id = %self.diagnostic_id(), "pairing completed");
        Ok(PairingOutcome {
            host_id: parsed.host_id,
            device_id,
            fingerprint: completion.host_fingerprint,
            access_url: parsed.access_url.to_string(),
        })
    }

    // -----------------------------------------------------------------
    // Snapshot API
    // -----------------------------------------------------------------

    pub async fn hosts(&self) -> CoreResult<Vec<HostInfo>> {
        let access = self.access_client()?;
        let listed = access.list_hosts().await.map_err(CoreError::from);
        let hosts: Vec<HostInfo> = self
            .settle(listed)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();
        if let Ok(sync) = self.sync_service() {
            sync.store_hosts(&hosts)?;
        }
        Ok(hosts)
    }

    /// Alias kept for the architecture's `listHosts()` shape.
    pub async fn list_hosts(&self) -> CoreResult<Vec<HostInfo>> {
        self.hosts().await
    }

    pub async fn get_host(&self, host_id: &str) -> CoreResult<HostInfo> {
        let cache = self.cache()?;
        if let Some(host) = cache
            .load_hosts()?
            .into_iter()
            .find(|host| host.host_id == host_id)
        {
            return Ok(host);
        }
        self.hosts()
            .await?
            .into_iter()
            .find(|host| host.host_id == host_id)
            .ok_or_else(|| CoreError::NotFound("host not found".into()))
    }

    pub fn get_connection_state(&self, host_id: &str) -> ConnectionState {
        self.runtime.state(host_id)
    }

    pub async fn sessions(&self, host_id: &str) -> CoreResult<SessionList> {
        let payload = RequestPayload {
            payload: Some(whalerein_proto::request_payload::Payload::ListSessions(
                ListSessions {},
            )),
        };
        match self.query(host_id, payload).await {
            Ok(response) => {
                let list = match response.payload {
                    Some(whalerein_proto::response_payload::Payload::SessionList(list)) => list,
                    _ => return Err(CoreError::Internal("unexpected query response".into())),
                };
                let sessions: Vec<SessionSummary> =
                    list.sessions.into_iter().map(Into::into).collect();
                let sync = self.sync_service()?;
                sync.store_sessions(host_id, &sessions, "remote", true)?;
                Ok(SessionList {
                    sessions,
                    source: "remote".into(),
                    complete: true,
                })
            }
            Err(error)
                if matches!(
                    error,
                    CoreError::HostOffline | CoreError::Timeout | CoreError::Unavailable(_)
                ) =>
            {
                let sync = self.sync_service()?;
                let sessions = sync.load_sessions(host_id)?;
                if sessions.is_empty() {
                    return Err(error);
                }
                Ok(SessionList {
                    sessions,
                    source: "cache".into(),
                    complete: false,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Alias kept for the architecture's `listSessions()` shape.
    pub async fn list_sessions(&self, host_id: &str) -> CoreResult<SessionList> {
        self.sessions(host_id).await
    }

    pub async fn get_session(&self, session_id: &str) -> CoreResult<SessionSummary> {
        let cache = self.cache()?;
        if let Some(session) = cache.get_session(&self.session_key(session_id)?)? {
            return Ok(session);
        }
        let host = self.host_for_session(session_id)?;
        let response = self
            .query(
                &host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::GetSession(
                        GetSession {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        let summary = match response.payload {
            Some(whalerein_proto::response_payload::Payload::SessionInfo(summary)) => {
                SessionSummary::from(summary)
            }
            _ => return Err(CoreError::Internal("unexpected session response".into())),
        };
        let sync = self.sync_service()?;
        sync.store_sessions(&host, std::slice::from_ref(&summary), "remote", true)?;
        Ok(summary)
    }

    // -----------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------

    pub async fn send_message(&self, session_id: &str, text: &str) -> CoreResult<CommandHandle> {
        let host = self.host_for_session(session_id)?;
        let handle = self
            .control(
                &host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                        SendMessage {
                            session_id: session_id.to_owned(),
                            text: text.to_owned(),
                            delivery_mode: whalerein_proto::DeliveryMode::StartIfIdle as i32,
                        },
                    )),
                },
            )
            .await?;
        if handle.status != OnlineStatus::HostAccepted {
            return Ok(handle);
        }
        self.await_command_terminal(&host, handle).await
    }

    /// Convert `host_accepted` into a confirmed terminal result by asking the
    /// host for the command result over the current channel.
    async fn await_command_terminal(
        &self,
        host_id: &str,
        handle: CommandHandle,
    ) -> CoreResult<CommandHandle> {
        let config = self.require_config()?;
        let channel = self.ensure_channel(host_id, false).await?;
        let commands = self.command_service()?;
        let result = commands
            .await_command_result(
                host_id,
                &config.device_id,
                &channel,
                &handle.request_id,
                &handle.command_id,
            )
            .await;
        let result = match result {
            Err(error)
                if matches!(error, CoreError::ChannelMismatch)
                    || error.device_authorization_lost() =>
            {
                self.recover_command(host_id, &handle.request_id, &handle.command_id)
                    .await
            }
            other => other,
        };
        self.settle(result).await
    }

    /// List the host's pending approvals, optionally scoped to one session.
    /// This is the read half of the approval path; approve/reject remain
    /// encrypted control commands.
    pub async fn list_approvals(&self, session_id: &str) -> CoreResult<Vec<ApprovalInfo>> {
        let host = self.host_for_session(session_id)?;
        let response = self
            .query(
                &host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ListApprovals(
                        whalerein_proto::ListApprovals {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::ApprovalList(list)) => {
                Ok(list.approvals.into_iter().map(ApprovalInfo::from).collect())
            }
            _ => Err(CoreError::Internal(
                "unexpected approval list response".into(),
            )),
        }
    }

    pub async fn approve(
        &self,
        approval_id: &str,
        expected_digest: &[u8],
        expected_version: i64,
    ) -> CoreResult<CommandHandle> {
        self.resolve_approval(
            approval_id,
            ApprovalDecision::Approve,
            expected_digest,
            expected_version,
        )
        .await
    }

    pub async fn reject(
        &self,
        approval_id: &str,
        expected_digest: &[u8],
        expected_version: i64,
    ) -> CoreResult<CommandHandle> {
        self.resolve_approval(
            approval_id,
            ApprovalDecision::Reject,
            expected_digest,
            expected_version,
        )
        .await
    }

    async fn resolve_approval(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
        expected_digest: &[u8],
        expected_version: i64,
    ) -> CoreResult<CommandHandle> {
        let config = self.require_config()?;
        self.control(
            &config.host_id,
            RequestPayload {
                payload: Some(whalerein_proto::request_payload::Payload::ResolveApproval(
                    ResolveApproval {
                        approval_id: approval_id.to_owned(),
                        decision: decision as i32,
                        expected_digest: expected_digest.to_vec(),
                        expected_version,
                    },
                )),
            },
        )
        .await
    }

    pub async fn interrupt(&self, session_id: &str) -> CoreResult<CommandHandle> {
        let host = self.host_for_session(session_id)?;
        self.control(
            &host,
            RequestPayload {
                payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                    Interrupt {
                        session_id: session_id.to_owned(),
                    },
                )),
            },
        )
        .await
    }

    /// Revoke grants and stop reconnects while preserving the profile identity.
    /// Clears the token, cache key and pairing configuration.
    ///
    /// A token Access already rejects uses the same terminal path as any other
    /// device-token call and returns `revoked` after the local files are gone.
    pub async fn revoke(&self) -> CoreResult<String> {
        let config = self.require_config()?;
        let device_id = config.device_id.clone();
        let access = self.access_client()?;
        match access.delete_device(&device_id).await {
            Ok(()) => {
                self.runtime.revoke();
                self.clear_local_pairing(&device_id)?;
                Ok(device_id)
            }
            Err(error) => {
                let rejected: CoreResult<()> = self.settle(Err(CoreError::from(error))).await;
                rejected?;
                Ok(device_id)
            }
        }
    }

    /// One exit for Access calls made with the stored device token.
    ///
    /// Host result codes can describe a lost channel rather than a lost grant.
    /// Clear local identity only after Access independently rejects the stored
    /// token on its host-list endpoint. An unavailable check preserves identity.
    async fn settle<T>(&self, result: CoreResult<T>) -> CoreResult<T> {
        let Err(error) = result else {
            return result;
        };
        if !error.device_authorization_lost() {
            return Err(error);
        }
        let access = self.access_client()?;
        self.journal
            .record("access", "authorization_check", "started", error.code());
        match access.list_hosts().await {
            Err(denied) if CoreError::from(denied.clone()).device_authorization_lost() => {
                self.journal
                    .record("access", "authorization_check", "revoked", error.code());
            }
            Ok(_) => {
                self.journal
                    .record("access", "authorization_check", "pairing_preserved", "none");
                return Err(error);
            }
            Err(check) => {
                self.journal.record(
                    "access",
                    "authorization_check",
                    "pairing_preserved",
                    CoreError::from(check).code(),
                );
                return Err(error);
            }
        }
        if let Some(config) = self.current_config() {
            tracing::info!("device authorization lost; clearing local pairing");
            self.runtime.revoke();
            self.clear_local_pairing(&config.device_id)?;
        }
        Err(CoreError::Revoked)
    }

    fn clear_local_pairing(&self, device_id: &str) -> CoreResult<()> {
        // Migrate before dropping config, so an old profile retains its identity.
        self.device_identity()?;
        self.clear_local_pairing_credentials(device_id)
    }

    fn clear_local_pairing_credentials(&self, device_id: &str) -> CoreResult<()> {
        self.options
            .secrets
            .delete(&keys::device_token(device_id))?;
        self.options.secrets.delete(&keys::cache_key(device_id))?;
        self.options
            .secrets
            .delete(&keys::diagnostic_salt(device_id))?;
        if let Some(cache) = self.cache.lock().expect("cache lock").take() {
            let _ = cache.clear();
        }
        let _ = std::fs::remove_file(self.options.data_dir.join("cache.sqlite3"));
        self.config_store().delete()?;
        *self.config.write().expect("config lock") = None;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Events / tail
    // -----------------------------------------------------------------

    /// Synchronize the Source-owned committed conversation through opaque cursors.
    pub async fn sync_session(&self, session_id: &str) -> CoreResult<u64> {
        let key = self.session_key(session_id)?;
        let mut conversation = crate::conversation::Conversation::default();
        self.sync_conversation(&key, &mut conversation, &mut |_| true)
            .await
    }

    fn mqtt_settings(&self, config: &PersistedConfig) -> CoreResult<MqttSettings> {
        let override_url = config::mqtt_url_from_env().or_else(|| config.mqtt_url.clone());
        let settings = match override_url {
            Some(url) => MqttSettings::parse(&url)?,
            None => {
                let access = Url::parse(&config.access_url)
                    .map_err(|_| CoreError::Config("invalid Access URL".into()))?;
                MqttSettings::from_access_url(&access)?
            }
        };
        Ok(settings.with_ca(config::ca_pem_from_env()))
    }

    // -----------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------

    /// Snapshot stored on disk. This does not open a channel.
    pub fn doctor(&self) -> CoreResult<DoctorReport> {
        let cache = self.cache_or_empty()?;
        let address = self
            .current_config()
            .map(|config| config.access_url.clone());
        Ok(diagnostics::build_report(&cache, address))
    }

    /// Write a redacted JSON diagnostic bundle. The caller must confirm first.
    pub fn export_diagnostics(&self, path: &Path) -> CoreResult<ExportManifest> {
        let report = self.doctor()?;
        let mut manifest = diagnostics::render_export(&report);
        let bundle = serde_json::json!({
            "schema": "whalerein/v1",
            "generated_at_unix_ms": crate::util::now_unix_ms(),
            "report": report,
            "last_failure": diagnostics::journal::load_failure(&self.options.data_dir),
            "fields": manifest.fields,
            "redactions": manifest.redactions,
        });
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| {
                CoreError::Internal("diagnostic directory could not be created".into())
            })?;
        }
        let bytes = serde_json::to_vec_pretty(&bundle)
            .map_err(|_| CoreError::Internal("diagnostic bundle could not encode".into()))?;
        std::fs::write(path, &bytes)
            .map_err(|_| CoreError::Internal("diagnostic bundle could not be written".into()))?;
        config::set_private_permissions(path)?;
        manifest.path = path.display().to_string();
        manifest.bytes = bytes.len() as u64;
        Ok(manifest)
    }

    /// Fetch the cached session list without a network call.
    pub fn cached_sessions(&self, host_id: &str) -> CoreResult<Vec<SessionSummary>> {
        self.sync_service()?.load_sessions(host_id)
    }

    pub fn cached_messages(
        &self,
        key: &whalerein_proto::SessionKey,
    ) -> CoreResult<Vec<whalerein_proto::ConversationMessage>> {
        Ok(self.cache()?.history_messages(key)?)
    }

    /// Decode an approval event payload's base64 digest for CLI comparison.
    #[allow(clippy::too_many_arguments)]
    pub fn approval_digest_hex(
        &self,
        approval_id: &str,
        session_id: &str,
        tool_name: &str,
        call_id: &str,
        reason: &str,
        version: i64,
        expires_at_unix_ms: i64,
    ) -> String {
        hex::encode(crate::security::approval_digest(
            approval_id,
            session_id,
            tool_name,
            call_id,
            reason,
            version,
            expires_at_unix_ms,
        ))
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod command_recovery_tests;

#[cfg(test)]
mod pairing_tests;

#[cfg(test)]
mod diagnostics_tests;

#[cfg(test)]
mod approval_revocation_tests;

#[cfg(test)]
#[path = "tail_tests.rs"]
mod tail_tests;

#[path = "watch.rs"]
mod watch;
