//! Core orchestration: lifecycle, pairing, snapshot queries, commands, sync
//! and diagnostics. CLI calls this type only.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
#[cfg(all(test, unix))]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use url::Url;
use whalerein_proto::{
    ApprovalDecision, GetApproval, GetSession, Interrupt, ListSessions, RequestPayload,
    ResolveApproval, ResponsePayload, SendMessage,
};

use super::config::{self, ConfigStore, PersistedConfig};
use super::error::{CoreError, CoreResult};
use super::instance::{ClientConfig, ClientService, Instance, InstanceSecrets};
use super::types::{
    ApprovalInfo, CommandHandle, CreateSessionInput, DeliveryMode, DoctorReport, ExportManifest,
    HostCapabilities, HostInfo, ModelCatalog, ModelChoice, ModelSelectionReceipt, OnlineStatus,
    PairingOutcome, PairingPrompt, PendingCommand, PendingQueue, PendingQueueAction,
    QuestionAnswer, QuestionBatchInfo, QuestionPageInfo, QuestionPhase, RevocationOutcome,
    RevocationRetention, SessionList, SessionModel, SessionSummary,
};
use crate::access::{AccessClient, MqttSettings};
use crate::api::{Cancellation, ConnectionSnapshot};
use crate::cache::Cache;
use crate::commands::CommandService;
use crate::diagnostics;
use crate::diagnostics::journal::{DiagnosticContext, DiagnosticKind, DiagnosticSink, Diagnostics};
use crate::pairing::{default_capabilities, parse_pairing_uri, PairingDecision, PairingInitiator};
use crate::runtime::channel::{
    ensure_channel_after_on, ensure_channel_on, require_payload_capability,
};
use crate::runtime::{ConnectionState, Runtime};
use crate::security::{keys, load_device_token, load_or_create_cache_key, SecretStore};
use crate::session::SessionKey;
use crate::sync::SyncService;

pub(super) mod cloud_pairing;
pub(crate) mod revoked_commands;

/// Internal options for selecting storage and credentials.
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

/// Fork result for callers that present the child location.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForkOutcome {
    pub handle: CommandHandle,
    /// Confirmed child working directory, when the child summary was readable.
    pub working_directory: Option<String>,
    /// Why the directory could not be resolved; the fork itself still
    /// succeeded and must not be retried or deleted.
    pub working_directory_error: Option<String>,
}

/// Refuse to write a diagnostic bundle through a symbolic link.
///
/// The check runs before any create/write so the target is untouched. On Unix
/// the write itself also uses `O_NOFOLLOW`, closing the check-then-write race.
fn ensure_export_target_is_not_a_symlink(path: &Path) -> CoreResult<()> {
    if std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(CoreError::InvalidInput(
            "diagnostic export target must not be a symbolic link".into(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn export_symlink_error() -> CoreError {
    CoreError::InvalidInput("diagnostic export target must not be a symbolic link".into())
}

fn write_private_bundle(path: &Path, bytes: &[u8]) -> CoreResult<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| {
                if error.raw_os_error() == Some(libc::ELOOP) {
                    export_symlink_error()
                } else {
                    CoreError::Internal("diagnostic bundle could not be written".into())
                }
            })?;
        file.write_all(bytes)
            .map_err(|_| CoreError::Internal("diagnostic bundle could not be written".into()))?;
    }
    #[cfg(not(unix))]
    {
        ensure_export_target_is_not_a_symlink(path)?;
        std::fs::write(path, bytes)
            .map_err(|_| CoreError::Internal("diagnostic bundle could not be written".into()))?;
    }
    Ok(())
}

/// Whether a Host path is absolute under either POSIX or Windows rules.
///
/// The value is interpreted on the Host, so the local platform must not decide
/// its validity. Accept both absolute forms and reject clearly relative paths.
fn is_absolute_host_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with("\\\\") {
        return true;
    }
    let mut chars = value.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some('/' | '\\')) if drive.is_ascii_alphabetic()
    )
}

/// Bounded, secret-free candidate Host list for an ambiguous approval choice.
/// Mirrors the session-routing ambiguity message so the CLI can render an
/// actionable `--host` choice without dumping every Host.
#[cfg(test)]
fn bounded_approval_host_candidates(hosts: &[HostInfo]) -> String {
    const MAX_HOSTS: usize = 8;
    const MAX_HOST_CHARS: usize = 48;
    let mut shown = hosts
        .iter()
        .take(MAX_HOSTS)
        .map(|host| {
            host.host_id
                .chars()
                .take(MAX_HOST_CHARS)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(", ");
    if hosts.len() > MAX_HOSTS {
        shown.push_str(&format!(" and {} more", hosts.len() - MAX_HOSTS));
    }
    shown
}

/// Convert a Core question phase to its wire value for a guarded answer.
fn wire_question_phase(phase: QuestionPhase) -> i32 {
    match phase {
        QuestionPhase::Open => whalerein_proto::QuestionPhase::QuestionOpen as i32,
        QuestionPhase::Continued => whalerein_proto::QuestionPhase::QuestionContinued as i32,
        QuestionPhase::Settled => whalerein_proto::QuestionPhase::QuestionSettled as i32,
        QuestionPhase::ReplyQueued => whalerein_proto::QuestionPhase::QuestionReplyQueued as i32,
    }
}

#[derive(Clone)]
pub struct Core {
    options: CoreOptions,
    pub(crate) config: Arc<RwLock<Option<PersistedConfig>>>,
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) diagnostics: Arc<Diagnostics>,
    identity: Arc<Mutex<Option<Arc<crate::security::device::DeviceIdentity>>>>,
    identity_init: Arc<Mutex<()>>,
    cache: Arc<Mutex<Option<Arc<Cache>>>>,
    pub(crate) watch_active: Arc<AtomicBool>,
    /// One device connection per Host, independent of any session.
    connections: Arc<crate::connection::HostConnections>,
    pub(crate) attachments: Arc<crate::attachments::Downloads>,
    instance: Option<Arc<Instance>>,
    /// Platform anchors supplied as data by a client that must extract them
    /// from a store Rust cannot enumerate. System mode uses them instead of
    /// rediscovering the store through a process-global path.
    platform_roots: Option<Arc<Vec<u8>>>,
    #[cfg(test)]
    profile: Option<crate::profiles::ProfileContext>,
    #[cfg(test)]
    _activation: Option<Arc<crate::profiles::ActivationGuard>>,
    #[cfg(all(test, unix))]
    fail_waiting_marker: Arc<AtomicBool>,
    /// Test-only compatibility mode for fixtures written against the retired
    /// HTTP query lane. Production always injects the shared connection owner;
    /// focused epoch-5 tests call `use_mqtt_queries` to exercise the real lane.
    #[cfg(test)]
    legacy_http_queries: Arc<AtomicBool>,
}

impl Core {
    /// Open one platform-selected TOML configuration and validate existing state offline.
    pub fn open(config_path: &Path, secrets: Arc<dyn SecretStore>) -> CoreResult<Self> {
        Self::open_instance(config_path, secrets, Arc::default(), None)
    }

    /// Open one configuration with platform anchors supplied as data.
    ///
    /// A client that must extract its platform store (Android) passes the
    /// anchors here instead of mutating a process-global certificate path.
    pub fn open_with_platform_roots(
        config_path: &Path,
        secrets: Arc<dyn SecretStore>,
        platform_roots: Option<Vec<u8>>,
    ) -> CoreResult<Self> {
        Self::open_instance(config_path, secrets, Arc::default(), platform_roots)
    }

    /// Open a client with an instance-local receiver, including configuration diagnostics.
    pub fn open_with_diagnostic_sink(
        config_path: &Path,
        secrets: Arc<dyn SecretStore>,
        sink: Arc<dyn DiagnosticSink>,
    ) -> CoreResult<Self> {
        Self::open_with_diagnostics(config_path, secrets, DiagnosticContext::new(sink))
    }

    /// Open a client with a trace reserved before configuration is loaded.
    pub fn open_with_diagnostics(
        config_path: &Path,
        secrets: Arc<dyn SecretStore>,
        diagnostics: DiagnosticContext,
    ) -> CoreResult<Self> {
        Self::open_instance(config_path, secrets, diagnostics.0, None)
    }

    /// As [`Core::open_with_diagnostics`], with platform anchors as data.
    pub fn open_with_diagnostics_and_platform_roots(
        config_path: &Path,
        secrets: Arc<dyn SecretStore>,
        diagnostics: DiagnosticContext,
        platform_roots: Option<Vec<u8>>,
    ) -> CoreResult<Self> {
        Self::open_instance(config_path, secrets, diagnostics.0, platform_roots)
    }

    fn open_instance(
        config_path: &Path,
        secrets: Arc<dyn SecretStore>,
        diagnostics: Arc<Diagnostics>,
        platform_roots: Option<Vec<u8>>,
    ) -> CoreResult<Self> {
        let loaded = Instance::open(config_path).and_then(|instance| {
            let data_dir = instance.runtime_dir();
            let config = ConfigStore::for_instance(&data_dir)
                .load()
                .map_err(|error| {
                    CoreError::Config(format!(
                        "{}: {error}; choose a fresh data_dir",
                        instance.root.display()
                    ))
                })?;
            Ok((instance, data_dir, config))
        });
        diagnostics.record(match &loaded {
            Ok(_) => DiagnosticKind::ConfigurationLoaded,
            Err(error) => DiagnosticKind::ConfigurationFailed { code: error.code() },
        });
        let (instance, data_dir, config) = loaded?;
        let instance = Arc::new(instance);
        let secrets = Arc::new(InstanceSecrets {
            instance: instance.clone(),
            backend: secrets,
        });
        Ok(Self::from_parts(
            CoreOptions { data_dir, secrets },
            config,
            Some(instance),
            diagnostics,
            platform_roots,
        ))
    }

    /// Create or replace a validated configuration using the shared typed contract.
    pub fn write_config(config_path: &Path, config: &ClientConfig) -> CoreResult<()> {
        Instance::write(config_path, config)
    }

    /// Validate one configuration with the same rules as [`Core::open`].
    ///
    /// Shared by the CLI file-opening path and typed bindings that keep their
    /// own authoritative document. Nothing is written and no instance state is
    /// created.
    pub fn validate_client_config(config_path: &Path, config: &ClientConfig) -> CoreResult<()> {
        Instance::validate(config_path, config)
    }

    /// Durable atomic replacement for a configuration document.
    ///
    /// Uses Core's shared configuration writer (temporary file in the target
    /// directory, fsync, rename, directory fsync). The previous complete
    /// document survives any failure before the rename.
    pub fn write_config_atomically(path: &Path, bytes: &[u8]) -> CoreResult<()> {
        let selected = std::path::absolute(path)
            .map_err(|_| CoreError::Config("configuration path could not be resolved".into()))?;
        crate::configuration::writer::write_atomically(&selected, bytes).map_err(|_| {
            CoreError::Config(format!(
                "{}: could not write the configuration atomically",
                selected.display()
            ))
        })
    }

    pub fn client_config(&self) -> &ClientConfig {
        &self
            .instance
            .as_ref()
            .expect("selected client instance")
            .configuration
    }

    pub fn config_path(&self) -> &Path {
        &self
            .instance
            .as_ref()
            .expect("selected client instance")
            .config_path
    }

    pub fn configured_data_dir(&self) -> &Path {
        &self
            .instance
            .as_ref()
            .expect("selected client instance")
            .root
    }

    pub fn service_label(&self) -> &str {
        self.instance
            .as_ref()
            .and_then(|instance| instance.cloud.as_ref())
            .and_then(|cloud| cloud.definition.label.as_deref())
            .unwrap_or("Standalone")
    }

    pub fn instance_id(&self) -> CoreResult<Option<String>> {
        match &self.instance {
            Some(instance) => Ok(instance.manifest()?.map(|manifest| manifest.id)),
            None => Ok(None),
        }
    }

    pub(crate) fn ensure_instance_state(&self) -> CoreResult<()> {
        if let Some(instance) = &self.instance {
            instance.prepare_runtime()?;
        }
        Ok(())
    }

    /// Platform anchors supplied as data, when the client extracted them.
    fn platform_roots(&self) -> Option<Vec<u8>> {
        self.platform_roots
            .as_ref()
            .map(|roots| roots.as_ref().clone())
    }

    pub(crate) fn ca_pem(&self) -> Option<Vec<u8>> {
        match &self.instance {
            Some(instance) => instance.ca_pem.clone(),
            #[cfg(test)]
            None => config::ca_pem_from_env(),
            #[cfg(not(test))]
            None => None,
        }
    }

    fn mqtt_override(&self) -> Option<String> {
        #[cfg(test)]
        {
            self.instance
                .is_none()
                .then(config::mqtt_url_from_env)
                .flatten()
        }
        #[cfg(not(test))]
        None
    }

    fn from_parts(
        options: CoreOptions,
        config: Option<PersistedConfig>,
        instance: Option<Arc<Instance>>,
        diagnostics: Arc<Diagnostics>,
        platform_roots: Option<Vec<u8>>,
    ) -> Self {
        let runtime = Arc::new(Runtime::new());
        if let Some(instance) = &instance {
            runtime.set_transport_budget(instance.transport);
        }
        Self {
            options,
            config: Arc::new(RwLock::new(config)),
            runtime,
            diagnostics,
            identity: Arc::new(Mutex::new(None)),
            identity_init: Arc::new(Mutex::new(())),
            cache: Arc::new(Mutex::new(None)),
            watch_active: Arc::new(AtomicBool::new(false)),
            connections: Arc::new(crate::connection::HostConnections::new()),
            attachments: Arc::new(crate::attachments::Downloads::new()),
            instance,
            platform_roots: platform_roots.map(Arc::new),
            #[cfg(test)]
            profile: None,
            #[cfg(test)]
            _activation: None,
            #[cfg(all(test, unix))]
            fail_waiting_marker: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            legacy_http_queries: Arc::new(AtomicBool::new(true)),
        }
    }

    pub async fn login(
        &self,
        present: impl FnOnce(crate::api::AccountLoginPrompt),
    ) -> CoreResult<crate::api::SignedInAccount> {
        self.account_client(true).await?.login(present).await
    }

    /// End this App session. A false result means remote sign-out is pending.
    pub async fn logout(&self) -> CoreResult<bool> {
        let expected = self.cloud_session_id().ok().flatten();
        match self.account_client(false).await {
            Ok(client) => {
                let complete = client.logout().await?;
                self.runtime
                    .disconnect_cloud_session_if(expected.as_deref());
                Ok(complete)
            }
            Err(CoreError::Unauthorized) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// Inspect the locally retained verified account without network requests or refresh.
    pub fn account_status(&self) -> CoreResult<Option<crate::api::SignedInAccount>> {
        let Some(instance) = &self.instance else {
            return Ok(None);
        };
        crate::account::native::AccountClient::local_status(instance, &self.options.secrets)
    }

    pub async fn cloud_account_sessions(&self) -> CoreResult<Vec<crate::api::AccountSessionInfo>> {
        self.account_client(false).await?.sessions().await
    }

    pub async fn revoke_cloud_account_session(
        &self,
        id: &str,
    ) -> CoreResult<crate::api::AccountSessionState> {
        let current = self.cloud_session_id()?;
        let state = self.account_client(false).await?.revoke_session(id).await?;
        if current.as_deref() == Some(id) {
            self.runtime.disconnect_cloud_session_if(current.as_deref());
        }
        Ok(state)
    }

    pub async fn revoke_all_cloud_account_sessions(
        &self,
    ) -> CoreResult<crate::api::AccountSessionState> {
        let expected = self.cloud_session_id()?;
        let result = self
            .account_client(false)
            .await?
            .revoke_all_sessions()
            .await;
        match self.cloud_session_id() {
            Ok(current) if current == expected => {}
            Ok(_) | Err(CoreError::AuthenticationRequired) => {
                self.runtime
                    .disconnect_cloud_session_if(expected.as_deref());
            }
            Err(error) => return Err(error),
        }
        result
    }

    pub async fn account_hosts(&self) -> CoreResult<Vec<crate::api::AccountHost>> {
        let device_id = if self.options.data_dir.is_dir() {
            crate::security::device::existing_device_id(
                &self.options.secrets,
                &self.options.data_dir,
            )?
        } else {
            None
        };
        self.account_client(false)
            .await?
            .hosts_for_device(device_id.as_deref())
            .await
    }

    /// Begin the native iOS authorization-code flow. Core owns the attempt,
    /// PKCE material, Cloud state and local credential material.
    pub async fn cloud_auth_start(&self) -> CoreResult<crate::api::CloudAuthAttempt> {
        self.account_client(true).await?.auth_start().await
    }

    /// Begin the Android authorization-code flow with the pinned local client,
    /// `/api/v1/client/android` routes and the HTTPS App Link callback.
    pub async fn cloud_auth_start_android(&self) -> CoreResult<crate::api::CloudAuthAttempt> {
        self.account_client(true)
            .await?
            .auth_start_platform(crate::api::CloudAuthPlatform::Android)
            .await
    }

    /// Recover a lost exchange response with the pending attempt credential and
    /// the scoped `/me`. Never re-exchanges the authorization code.
    pub async fn cloud_auth_recover(&self) -> CoreResult<crate::api::SignedInAccount> {
        self.account_client(false).await?.auth_recover().await
    }

    /// Complete the exact pending iOS attempt from the callback URL. This is
    /// the only path that binds an account for the native flow.
    pub async fn cloud_auth_complete(
        &self,
        callback_url: &str,
    ) -> CoreResult<crate::api::SignedInAccount> {
        if callback_url.is_empty() || callback_url.len() > 8192 {
            return Err(CoreError::InvalidInput(
                "invalid Cloud sign-in callback".into(),
            ));
        }
        self.account_client(false)
            .await?
            .auth_complete(callback_url)
            .await
    }

    /// Drop the pending iOS attempt. A later callback cannot complete it.
    pub async fn cloud_auth_cancel(&self) -> CoreResult<()> {
        match self.account_client(false).await {
            Ok(client) => client.auth_cancel().await,
            Err(CoreError::Unauthorized) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Refresh the verified Cloud session against Cloud. A revoked or expired
    /// session clears the local active session and reports re-authentication.
    pub async fn refresh_account_status(&self) -> CoreResult<crate::api::SignedInAccount> {
        self.account_client(false).await?.status().await
    }

    /// True when a local sign-out still needs remote confirmation.
    pub fn account_logout_pending(&self) -> CoreResult<bool> {
        crate::account::native::AccountClient::local_logout_pending(&self.options.secrets)
    }

    async fn account_client(
        &self,
        create: bool,
    ) -> CoreResult<crate::account::native::AccountClient> {
        let instance = self.instance.as_ref().ok_or(CoreError::Unsupported)?;
        crate::account::native::AccountClient::for_instance(
            instance.clone(),
            self.options.secrets.clone(),
            create,
            self.platform_roots(),
        )
        .await
    }
    /// Allocate a non-secret command identity before starting a control.
    pub fn new_command_id() -> String {
        crate::util::random_id(16)
    }
    #[cfg(test)]
    pub fn new(mut options: CoreOptions) -> CoreResult<Self> {
        let root = options.data_dir.clone();
        let existed = root.exists();
        std::fs::create_dir_all(&root)
            .map_err(|_| CoreError::Config("data directory could not be created".into()))?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut open = std::fs::OpenOptions::new();
            open.read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let directory = open.open(&root).map_err(|_| {
                CoreError::Config("data directory could not be opened safely".into())
            })?;
            if !directory
                .metadata()
                .map_err(|_| CoreError::Config("data directory could not be inspected".into()))?
                .is_dir()
            {
                return Err(CoreError::Config(
                    "data directory is not a directory".into(),
                ));
            }
            directory
                .set_permissions(std::fs::Permissions::from_mode(0o700))
                .map_err(|_| {
                    CoreError::Config("data directory permissions could not be set".into())
                })?;
        }
        #[cfg(not(unix))]
        let _ = existed;
        // The shared activation lease is acquired before the active profile is
        // resolved: a switch that completes while this process waits cannot
        // leave Core on a selection the lease no longer covers.
        let activation = Arc::new(crate::profiles::ActivationGuard::shared(&root)?);
        let profiles = crate::profiles::Profiles::new(&root);
        let profile = profiles.resolve_active(&options.secrets)?;
        options.data_dir = profile.state_dir.clone();
        let store = ConfigStore::new(&options.data_dir);
        let startup_marker = revoked_commands::marker(&options.data_dir)?;
        let config = match store.load() {
            Ok(config) => config,
            Err(error)
                if startup_marker
                    .as_ref()
                    .is_some_and(|value| value.starts_with("confirmed:")) =>
            {
                if let Some(device_id) = crate::security::device::existing_device_id(
                    &options.secrets,
                    &options.data_dir,
                )? {
                    let guard = crate::security::device::ProfileLock::acquire_admission_waiting(
                        &options.data_dir,
                    )
                    .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
                    if revoked_commands::marker_in(guard.directory())? == startup_marker {
                        Self::clear_pairing_secrets_with(&options.secrets, &device_id)?;
                    }
                }
                return Err(CoreError::Internal(format!(
                    "revocation cleanup requires manual inspection; configuration could not be read ({})",
                    error.code()
                )));
            }
            Err(error) => return Err(error),
        };
        let mut core = Self::from_parts(options, config, None, Arc::default(), None);
        core.profile = Some(profile);
        core._activation = Some(activation);
        if startup_marker
            .as_deref()
            .and_then(revoked_commands::confirmed_phase)
            == Some("conflict")
        {
            let configured_device = core.current_config().map(|config| config.device_id);
            if let Some(device_id) = crate::security::device::existing_device_id(
                &core.options.secrets,
                &core.options.data_dir,
            )?
            .or_else(|| configured_device.clone())
            {
                let guard = crate::security::device::ProfileLock::acquire_admission_waiting(
                    &core.options.data_dir,
                )
                .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
                if revoked_commands::marker_in(guard.directory())? == startup_marker {
                    let pending =
                        core.redact_pending_pairing_after_revocation(&device_id, guard.directory());
                    let secrets = core.clear_local_pairing_secrets(&device_id);
                    pending?;
                    secrets?;
                    if configured_device.is_some() {
                        return Err(CoreError::Internal("revocation cleanup requires manual inspection; command retention is unverified".into()));
                    }
                }
            }
        } else if startup_marker
            .as_ref()
            .is_some_and(|v| v.starts_with("confirmed"))
        {
            if let Some(config) = core.current_config() {
                core.clear_local_pairing_inner(&config.device_id, true, startup_marker.as_deref())?;
            } else if let Some(device_id) = crate::security::device::existing_device_id(
                &core.options.secrets,
                &core.options.data_dir,
            )? {
                #[cfg(feature = "protocol-testing")]
                if let Some(path) = std::env::var_os("WHALEREIN_TEST_STARTUP_FENCE_BARRIER") {
                    let path = std::path::PathBuf::from(path);
                    std::fs::write(&path, b"paused")
                        .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
                    while path.exists() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                core.resume_unconfigured_revocation(
                    &device_id,
                    startup_marker.as_deref().expect("confirmed marker"),
                )?;
            }
        }
        Ok(core)
    }

    #[cfg(test)]
    fn resume_unconfigured_revocation(&self, device_id: &str, expected: &str) -> CoreResult<()> {
        let _admission =
            crate::security::device::ProfileLock::acquire_admission_waiting(&self.options.data_dir)
                .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
        if revoked_commands::marker_in(_admission.directory())?.as_deref() == Some(expected)
            && self.config_store().load()?.is_none()
        {
            revoked_commands::ensure_pairing_epoch_in(_admission.directory())?;
            if self
                .redact_pending_pairing_after_revocation(device_id, _admission.directory())
                .is_err()
            {
                revoked_commands::write_marker_in(_admission.directory(), "confirmed-lost")?;
            }
            self.clear_local_pairing_credentials(device_id, _admission.directory(), None)?;
        } else {
            *self.config.write().expect("config lock") = self.config_store().load()?;
        }
        Ok(())
    }

    /// Ephemeral operation ID shared with Access and host logs.
    pub fn diagnostic_id(&self) -> &str {
        self.diagnostics.trace_id()
    }

    /// Persist only bounded diagnostic metadata, never the error message.
    pub fn save_failure(&self, code: &str) -> CoreResult<()> {
        if self.instance.is_some() && !self.options.data_dir.is_dir() {
            return Ok(());
        }
        self.diagnostics
            .save_failure(&self.options.data_dir, code)
            .inspect_err(|_| {
                self.diagnostics
                    .record(DiagnosticKind::DiagnosticSaveFailed)
            })
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

    /// Resolved local profile for this regional Core instance. Native account
    /// sign-in and directory reads use the account client.
    #[cfg(test)]
    pub fn profile(&self) -> &crate::profiles::ProfileContext {
        self.profile.as_ref().expect("legacy profile")
    }

    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn is_paired(&self) -> bool {
        self.config.read().expect("config lock").is_some()
    }

    /// Local enforcement evidence. This check does not contact Cloud or Access.
    pub fn local_revocation_state(&self) -> CoreResult<Option<&'static str>> {
        if !self.options.data_dir.is_dir() {
            return Ok(None);
        }
        if let Some(state) = self.local_cloud_revocation_state()? {
            return Ok(Some(state));
        }
        match revoked_commands::marker(&self.options.data_dir)? {
            Some(marker) if marker.starts_with("confirmed:") => Ok(Some("revoked")),
            Some(marker) if marker.starts_with("pending:") => Ok(Some("revocation_pending")),
            _ => Ok(None),
        }
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

    pub(crate) fn transport_budget(&self) -> crate::access::transport::TransportBudget {
        self.runtime.transport_budget()
    }

    fn outbound_envelope_bytes(&self, host_id: &str) -> u64 {
        let local = self.transport_budget().envelope_bytes;
        self.runtime
            .host_receive(host_id)
            .map_or(local, |peer| local.min(peer.envelope_bytes))
    }

    /// Resolve one live event to its complete verified ciphertext. A reference
    /// is fetched from the Access HTTPS directory with the current Device
    /// bearer; inline events pass through unchanged.
    pub(crate) async fn resolve_event_envelope(
        &self,
        envelope: &whalerein_proto::EventEnvelope,
    ) -> CoreResult<whalerein_proto::EventEnvelope> {
        let access = self.access_client().await?;
        // A referenced control event must fit the local control-envelope
        // budget; refuse a larger declaration before any fetch.
        let max_ciphertext_bytes = self.transport_budget().envelope_bytes;
        crate::access::transport::resolve_event_envelope(&access, envelope, max_ciphertext_bytes)
            .await
            .map_err(CoreError::from)
    }

    fn current_config(&self) -> Option<PersistedConfig> {
        self.config.read().expect("config lock").clone()
    }

    pub(crate) fn require_config(&self) -> CoreResult<PersistedConfig> {
        self.current_config().ok_or(CoreError::NotPaired)
    }

    fn config_store(&self) -> ConfigStore {
        #[cfg(test)]
        if self.instance.is_none() {
            return ConfigStore::new(&self.options.data_dir);
        }
        ConfigStore::for_instance(&self.options.data_dir)
    }

    fn refresh_regional_state(&self) -> CoreResult<()> {
        if self.instance.is_some() {
            let state = self.config_store().load()?;
            *self.config.write().expect("config lock") = state;
        }
        Ok(())
    }

    pub(crate) fn cloud_session_id(&self) -> CoreResult<Option<String>> {
        let Some(instance) = self
            .instance
            .as_ref()
            .filter(|instance| instance.cloud.is_some())
        else {
            return Ok(None);
        };
        crate::account::native::AccountClient::local_session_id(instance, &self.options.secrets)
            .and_then(|id| id.map(Some).ok_or(CoreError::AuthenticationRequired))
    }

    /// The locally retained Cloud app session identity for scoped UI state.
    ///
    /// Account app-session listings do not mark which row belongs to this app;
    /// the binding pairs this read-only identity with the list so Settings can
    /// label the current sign-in without a second session store.
    pub fn current_cloud_session_id(&self) -> CoreResult<Option<String>> {
        match self.cloud_session_id() {
            // A fresh Cloud profile has a manifest but no signed-in session.
            // For this read-only accessor that is absence, not authentication
            // failure; a real store failure still propagates.
            Err(CoreError::AuthenticationRequired) => Ok(None),
            other => other,
        }
    }

    pub(crate) async fn cloud_transport(
        &self,
    ) -> CoreResult<Option<crate::account::native::RegionalSession>> {
        if self.cloud_session_id()?.is_none() {
            return Ok(None);
        }
        let identity = self.device_identity()?;
        let region = self
            .account_client(false)
            .await?
            .regional_session(&identity)
            .await?;
        if self.cloud_session_id()?.as_deref() != Some(&region.id) {
            return Err(CoreError::AuthenticationRequired);
        }
        self.runtime.bind_cloud_session(&region.id);
        Ok(Some(region))
    }

    fn regional_access(
        &self,
        base: Url,
        region: crate::account::native::RegionalSession,
    ) -> CoreResult<AccessClient> {
        let ca = self.ca_pem();
        let roots = self.platform_roots();
        Ok(AccessClient::with_trust(
            base,
            Some(region.token),
            ca.as_deref(),
            roots.as_deref(),
            self.diagnostics.clone(),
        )?
        .with_transport_budget(self.transport_budget())
        .with_cloud_session(region.id, self.options.secrets.clone()))
    }

    async fn access_client(&self) -> CoreResult<AccessClient> {
        let config = self.require_config()?;
        let base = Url::parse(&config.access_url)
            .map_err(|_| CoreError::Config("invalid Access URL".into()))?;
        // A paired Device keeps its durable regional Access token. When the
        // Cloud account/transport service is temporarily unreachable, use that
        // saved authority instead of failing the local Host operation. A
        // definitive account refusal still propagates.
        let cloud = match self.cloud_transport().await {
            Ok(region) => region,
            Err(CoreError::Unavailable(_)) | Err(CoreError::Timeout) => None,
            Err(error @ CoreError::Api { .. }) if error.is_retryable_transport() => None,
            Err(error) => return Err(error),
        };
        if let Some(region) = cloud {
            let origin = region
                .permission
                .as_ref()
                .ok_or(CoreError::AuthenticationRequired)?
                .access_origin
                .clone();
            let access = self.regional_access(origin.clone(), region)?;
            if origin != base {
                self.adopt_cloud_route(&access, &config, None).await?;
                if let Some(id) = self.cloud_session_id()? {
                    self.runtime.bind_cloud_session(&id);
                }
            }
            return Ok(access);
        }
        let token = load_device_token(&self.options.secrets, &config.device_id)?
            .ok_or(CoreError::NotPaired)?;
        let ca = self.ca_pem();
        let roots = self.platform_roots();
        AccessClient::with_trust(
            base,
            Some(token),
            ca.as_deref(),
            roots.as_deref(),
            self.diagnostics.clone(),
        )
        .map(|access| access.with_transport_budget(self.transport_budget()))
        .map_err(Into::into)
    }

    async fn adopt_cloud_route(
        &self,
        access: &AccessClient,
        expected: &PersistedConfig,
        target: Option<&str>,
    ) -> CoreResult<()> {
        let hosts = access.list_hosts().await?;
        let confirmed = hosts.iter().any(|host| {
            target.is_none_or(|target| host.host_id == target)
                && (if host.host_id == expected.host_id {
                    expected.host_fingerprint.as_ref()
                } else {
                    expected.host_fingerprints.get(&host.host_id)
                }) == Some(&host.fingerprint)
        });
        if !confirmed {
            return Err(CoreError::Unavailable(
                "No pinned Host grant is available on the current route".into(),
            ));
        }
        let _admission = crate::security::device::ProfileLock::acquire_admission_async(
            &self.options.data_dir,
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
        if access.cloud_session_current(Some(&expected.device_id))? != Some(true) {
            return Err(CoreError::AuthenticationRequired);
        }
        let mut latest = self.config_store().load()?.ok_or(CoreError::NotPaired)?;
        let origin = access.base_url().as_str();
        if latest == *expected {
            latest.access_url = origin.into();
            latest.mqtt_url = None;
            self.config_store().save(&latest)?;
        } else if latest.device_id != expected.device_id || latest.access_url != origin {
            return Err(CoreError::Conflict(
                "the active regional route changed".into(),
            ));
        }
        *self.config.write().expect("config lock") = Some(latest);
        self.runtime.disconnect_cloud_session();
        Ok(())
    }

    fn verify_access_generation(&self, access: &AccessClient) -> CoreResult<()> {
        let config = self.config_store().load()?.ok_or(CoreError::NotPaired)?;
        let current = match access.cloud_session_current(Some(&config.device_id))? {
            Some(current) => current,
            None => {
                let token = load_device_token(&self.options.secrets, &config.device_id)?
                    .ok_or(CoreError::NotPaired)?;
                access.has_token(&token)
            }
        };
        if self
            .current_config()
            .as_ref()
            .is_none_or(|original| original.device_id != config.device_id)
            || !current
            || url::Url::parse(&config.access_url).ok().as_ref() != Some(access.base_url())
        {
            return Err(CoreError::Conflict(
                "pairing changed while checking authorization".into(),
            ));
        }
        Ok(())
    }

    fn identity_lock(&self) -> CoreResult<crate::security::device::ProfileLock> {
        crate::security::device::ProfileLock::acquire_identity_waiting(&self.options.data_dir)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(32)
                {
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
        // Loading the same cached identity from concurrent Core clones is one
        // initialization, not a competing profile identity operation.
        let _init = self.identity_init.lock().expect("identity init mutex");
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
        if self.instance.is_some() && !self.options.data_dir.is_dir() {
            return Ok(());
        }
        let _guard = self.identity_lock()?;
        self.refresh_regional_state()?;
        if !local_only
            && self.is_paired()
            && !self.runtime.is_revoked()
            && !self.cloud_grants_locally_revoked()?
        {
            return Err(CoreError::Conflict(
                "revoke the active pairing before forgetting this device".into(),
            ));
        }
        let _admission =
            crate::security::device::ProfileLock::acquire_admission_waiting(&self.options.data_dir)
                .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
        if !local_only
            && self.is_paired()
            && !self.runtime.is_revoked()
            && !self.cloud_grants_locally_revoked()?
        {
            return Err(CoreError::Conflict(
                "revoke the active pairing before forgetting this device".into(),
            ));
        }
        self.runtime.revoke();
        if let Some(config) = self.current_config() {
            self.options
                .secrets
                .delete(&keys::device_private_key(&config.device_id))?;
            self.clear_local_pairing_credentials(&config.device_id, _guard.directory(), None)?;
        }
        crate::security::device::forget(&self.options.secrets, &self.options.data_dir)?;
        self.clear_cloud_pairing_material()?;
        *self.identity.lock().expect("identity mutex") = None;
        Ok(())
    }

    /// Rotate the client installation key while preserving its client id.
    /// Every Host grant is revoked first, then the Cloud registry is updated.
    pub async fn rotate_device(&self) -> CoreResult<()> {
        if self.instance.is_some() && !self.options.data_dir.is_dir() {
            return Ok(());
        }
        let _guard = self.identity_lock()?;
        self.refresh_regional_state()?;
        let identity = self.device_identity_locked(&_guard)?;
        let account = self.account_client(false).await?;
        let revoke_id = crate::util::random_id(16);
        let state = account
            .revoke_client_installation(&identity.device_id, &revoke_id)
            .await?;
        if state != "revoked" {
            return Err(CoreError::Conflict(
                "client revocation is still pending; retry rotation later".into(),
            ));
        }
        let key = crate::security::identity::StaticIdentity::generate();
        let rotate_id = crate::util::random_id(16);
        let state = account
            .rotate_client_installation(&identity.device_id, &rotate_id, key.public_key())
            .await?;
        if state != "active" {
            return Err(CoreError::ProtocolIncompatible);
        }
        crate::security::device::replace_key(&self.options.secrets, &self.options.data_dir, &key)?;
        self.runtime.revoke();
        self.clear_local_pairing_secrets(&identity.device_id)?;
        self.clear_cloud_pairing_material()?;
        *self.identity.lock().expect("identity mutex") = None;
        Ok(())
    }

    #[cfg(any(unix, windows))]
    fn admission_revocation_error(state: Option<&str>, warning: bool) -> Option<CoreError> {
        match state.and_then(revoked_commands::confirmed_phase) {
            Some("saved") if !warning => Some(CoreError::Revoked),
            Some(_) => Some(CoreError::RevokedRetentionUnverified),
            None => None,
        }
    }

    pub(crate) fn cache(&self) -> CoreResult<Arc<Cache>> {
        if self.instance.is_some() && !self.options.data_dir.is_dir() {
            return Err(CoreError::NotPaired);
        }
        #[cfg(any(unix, windows))]
        {
            let (state, warning) =
                revoked_commands::marker_with_retention_warning(&self.options.data_dir)?;
            if let Some(error) = Self::admission_revocation_error(state.as_deref(), warning) {
                #[cfg(windows)]
                if let Some(cache) = self.cache.lock().expect("cache lock").take() {
                    cache.release_for_revocation()?;
                }
                return Err(error);
            }
        }
        let config = self.require_config()?;
        let mut slot = self.cache.lock().expect("cache lock");
        if let Some(cache) = slot.as_ref() {
            #[cfg(any(unix, windows))]
            if !cache.is_revocation_only() {
                return Ok(cache.clone());
            }
            #[cfg(not(any(unix, windows)))]
            return Ok(cache.clone());
        }
        #[cfg(all(unix, feature = "protocol-testing"))]
        if let Some(path) = std::env::var_os("WHALEREIN_TEST_CACHE_INIT_BARRIER") {
            let path = std::path::PathBuf::from(path);
            std::fs::write(&path, b"paused")
                .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            while path.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(unix)]
        let admission = {
            let guard = crate::security::device::ProfileLock::acquire_admission_shared_waiting(
                &self.options.data_dir,
            )
            .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
            if let Some(error) = Self::admission_revocation_error(
                revoked_commands::marker_in(guard.directory())?.as_deref(),
                revoked_commands::retention_warning_in(guard.directory()),
            ) {
                return Err(error);
            }
            guard
        };
        #[cfg(any(unix, windows))]
        {
            slot.take();
        }
        let key_name = keys::cache_key(&config.device_id);
        let key = match self.options.secrets.load(&key_name)? {
            Some(bytes) => match <[u8; 32]>::try_from(bytes) {
                Ok(key) => key,
                Err(_) => {
                    // A malformed cache key cannot decrypt anything. Replace
                    // it under the identity lock; the cache is rebuilt below
                    // while pairing configuration and credentials stay intact.
                    let _guard = self.identity_lock()?;
                    self.options.secrets.delete(&key_name)?;
                    load_or_create_cache_key(&self.options.secrets, &config.device_id)?
                }
            },
            None => {
                let _guard = self.identity_lock()?;
                load_or_create_cache_key(&self.options.secrets, &config.device_id)?
            }
        };
        let (cache, rebuilt) =
            Cache::open_recovering(&self.options.data_dir.join("cache.sqlite3"), key)?;
        if let Some(reason) = rebuilt {
            self.write_cache_rebuild_marker(reason);
        }
        let cache = Arc::new(cache);
        *slot = Some(cache.clone());
        #[cfg(unix)]
        drop(admission);
        drop(slot);
        if let Some(reason) = rebuilt {
            self.diagnostics
                .record_stage("storage", "cache_rebuild", "succeeded", reason.reason());
        }
        Ok(cache)
    }

    /// Leave a bounded, secret-free marker when a cache is quarantined and
    /// rebuilt. `doctor` and diagnostics can report the storage state without
    /// opening the cache, and the pairing identity is explicitly preserved.
    fn write_cache_rebuild_marker(&self, reason: crate::cache::CacheRebuild) {
        let marker = serde_json::json!({
            "reason": reason.reason(),
            "quarantine": "cache.sqlite3.quarantine",
            "pairing_preserved": true,
            "at_unix_ms": crate::util::now_unix_ms(),
        });
        if let Ok(bytes) = serde_json::to_vec(&marker) {
            let _ = std::fs::write(self.options.data_dir.join("cache-recovery.json"), bytes);
        }
    }

    /// Why the local cache was last rebuilt, if it ever was. Plain local state;
    /// it never contains message content or credentials.
    pub fn cache_recovery_reason(&self) -> CoreResult<Option<String>> {
        let path = self.options.data_dir.join("cache-recovery.json");
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(CoreError::Cache("cache recovery marker unreadable".into())),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Cache("cache recovery marker corrupt".into()))?;
        Ok(value
            .get("reason")
            .and_then(|reason| reason.as_str())
            .map(str::to_owned))
    }

    /// Path of the quarantined database, whether or not it exists.
    pub fn cache_quarantine_path(&self) -> std::path::PathBuf {
        self.options.data_dir.join("cache.sqlite3.quarantine")
    }

    #[cfg(any(unix, windows))]
    fn try_prepare_revocation_cache(&self, directory: &std::fs::File) -> bool {
        let mut slot = match self.cache.try_lock() {
            Ok(slot) => slot,
            Err(std::sync::TryLockError::WouldBlock) => return false,
            Err(std::sync::TryLockError::Poisoned(_)) => return true,
        };
        if slot.is_some() {
            return true;
        }
        let Some(config) = self.current_config() else {
            return true;
        };
        let Ok(Some(bytes)) = self
            .options
            .secrets
            .load(&keys::cache_key(&config.device_id))
        else {
            return true;
        };
        let Ok(key) = <[u8; 32]>::try_from(bytes) else {
            return true;
        };
        if let Ok(cache) = Cache::open_existing_verified(
            &self.options.data_dir.join("cache.sqlite3"),
            key,
            directory,
        ) {
            *slot = Some(Arc::new(cache));
        }
        true
    }

    #[cfg(unix)]
    fn try_cached_for_revocation(&self) -> Result<Option<Arc<Cache>>, ()> {
        match self.cache.try_lock() {
            Ok(slot) => Ok(slot.clone()),
            Err(std::sync::TryLockError::WouldBlock) => Err(()),
            Err(std::sync::TryLockError::Poisoned(_)) => Ok(None),
        }
    }

    pub(crate) fn sync_service(&self) -> CoreResult<SyncService> {
        Ok(SyncService::new(self.cache()?))
    }

    #[cfg(test)]
    async fn command_service(&self) -> CoreResult<CommandService> {
        self.command_service_for_session(self.cloud_session_id()?.as_deref())
            .await
    }

    async fn command_service_for_session(
        &self,
        expected: Option<&str>,
    ) -> CoreResult<CommandService> {
        let access = Arc::new(self.access_client().await?);
        if access.cloud_session_id() != expected {
            return Err(CoreError::AuthenticationRequired);
        }
        let mut service = CommandService::new(access, self.cache()?)
            .with_transport_budget(self.transport_budget())
            .with_admission(
                self.config_store(),
                self.options.data_dir.clone(),
                self.options.secrets.clone(),
            );
        // Query replies and command notices are routed by the one shared Host
        // connection owner for this Core instance. A watch or sibling command
        // keeps its separate waiter without displacing the connection.
        #[cfg(test)]
        if self
            .legacy_http_queries
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(service);
        }
        service = service.with_connections(self.connections.clone());
        Ok(service)
    }

    fn cache_or_empty(&self) -> CoreResult<Arc<Cache>> {
        if self.is_paired() {
            self.cache()
        } else {
            Ok(Arc::new(Cache::open_in_memory([0u8; 32])?))
        }
    }

    /// Reject a key whose Host this profile does not authorize before I/O.
    ///
    /// Scope authorization is checked at the operation boundary so a key that
    /// was valid at resolution time cannot be used after revocation or after
    /// this profile stopped authorizing that Host.
    pub(crate) fn authorized_session(&self, key: &SessionKey) -> CoreResult<()> {
        if key.session_id.is_empty() {
            return Err(CoreError::InvalidInput("session identity missing".into()));
        }
        self.ensure_authorized_host(&key.host_id)?;
        if self.runtime.is_revoked() {
            return Err(CoreError::Revoked);
        }
        Ok(())
    }

    /// Resolve a Host that the current Access listing still advertises.
    #[cfg(test)]
    async fn live_authorized_host(&self, host_id: &str) -> CoreResult<String> {
        if host_id.trim().is_empty() {
            return Err(CoreError::InvalidInput("Host identity missing".into()));
        }
        if self
            .hosts()
            .await?
            .iter()
            .any(|host| host.host_id == host_id)
        {
            Ok(host_id.to_owned())
        } else {
            Err(CoreError::NotFound("Host is not authorized".into()))
        }
    }

    fn confirmed_host_fingerprint(&self, host_id: &str) -> CoreResult<String> {
        let config = self.require_config()?;
        let pin = if host_id == config.host_id {
            config.host_fingerprint
        } else {
            // Each additional Host must have its own explicitly confirmed pin.
            config.host_fingerprints.get(host_id).cloned()
        };
        pin.ok_or_else(|| {
            CoreError::Conflict("Host fingerprint is missing; explicit pairing is required".into())
        })
    }

    /// A broker can refuse a connection while its Access policy source is
    /// unreachable. Resolve a refusal against the authority that owns the grant.
    pub(crate) async fn wait_host_transport(
        &self,
        host_id: &str,
        connection: &crate::connection::HostConnection,
        cancellation: &Cancellation,
    ) -> CoreResult<()> {
        match connection.wait_ready(cancellation).await {
            Err(CoreError::Unauthorized) => {
                self.access_client()
                    .await?
                    .host_authority(host_id)
                    .await
                    .map_err(CoreError::from)?;
                Err(CoreError::Unavailable(
                    "broker has not admitted the authorized Device".into(),
                ))
            }
            result => result,
        }
    }

    pub(crate) async fn ensure_channel(
        &self,
        host_id: &str,
        force_new: bool,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.ensure_channel_inner(host_id, force_new, None).await
    }

    async fn ensure_channel_after(
        &self,
        host_id: &str,
        stale_channel_id: &str,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.ensure_channel_inner(host_id, true, Some(stale_channel_id))
            .await
    }

    async fn ensure_channel_inner(
        &self,
        host_id: &str,
        force_new: bool,
        stale_channel_id: Option<&str>,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.diagnostics
            .record_stage("core", "handshake", "started", "none");
        #[cfg(test)]
        if self
            .legacy_http_queries
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let config = self.require_config()?;
            let identity = self.device_identity()?;
            let access = self.access_client().await?;
            let expected = self.confirmed_host_fingerprint(host_id)?;
            return crate::runtime::channel::ensure_channel(
                &self.runtime,
                &access,
                &identity.key,
                host_id,
                &config.device_id,
                force_new,
                Some(&expected),
            )
            .await;
        }
        let handshake = async {
            let config = self.require_config()?;
            let identity = self.device_identity()?;
            let access = self.access_client().await?;
            let expected = self.confirmed_host_fingerprint(host_id)?;
            // Ordering: the authorized MQTT subscription and receive-budget
            // readiness come first; pinned Noise then travels on the same
            // connection's query/reply lane.
            let connection = self.host_connection(host_id).await?;
            self.wait_host_transport(host_id, &connection, &Cancellation::new())
                .await
                .inspect_err(|error| {
                    self.diagnostics.record_stage(
                        error.source_layer(),
                        "mqtt_subscription",
                        "unavailable",
                        error.code(),
                    );
                })?;
            if let Some(stale) = stale_channel_id {
                ensure_channel_after_on(
                    &self.runtime,
                    &access,
                    &identity.key,
                    &connection,
                    host_id,
                    &config.device_id,
                    stale,
                    Some(&expected),
                )
                .await
            } else {
                ensure_channel_on(
                    &self.runtime,
                    &access,
                    &identity.key,
                    &connection,
                    host_id,
                    &config.device_id,
                    force_new,
                    Some(&expected),
                )
                .await
            }
        };
        let opened = match tokio::time::timeout(
            Duration::from_millis(crate::runtime::channel::HANDSHAKE_TIMEOUT_MS as u64),
            handshake,
        )
        .await
        {
            Err(_) | Ok(Err(CoreError::Timeout)) => Err(CoreError::Unavailable(
                "Host channel establishment timed out".into(),
            )),
            Ok(result) => result,
        };
        self.diagnostics.record_stage(
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

    pub(crate) async fn query(
        &self,
        host_id: &str,
        payload: RequestPayload,
    ) -> CoreResult<ResponsePayload> {
        let payload_label = Self::request_payload_name(&payload);
        let expected_session = self.cloud_session_id()?;
        let result = async {
            self.ensure_authorized_host(host_id)?;
            let config = self.require_config()?;
            let channel = self.ensure_channel(host_id, false).await?;
            require_payload_capability(&self.runtime, host_id, &payload, &self.diagnostics)?;
            let commands = self
                .command_service_for_session(expected_session.as_deref())
                .await?
                .with_outbound_envelope(self.outbound_envelope_bytes(host_id));
            match commands
                .query(host_id, &config.device_id, &channel, payload.clone())
                .await
            {
                Err(error)
                    if matches!(error, CoreError::ChannelMismatch)
                        || error.host_channel_replaced()
                        || error.device_authorization_lost() =>
                {
                    // A host reconnect discards its channels. Read-only queries
                    // may negotiate once more; control commands are never resent.
                    self.diagnostics.record_stage(
                        error.source_layer(),
                        "query_channel_recovery",
                        "retrying",
                        error.code(),
                    );
                    let fresh = self
                        .ensure_channel_after(host_id, channel.channel_id())
                        .await?;
                    require_payload_capability(
                        &self.runtime,
                        host_id,
                        &payload,
                        &self.diagnostics,
                    )?;
                    commands
                        .query(host_id, &config.device_id, &fresh, payload)
                        .await
                }
                other => other,
            }
        }
        .await;
        self.diagnostics.record_query(
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
            payload_label,
        );
        self.settle(result).await
    }

    /// Fixed, content-free name for a query payload. A failed query must be
    /// attributable to its purpose; the transport layer only sees encrypted
    /// bodies and cannot answer that.
    fn request_payload_name(payload: &RequestPayload) -> &'static str {
        match payload.payload {
            Some(whalerein_proto::request_payload::Payload::ListSessions(_)) => "list_sessions",
            Some(whalerein_proto::request_payload::Payload::GetSession(_)) => "get_session",
            Some(whalerein_proto::request_payload::Payload::GetApproval(_)) => "get_approval",
            Some(whalerein_proto::request_payload::Payload::GetCommandResult(_)) => {
                "get_command_result"
            }
            Some(whalerein_proto::request_payload::Payload::SendMessage(_)) => "send_message",
            Some(whalerein_proto::request_payload::Payload::Interrupt(_)) => "interrupt",
            Some(whalerein_proto::request_payload::Payload::ResolveApproval(_)) => {
                "resolve_approval"
            }
            Some(whalerein_proto::request_payload::Payload::Ping(_)) => "ping",
            Some(whalerein_proto::request_payload::Payload::ListApprovals(_)) => "list_approvals",
            Some(whalerein_proto::request_payload::Payload::ReadHistory(_)) => "read_history",
            Some(whalerein_proto::request_payload::Payload::WatchSession(_)) => "watch_session",
            Some(whalerein_proto::request_payload::Payload::CheckVersions(_)) => "check_versions",
            Some(whalerein_proto::request_payload::Payload::UnwatchSession(_)) => "unwatch_session",
            Some(whalerein_proto::request_payload::Payload::ReadMessageReply(_)) => {
                "read_message_reply"
            }
            Some(whalerein_proto::request_payload::Payload::GetHostCapabilities(_)) => {
                "get_host_capabilities"
            }
            Some(whalerein_proto::request_payload::Payload::ListModels(_)) => "list_models",
            Some(whalerein_proto::request_payload::Payload::ReadSessionModel(_)) => {
                "read_session_model"
            }
            Some(whalerein_proto::request_payload::Payload::SelectSessionModel(_)) => {
                "select_session_model"
            }
            Some(whalerein_proto::request_payload::Payload::WaitSession(_)) => "wait_session",
            Some(whalerein_proto::request_payload::Payload::CancelSessionWait(_)) => {
                "cancel_session_wait"
            }
            Some(whalerein_proto::request_payload::Payload::ReadWaitState(_)) => "read_wait_state",
            Some(whalerein_proto::request_payload::Payload::CreateSession(_)) => "create_session",
            Some(whalerein_proto::request_payload::Payload::ReadPendingQueue(_)) => {
                "read_pending_queue"
            }
            Some(whalerein_proto::request_payload::Payload::UpdatePendingQueue(_)) => {
                "update_pending_queue"
            }
            Some(whalerein_proto::request_payload::Payload::ListQuestions(_)) => "list_questions",
            Some(whalerein_proto::request_payload::Payload::GetQuestion(_)) => "get_question",
            Some(whalerein_proto::request_payload::Payload::AnswerQuestion(_)) => "answer_question",
            Some(whalerein_proto::request_payload::Payload::CancelQuestionAnswer(_)) => {
                "cancel_question_answer"
            }
            Some(whalerein_proto::request_payload::Payload::ForkSession(_)) => "fork_session",
            Some(whalerein_proto::request_payload::Payload::OpenAttachment(_)) => "open_attachment",
            Some(whalerein_proto::request_payload::Payload::PullAttachment(_)) => "pull_attachment",
            Some(whalerein_proto::request_payload::Payload::CloseAttachment(_)) => {
                "close_attachment"
            }
            None => "unspecified",
        }
    }

    async fn control(&self, host_id: &str, payload: RequestPayload) -> CoreResult<CommandHandle> {
        self.control_with_id(host_id, payload, None).await
    }

    async fn control_with_id(
        &self,
        host_id: &str,
        payload: RequestPayload,
        command_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        self.control_with_identity(host_id, payload, command_id, None)
            .await
    }

    async fn control_with_identity(
        &self,
        host_id: &str,
        payload: RequestPayload,
        command_id: Option<&str>,
        request_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        let expected_session = self.cloud_session_id()?;
        let result = async {
            let config = self.require_config()?;
            let channel = self.ensure_channel(host_id, false).await?;
            require_payload_capability(&self.runtime, host_id, &payload, &self.diagnostics)?;
            let commands = self
                .command_service_for_session(expected_session.as_deref())
                .await?
                .with_outbound_envelope(self.outbound_envelope_bytes(host_id))
                .with_host_fingerprint(Some(self.confirmed_host_fingerprint(host_id)?));
            let submitted = match (command_id, request_id) {
                (None, None) => {
                    commands
                        .control(host_id, &config.device_id, &channel, payload)
                        .await
                }
                (Some(id), None) => {
                    commands
                        .control_with_id(host_id, &config.device_id, &channel, payload, Some(id))
                        .await
                }
                (id, request_id) => {
                    commands
                        .control_with_identity(
                            host_id,
                            &config.device_id,
                            &channel,
                            payload,
                            id,
                            request_id,
                        )
                        .await
                }
            };
            match submitted {
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
        let managed = self.cloud_session_id()?.is_some();
        let config = self.require_config()?;
        let pending = match self
            .pending_commands()?
            .into_iter()
            .find(|command| command.request_id == request_id)
        {
            Some(pending) => pending,
            None => return Err(CoreError::NotFound("pending command not found".into())),
        };
        if pending.host_id.is_empty()
            || pending.device_id != config.device_id
            || (!managed && pending.access_url != config.access_url)
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
        match self
            .access_client()
            .await?
            .get_request(&pending.request_id)
            .await
        {
            Ok(state) => {
                if state.request_id != pending.request_id
                    || state.host_id != pending.host_id
                    || state.device_id != pending.device_id
                    || state.command_id != pending.command_id
                {
                    return Err(CoreError::Security(
                        "command recovery response identity mismatch".into(),
                    ));
                }
                // Access stores an opaque status copy. A contradictory negative
                // copy cannot override the authenticated Host command journal;
                // query the original command on a fresh channel below.
            }
            // No Access copy at all, or one whose request TTL has passed:
            // the durable Host journal is still authoritative for a command
            // that may have executed. Do not discard recoverable work just
            // because the control-plane row expired.
            Err(error)
                if error.status == Some(404)
                    || error.code == Some(whalerein_proto::ErrorCode::Expired) => {}
            Err(error) => return self.settle(Err(error.into())).await,
        }
        let result = self
            .recover_command(&pending.host_id, &pending.request_id, &pending.command_id)
            .await;
        self.settle(result).await
    }

    /// An authorization check or explicit revocation awaits reconciliation.
    pub fn authorization_verification_pending(&self) -> CoreResult<bool> {
        Ok(revoked_commands::marker(&self.options.data_dir)?
            .is_some_and(|state| state.starts_with("pending:")))
    }

    /// Whether a cleanup conflict requires inspection before pairing again.
    pub fn revocation_cleanup_requires_inspection(&self) -> CoreResult<bool> {
        Ok(revoked_commands::marker(&self.options.data_dir)?
            .as_deref()
            .and_then(revoked_commands::confirmed_phase)
            == Some("conflict"))
    }

    /// Whether an interrupted or failed identity snapshot needs manual review.
    pub fn revoked_identity_retention_unverified(&self) -> CoreResult<bool> {
        #[cfg(unix)]
        let (marker, warning) =
            revoked_commands::marker_with_retention_warning(&self.options.data_dir)?;
        #[cfg(not(unix))]
        let (marker, warning) = (
            revoked_commands::marker(&self.options.data_dir)?,
            self.options
                .data_dir
                .join("revocation-retention-warning")
                .exists(),
        );
        Ok(warning
            || marker.is_some_and(|state| {
                matches!(
                    revoked_commands::confirmed_phase(&state),
                    Some("check" | "waiting" | "lost" | "conflict")
                ) || (state.starts_with("pending:") && self.current_config().is_none())
            }))
    }

    /// List opaque request identities retained after a device grant was revoked.
    pub fn unconfirmed_revoked_commands(&self) -> CoreResult<Vec<String>> {
        match crate::security::device::existing_device_id(
            &self.options.secrets,
            &self.options.data_dir,
        )? {
            Some(device_id) => revoked_commands::list(&self.options.data_dir, &device_id),
            None => Ok(Vec::new()),
        }
    }

    fn uncertain_recovery_error(&self, error: CoreError) -> CoreError {
        self.diagnostics.record_stage(
            error.source_layer(),
            "command_result_recovery",
            "unavailable",
            error.code(),
        );
        match error {
            // Preserve integrity warnings; they do not assert the command failed.
            CoreError::FingerprintMismatch { .. }
            | CoreError::Security(_)
            | CoreError::Revoked
            | CoreError::RevokedRetentionUnverified => error,
            _ => CoreError::UnknownResult,
        }
    }

    /// After a reconnect, recover the terminal command result over a new
    /// channel using the encrypted `GetCommandResult` query.
    async fn recover_command(
        &self,
        host_id: &str,
        request_id: &str,
        command_id: &str,
    ) -> CoreResult<CommandHandle> {
        let expected_session = self.cloud_session_id()?;
        self.diagnostics
            .record_stage("host", "command_result_recovery", "started", "none");
        let config = self.require_config()?;
        let mut channel = self
            .ensure_channel(host_id, true)
            .await
            .map_err(|error| self.uncertain_recovery_error(error))?;
        let commands = self
            .command_service_for_session(expected_session.as_deref())
            .await?;
        for attempt in 0..10u32 {
            match commands
                .get_command_result(host_id, &config.device_id, &channel, command_id)
                .await
            {
                Ok(result) => {
                    return commands.handle_from_command_result(request_id, command_id, &result);
                }
                Err(CoreError::UnknownResult) if attempt < 9 => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error)
                    if attempt < 9
                        && (matches!(error, CoreError::ChannelMismatch)
                            || error.device_authorization_lost()) =>
                {
                    channel = self
                        .ensure_channel_after(host_id, channel.channel_id())
                        .await
                        .map_err(|error| self.uncertain_recovery_error(error))?;
                }
                Err(error) => return Err(self.uncertain_recovery_error(error)),
            }
        }
        Err(CoreError::UnknownResult)
    }

    // -----------------------------------------------------------------
    // Pairing
    // -----------------------------------------------------------------

    /// Run the complete pairing flow. `decide` is called after HELLO with the
    /// Noise-derived fingerprint; rejecting or mismatching never sends msg3.
    #[cfg(test)]
    pub(crate) async fn pair<F>(&self, uri: &str, decide: F) -> CoreResult<PairingOutcome>
    where
        F: FnOnce(&PairingPrompt) -> PairingDecision,
    {
        self.connect_standalone(uri, decide).await
    }

    /// Connect from a Standalone invitation and select the activated Host.
    pub async fn connect_standalone<F>(&self, uri: &str, decide: F) -> CoreResult<PairingOutcome>
    where
        F: FnOnce(&PairingPrompt) -> PairingDecision,
    {
        self.connect_standalone_as(uri, decide, "").await
    }

    /// Pair and carry `display_name` inside the Noise HELLO. The name is a
    /// label. An empty name stays empty after sanitizing.
    pub async fn connect_standalone_as<F>(
        &self,
        uri: &str,
        decide: F,
        display_name: &str,
    ) -> CoreResult<PairingOutcome>
    where
        F: FnOnce(&PairingPrompt) -> PairingDecision,
    {
        if self
            .instance
            .as_ref()
            .is_some_and(|instance| instance.configuration.service != ClientService::Standalone)
        {
            return Err(CoreError::InvalidInput(
                "invitations require the standalone service".into(),
            ));
        }
        let parsed = parse_pairing_uri(uri)?;
        self.ensure_instance_state()?;
        let guard = self.identity_lock()?;
        self.refresh_regional_state()?;
        let identity = self.device_identity_locked(&guard)?;
        if self.is_paired() {
            let config = self.require_config()?;
            load_or_create_cache_key(&self.options.secrets, &config.device_id)?;
            match self.hosts().await {
                Ok(hosts)
                    if parsed.access_url.as_str() == config.access_url
                        && (parsed.host_id != config.host_id
                            || !hosts.iter().any(|host| host.host_id == parsed.host_id)) => {}
                Ok(_) => return Err(CoreError::Conflict("client is already paired".into())),
                Err(CoreError::Revoked | CoreError::RevokedRetentionUnverified) => {
                    self.runtime.accept_pairing()
                }
                Err(CoreError::NotPaired) => {
                    let config = self.require_config()?;
                    if self.load_pending_pairing(&config.device_id)?.is_some() {
                        return Err(CoreError::NotPaired);
                    }
                    self.clear_local_pairing(&config.device_id)?;
                }
                Err(error) => return Err(error),
            }
        }
        let device_id = identity.device_id.clone();
        if let Some(pending) = self.load_pending_pairing(&device_id)? {
            if pending.config.access_url != parsed.access_url.as_str()
                || pending.config.host_id != parsed.host_id
            {
                return Err(CoreError::Conflict(
                    "a pairing is pending; resume it before pairing elsewhere".into(),
                ));
            }
            if pending.finish.token != parsed.token {
                match self.resume_pending_pairing(pending, &identity).await {
                    Err(error) if error.pairing_closed() => {}
                    Err(error) => return Err(error),
                    Ok(_) => {
                        return Err(CoreError::Conflict(
                            "previous pairing recovered; retry the new invitation".into(),
                        ));
                    }
                }
            } else {
                let fingerprint = pending
                    .config
                    .host_fingerprint
                    .clone()
                    .ok_or(CoreError::NotPaired)?;
                let decision = decide(&PairingPrompt {
                    host_id: parsed.host_id.clone(),
                    access_url: parsed.access_url.to_string(),
                    fingerprint: fingerprint.clone(),
                    fingerprint_display: crate::util::format_fingerprint(&fingerprint),
                });
                if !decision.approve {
                    return Err(CoreError::PairingCancelled);
                }
                if let Some(expected) = decision.expected_fingerprint {
                    if crate::util::normalize_fingerprint(&expected) != fingerprint {
                        return Err(CoreError::FingerprintMismatch {
                            expected,
                            actual: fingerprint,
                        });
                    }
                }
                return self.resume_pending_pairing(pending, &identity).await;
            }
        }
        let pairing_guard =
            crate::security::device::ProfileLock::acquire_admission_waiting(&self.options.data_dir)
                .map_err(pairing_recovery::pairing_admission_error)?;
        let pairing_marker = revoked_commands::marker_in(pairing_guard.directory())?;
        let pairing_epoch = revoked_commands::ensure_pairing_epoch_in(pairing_guard.directory())?;
        drop(pairing_guard);
        if let Some(marker) = pairing_marker.as_deref() {
            if !matches!(
                revoked_commands::confirmed_phase(marker),
                Some("saved" | "lost")
            ) || self.config_store().load()?.is_some()
            {
                return Err(CoreError::Revoked);
            }
        }
        let (mut session, hello) = PairingInitiator::start_named(
            &parsed,
            &device_id,
            &identity.key,
            default_capabilities(),
            display_name,
        )?;

        let ca = self.ca_pem();
        let roots = self.platform_roots();
        let access = AccessClient::with_trust(
            parsed.access_url.clone(),
            None,
            ca.as_deref(),
            roots.as_deref(),
            self.diagnostics.clone(),
        )?;
        let hello_response = access
            .pair_claim(&hello)
            .await
            .map_err(CoreError::from_pairing_hello_access)?;
        let confirmation = session.accept_hello(&hello_response)?;
        if let Some(current) = self.current_config() {
            let existing = if current.host_id == confirmation.host_id {
                current.host_fingerprint.as_ref()
            } else {
                current.host_fingerprints.get(&confirmation.host_id)
            };
            if let Some(existing) = existing {
                if existing != &confirmation.fingerprint {
                    session.cancel();
                    return Err(CoreError::Security("Host fingerprint changed".into()));
                }
            }
        }
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
        let mut persisted = PersistedConfig::new(
            parsed.access_url.to_string(),
            self.mqtt_override().or(parsed.mqtt_url.clone()),
            parsed.host_id.clone(),
            device_id.clone(),
        );
        persisted.host_fingerprint = Some(confirmation.fingerprint.clone());
        let pending = pairing_recovery::PendingPairing {
            config: persisted.clone(),
            finish: finish.clone(),
            device_public_key: identity.key.public_key().to_vec(),
            admission_marker: pairing_marker.clone(),
            admission_epoch: pairing_epoch.clone(),
        };
        self.save_pending_pairing(&pending, pairing_marker.as_deref())?;
        let finish_response = match access.pair_claim(&finish).await {
            Ok(reply) => reply,
            Err(error) => {
                let error = CoreError::from_pairing_access(error);
                self.settle_pairing_terminal(&pending, &error)?;
                return Err(error);
            }
        };
        let completion = session.complete(&finish_response)?;
        #[cfg(feature = "protocol-testing")]
        if let Some(path) = std::env::var_os("WHALEREIN_TEST_PAIR_BEFORE_CONFIG_BARRIER") {
            let path = std::path::PathBuf::from(path);
            std::fs::write(&path, b"paused")
                .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            while path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        self.finish_pairing_config(
            persisted,
            &completion.device_token,
            pairing_marker.as_deref(),
            pairing_epoch.as_deref(),
        )?;
        let outcome = PairingOutcome {
            operation_id: Some(finish.claim_id),
            host_id: parsed.host_id,
            device_id,
            fingerprint: completion.host_fingerprint,
            access_url: parsed.access_url.to_string(),
        };
        self.complete_standalone_pairing(&outcome)?;
        Ok(outcome)
    }

    // -----------------------------------------------------------------
    // Snapshot API
    // -----------------------------------------------------------------

    pub async fn hosts(&self) -> CoreResult<Vec<HostInfo>> {
        let access = self.access_client().await?;
        let prior = revoked_commands::marker(&self.options.data_dir)?;
        let listed = access.list_hosts().await.map_err(CoreError::from);
        if listed.is_ok()
            && prior
                .as_ref()
                .is_some_and(|v| v.starts_with("pending:observe:"))
        {
            let _admission = crate::security::device::ProfileLock::acquire_admission_async(
                &self.options.data_dir,
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .await
            .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
            self.verify_access_generation(&access)?;
            if let Some(id) = prior.as_deref() {
                revoked_commands::clear_owned_pending_in(_admission.directory(), id)?;
            }
        }
        let hosts: Vec<HostInfo> = self
            .settle(listed)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();
        let sync = self.sync_service();
        #[cfg(all(unix, feature = "protocol-testing"))]
        if let Some(path) = std::env::var_os("WHALEREIN_TEST_HOST_SYNC_BARRIER") {
            let path = std::path::PathBuf::from(path);
            std::fs::write(&path, b"paused")
                .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            while path.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(unix)]
        let _admission = {
            let guard = crate::security::device::ProfileLock::acquire_admission_shared_waiting(
                &self.options.data_dir,
            )
            .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
            if let Some(error) = Self::admission_revocation_error(
                revoked_commands::marker_in(guard.directory())?.as_deref(),
                revoked_commands::retention_warning_in(guard.directory()),
            ) {
                return Err(error);
            }
            guard
        };
        match sync {
            Ok(sync) => sync.store_hosts(&hosts)?,
            Err(error @ (CoreError::Revoked | CoreError::RevokedRetentionUnverified)) => {
                return Err(error);
            }
            Err(_) => {}
        }
        Ok(hosts)
    }

    /// Alias kept for the architecture's `listHosts()` shape.
    #[cfg(test)]
    pub async fn list_hosts(&self) -> CoreResult<Vec<HostInfo>> {
        self.hosts().await
    }

    #[cfg(test)]
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

    /// The authoritative device connection for one Host, if it was opened.
    ///
    /// A Host that was never connected has no snapshot; callers must not read
    /// "absent" as offline.
    pub fn connection_snapshot(&self, host_id: &str) -> Option<ConnectionSnapshot> {
        self.connections.snapshot(host_id)
    }

    /// Open (or reuse) the Host connection and wait for its first usable state.
    ///
    /// This is the app-level probe: it performs a real broker handshake and
    /// subscription, so a success means the Host is reachable now, not that it
    /// was reachable when the app started.
    pub async fn connect_host(&self, host_id: &str) -> CoreResult<ConnectionSnapshot> {
        let connection = self.host_connection(host_id).await?;
        let snapshot = connection.snapshot();
        if snapshot.state == ConnectionState::Connected {
            return Ok(snapshot);
        }
        // Bounded so a caller that probes (the CLI, an app launch) reports what
        // is true now instead of blocking on a retry loop. A slow or absent
        // Host simply keeps its current state; the connection keeps trying.
        match tokio::time::timeout(
            Duration::from_secs(5),
            self.wait_host_transport(host_id, &connection, &Cancellation::new()),
        )
        .await
        {
            Ok(Ok(())) => Ok(connection.snapshot()),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(connection.snapshot()),
        }
    }

    /// Open the connection without waiting for it, for a background monitor.
    /// Test-only: exercise the production MQTT query lane.
    #[cfg(test)]
    pub(crate) fn use_mqtt_queries(&self) {
        self.legacy_http_queries
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) async fn host_connection(
        &self,
        host_id: &str,
    ) -> CoreResult<Arc<crate::connection::HostConnection>> {
        let regional = self.cloud_transport().await?;
        self.host_connection_with(host_id, regional).await
    }

    /// Open the connection with the already-resolved regional authority.
    ///
    /// The observation path resolves the region itself (it also watches it for
    /// changes), so it must not resolve it twice.
    pub(crate) async fn host_connection_with(
        &self,
        host_id: &str,
        regional: Option<crate::account::native::RegionalSession>,
    ) -> CoreResult<Arc<crate::connection::HostConnection>> {
        if let Some(existing) = self.connections.running(host_id) {
            return Ok(existing);
        }
        let input = self.connection_input(host_id, regional).await?;
        Ok(self.connections.connect(input))
    }

    /// Everything a connection needs, gathered from the current authority.
    async fn connection_input(
        &self,
        host_id: &str,
        regional: Option<crate::account::native::RegionalSession>,
    ) -> CoreResult<crate::connection::ConnectionInput> {
        let config = self.require_config()?;
        let token = match &regional {
            Some(session) => session.token.clone(),
            None => load_device_token(&self.options.secrets, &config.device_id)?
                .ok_or(CoreError::NotPaired)?,
        };
        Ok(crate::connection::ConnectionInput {
            host_id: host_id.to_owned(),
            device_id: config.device_id.clone(),
            settings: self.mqtt_settings(&config)?,
            token,
            cloud_session: regional.map(|session| session.id),
            runtime: self.runtime.clone(),
            diagnostics: self.diagnostics.clone(),
            queue_bytes: self.transport_budget().reserved_bytes as usize,
        })
    }

    /// Close the Host connection. Sessions attached to it are interrupted.
    pub fn disconnect_host(&self, host_id: &str) {
        self.connections.disconnect(host_id);
    }

    /// App-wide stream of connection transitions, independent of any session.
    pub fn subscribe_connections(&self) -> tokio::sync::broadcast::Receiver<ConnectionSnapshot> {
        self.connections.subscribe()
    }

    /// Confirm that this profile currently authorizes one Host identity.
    ///
    /// Discovery must never imply that an unknown or unpinned Host can be used:
    /// an unqualified Host argument is a lookup, not a reason to re-pair.
    fn ensure_authorized_host(&self, host_id: &str) -> CoreResult<()> {
        let config = self.require_config()?;
        if host_id == config.host_id || config.host_fingerprints.contains_key(host_id) {
            Ok(())
        } else {
            Err(CoreError::NotFound("Host is not authorized".into()))
        }
    }

    pub async fn sessions(&self, host_id: &str) -> CoreResult<SessionList> {
        self.ensure_authorized_host(host_id)?;
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
                let mut sessions: Vec<SessionSummary> =
                    list.sessions.into_iter().map(Into::into).collect();
                crate::cache::sort_sessions(&mut sessions);
                // A complete remote listing replaces the cached one, so
                // deleted/archived sessions cannot survive offline.
                self.cache()?
                    .replace_sessions(host_id, &sessions, "remote", true)?;
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
    #[cfg(test)]
    pub async fn list_sessions(&self, host_id: &str) -> CoreResult<SessionList> {
        self.sessions(host_id).await
    }

    pub async fn get_session(&self, key: &SessionKey) -> CoreResult<SessionSummary> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let cache = self.cache()?;
        if let Some(session) = cache.get_session(key)? {
            return Ok(session);
        }
        let response = self
            .query(
                host,
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
        sync.store_sessions(host, std::slice::from_ref(&summary), "remote", true)?;
        Ok(summary)
    }

    /// Read optional host operations. Older peers safely report no operations.
    pub async fn host_capabilities(&self, host_id: &str) -> CoreResult<HostCapabilities> {
        let result = self
            .query(
                host_id,
                RequestPayload {
                    payload: Some(
                        whalerein_proto::request_payload::Payload::GetHostCapabilities(
                            whalerein_proto::GetHostCapabilities {},
                        ),
                    ),
                },
            )
            .await;
        match result {
            Ok(response) => match response.payload {
                Some(whalerein_proto::response_payload::Payload::HostCapabilities(caps)) => {
                    Ok(caps.into())
                }
                None => Ok(HostCapabilities {
                    model_catalog: false,
                    model_read: false,
                }),
                _ => Err(CoreError::Internal(
                    "unexpected host capabilities response".into(),
                )),
            },
            Err(error) if matches!(error.code(), "unsupported_capability" | "invalid_argument") => {
                Ok(HostCapabilities {
                    model_catalog: false,
                    model_read: false,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Fetch current source catalog. Offline values are never served from cache.
    pub async fn list_models(&self, host_id: &str) -> CoreResult<ModelCatalog> {
        if !self.host_capabilities(host_id).await?.model_catalog {
            return Err(CoreError::Unsupported);
        }
        let response = self
            .query(
                host_id,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ListModels(
                        whalerein_proto::ListModels {},
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::ModelCatalog(catalog)) => {
                Ok(catalog.into())
            }
            _ => Err(CoreError::Internal(
                "unexpected model catalog response".into(),
            )),
        }
    }

    /// Fetch a passive source observation without activating the session.
    pub async fn read_session_model(&self, key: &SessionKey) -> CoreResult<SessionModel> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        if !self.host_capabilities(host).await?.model_read {
            return Err(CoreError::Unsupported);
        }
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ReadSessionModel(
                        whalerein_proto::ReadSessionModel {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::SessionModel(model)) => {
                Ok(model.into())
            }
            _ => Err(CoreError::Internal(
                "unexpected session model response".into(),
            )),
        }
    }

    /// Submit exactly one journaled source selection; no prompt is sent.
    pub async fn select_session_model(
        &self,
        key: &SessionKey,
        provider: &str,
        model: &str,
        effort: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::GetSession(
                        GetSession {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::SessionInfo(session))
                if session
                    .source_capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.model_selection) => {}
            Some(whalerein_proto::response_payload::Payload::SessionInfo(_)) => {
                return Err(CoreError::Unsupported);
            }
            _ => return Err(CoreError::Internal("unexpected session response".into())),
        }
        // Re-selecting the effective next model is a no-op: returning a
        // synthesized receipt here avoids journaling a second indistinguishable
        // pending command. A read failure is not proof of a mismatch, so it
        // falls through to the normal submission path.
        if let Ok(current) = self.read_session_model(key).await {
            let same = current.next.as_ref().is_some_and(|next| {
                next.provider == provider
                    && next.model == model
                    && next.effort.as_deref().unwrap_or("") == effort.unwrap_or("")
            });
            if same {
                return Ok(CommandHandle {
                    // No command identity is invented for a no-op.
                    request_id: String::new(),
                    command_id: String::new(),
                    status: OnlineStatus::Succeeded,
                    message_id: None,
                    delivery_id: None,
                    model_selection: Some(ModelSelectionReceipt {
                        selected: Some(ModelChoice {
                            provider: provider.to_owned(),
                            model: model.to_owned(),
                            effort: effort.map(str::to_owned),
                        }),
                        effect_scope: "session_and_source_default_attempt".into(),
                        default_persistence: "unconfirmed".into(),
                    }),
                    lifecycle_receipt: None,
                    question_receipt: None,
                });
            }
        }
        let handle = self
            .control(
                host,
                RequestPayload {
                    payload: Some(
                        whalerein_proto::request_payload::Payload::SelectSessionModel(
                            whalerein_proto::SelectSessionModel {
                                session_id: session_id.to_owned(),
                                provider: provider.to_owned(),
                                model: model.to_owned(),
                                effort: effort.map(str::to_owned),
                            },
                        ),
                    ),
                },
            )
            .await?;
        if handle.status != OnlineStatus::HostAccepted {
            return Ok(handle);
        }
        self.await_command_terminal(host, handle).await
    }

    // -----------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------

    /// Query capabilities before submitting any source-owned Host operation.
    pub async fn session_create_supported(&self, host_id: &str) -> CoreResult<bool> {
        let response = self
            .query(
                host_id,
                RequestPayload {
                    payload: Some(
                        whalerein_proto::request_payload::Payload::GetHostCapabilities(
                            whalerein_proto::GetHostCapabilities {},
                        ),
                    ),
                },
            )
            .await;
        match response {
            Ok(value) => match value.payload {
                Some(whalerein_proto::response_payload::Payload::HostCapabilities(caps)) => {
                    Ok(caps.session_create)
                }
                None => Ok(false),
                _ => Err(CoreError::Internal(
                    "unexpected host capabilities response".into(),
                )),
            },
            Err(error) if matches!(error.code(), "unsupported_capability" | "invalid_argument") => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Reject create values the Host would reject in preflight.
    ///
    /// These rules describe the request, not Host state: blank option values,
    /// a relative working directory, and a working directory plus workspace
    /// together can never succeed, so the client fails before submitting a
    /// lifecycle command that could otherwise create an unclear journal entry.
    fn validate_create_input(input: &CreateSessionInput) -> CoreResult<()> {
        for (name, value) in [
            ("cwd", &input.cwd),
            ("workspace", &input.workspace_id),
            ("preset", &input.agent_preset),
            ("session id", &input.preferred_session_id),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(CoreError::InvalidInput(format!(
                        "create {name} must not be blank"
                    )));
                }
            }
        }
        if input.cwd.is_some() && input.workspace_id.is_some() {
            return Err(CoreError::InvalidInput(
                "create cwd and workspace are mutually exclusive".into(),
            ));
        }
        if let Some(cwd) = &input.cwd {
            if !is_absolute_host_path(cwd) {
                return Err(CoreError::InvalidInput(
                    "create cwd must be an absolute Host path".into(),
                ));
            }
        }
        Ok(())
    }

    pub async fn create_session(
        &self,
        host_id: &str,
        input: CreateSessionInput,
    ) -> CoreResult<CommandHandle> {
        Self::validate_create_input(&input)?;
        if !self.session_create_supported(host_id).await? {
            return Err(CoreError::Unsupported);
        }
        let handle = self
            .control(
                host_id,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::CreateSession(
                        whalerein_proto::CreateSession {
                            cwd: input.cwd,
                            workspace_id: input.workspace_id,
                            agent_preset: input.agent_preset,
                            preferred_session_id: input.preferred_session_id,
                        },
                    )),
                },
            )
            .await?;
        let handle = if handle.status == OnlineStatus::HostAccepted {
            self.await_command_terminal(host_id, handle).await?
        } else {
            handle
        };
        if handle.status == OnlineStatus::Succeeded && handle.lifecycle_receipt.is_none() {
            return Err(CoreError::UnknownResult);
        }
        Ok(handle)
    }

    /// Fork the latest complete source prefix without starting the child.
    ///
    /// A confirmed child is only described completely once its working
    /// directory is known. Callers that show the child location should use
    /// [`Core::fork_session_outcome`] so an unreadable directory is reported
    /// instead of silently omitted.
    #[cfg(test)]
    pub async fn fork_session(&self, key: &SessionKey) -> CoreResult<CommandHandle> {
        Ok(self.fork_session_outcome(key).await?.handle)
    }

    /// Fork and resolve the confirmed child's working directory.
    ///
    /// Fork creates a child that shares the parent's working directory; the
    /// child summary is a second, best-effort read. Its failure cannot undo the
    /// fork, so it is returned as an explicit error code next to the handle
    /// rather than dropped.
    pub async fn fork_session_outcome(&self, key: &SessionKey) -> CoreResult<ForkOutcome> {
        let handle = self.fork_session_receipt(key).await?;
        let created = handle.lifecycle_receipt.as_ref().is_some_and(|receipt| {
            !receipt.session_id.is_empty()
                && matches!(receipt.disposition.as_str(), "READY" | "READY_WITH_WARNING")
        });
        let mut outcome = ForkOutcome {
            handle,
            working_directory: None,
            working_directory_error: None,
        };
        if !created {
            return Ok(outcome);
        }
        let child = outcome
            .handle
            .lifecycle_receipt
            .as_ref()
            .map(|receipt| receipt.session_id.clone())
            .unwrap_or_default();
        let child_key = SessionKey {
            host_id: key.host_id.clone(),
            session_id: child,
        };
        match self.get_session(&child_key).await {
            Ok(summary) if !summary.cwd.is_empty() => outcome.working_directory = Some(summary.cwd),
            Ok(_) => outcome.working_directory_error = Some("not_available".into()),
            Err(error) => outcome.working_directory_error = Some(error.code().to_owned()),
        }
        Ok(outcome)
    }

    async fn fork_session_receipt(&self, key: &SessionKey) -> CoreResult<CommandHandle> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(
                        whalerein_proto::request_payload::Payload::GetHostCapabilities(
                            whalerein_proto::GetHostCapabilities {},
                        ),
                    ),
                },
            )
            .await;
        match response {
            Ok(response) => match response.payload {
                Some(whalerein_proto::response_payload::Payload::HostCapabilities(c))
                    if c.session_fork => {}
                Some(whalerein_proto::response_payload::Payload::HostCapabilities(_)) => {
                    return Err(CoreError::Unsupported);
                }
                _ => {
                    return Err(CoreError::Internal(
                        "unexpected host capabilities response".into(),
                    ));
                }
            },
            Err(error) if matches!(error.code(), "unsupported_capability" | "invalid_argument") => {
                return Err(CoreError::Unsupported);
            }
            Err(error) => return Err(error),
        }
        let handle = self
            .control(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ForkSession(
                        whalerein_proto::ForkSession {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        let handle = if handle.status == OnlineStatus::HostAccepted {
            self.await_command_terminal(host, handle).await?
        } else {
            handle
        };
        if handle.status == OnlineStatus::Succeeded
            && !handle.lifecycle_receipt.as_ref().is_some_and(|r| {
                r.operation == "fork"
                    && (r.parent_session_id.as_deref() == Some(session_id)
                        || (matches!(r.disposition.as_str(), "FAILED" | "UNKNOWN")
                            && r.reconcile_key.as_deref() == Some(session_id)))
            })
        {
            return Err(CoreError::UnknownResult);
        }
        Ok(handle)
    }

    /// Observe the source queue without caching its contents.
    pub async fn pending_queue(&self, key: &SessionKey) -> CoreResult<PendingQueue> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ReadPendingQueue(
                        whalerein_proto::ReadPendingQueue {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::PendingQueue(queue))
                if queue.session_id == session_id =>
            {
                Ok(queue.into())
            }
            _ => Err(CoreError::Internal("invalid pending queue response".into())),
        }
    }

    /// Apply one source-owned queued item action. A separate read refreshes the view.
    pub async fn update_pending_queue(
        &self,
        key: &SessionKey,
        item_id: &str,
        action: PendingQueueAction,
        text: &str,
    ) -> CoreResult<CommandHandle> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let handle = self
            .control(
                host,
                RequestPayload {
                    payload: Some(
                        whalerein_proto::request_payload::Payload::UpdatePendingQueue(
                            whalerein_proto::UpdatePendingQueue {
                                session_id: session_id.to_owned(),
                                item_id: item_id.to_owned(),
                                action: whalerein_proto::QueueAction::from(action) as i32,
                                text: text.to_owned(),
                            },
                        ),
                    ),
                },
            )
            .await?;
        if handle.status != OnlineStatus::HostAccepted {
            return Ok(handle);
        }
        self.await_command_terminal(host, handle).await
    }

    #[cfg(test)]
    pub async fn send_message(&self, key: &SessionKey, text: &str) -> CoreResult<CommandHandle> {
        self.send_message_with_delivery(key, text, DeliveryMode::EnqueueAfterRun)
            .await
    }

    #[cfg(test)]
    pub async fn send_message_with_delivery(
        &self,
        key: &SessionKey,
        text: &str,
        mode: DeliveryMode,
    ) -> CoreResult<CommandHandle> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        // A non-default delivery mode is a source capability. Reject one the
        // Host does not advertise before any command is submitted; an empty
        // advertisement is a legacy Host and keeps the old pass-through path.
        if matches!(mode, DeliveryMode::StartIfIdle | DeliveryMode::Steer) {
            // Read the Source's current advertisement rather than the cached
            // session summary, which may predate this Host build.
            let list = self.sessions(host).await?;
            if let Some(capabilities) = list
                .sessions
                .iter()
                .find(|session| session.session_id == session_id)
                .and_then(|session| session.source_capabilities.as_ref())
            {
                if !capabilities.delivery_modes.is_empty()
                    && !capabilities
                        .delivery_modes
                        .iter()
                        .any(|advertised| advertised == mode.as_str_name())
                {
                    return Err(CoreError::Unsupported);
                }
            }
        }
        let cache = self.cache()?;
        let digest = cache.send_intent_digest(text);
        // A blind resend after an unknown observation is recovered instead of
        // being submitted as a second user message. Once a send resolves, the
        // intent is removed and identical text is a new message again.
        if let Some(retained) = cache.unresolved_send_intent(host, session_id, &digest)? {
            match self.recover_pending_command(&retained).await {
                Ok(handle) => {
                    if handle.status == OnlineStatus::HostAccepted
                        || handle.status == OnlineStatus::Received
                    {
                        let request_id = handle.request_id.clone();
                        let command_id = handle.command_id.clone();
                        return self
                            .observe_send_terminal(
                                host, session_id, &digest, request_id, command_id, handle,
                            )
                            .await;
                    }
                    cache.resolve_send_intent(host, session_id, &digest)?;
                    return Ok(handle);
                }
                Err(CoreError::NotFound(_)) => {
                    // The original command is gone; a fresh send cannot
                    // duplicate a message the Host no longer has.
                    cache.resolve_send_intent(host, session_id, &digest)?;
                }
                Err(error) => return Err(error),
            }
        }
        let handle = self
            .control(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                        SendMessage {
                            session_id: session_id.to_owned(),
                            text: text.to_owned(),
                            delivery_mode: whalerein_proto::DeliveryMode::from(mode) as i32,
                        },
                    )),
                },
            )
            .await?;
        if handle.status != OnlineStatus::HostAccepted {
            cache.resolve_send_intent(host, session_id, &digest)?;
            return Ok(handle);
        }
        let request_id = handle.request_id.clone();
        let command_id = handle.command_id.clone();
        self.observe_send_terminal(host, session_id, &digest, request_id, command_id, handle)
            .await
    }

    /// Follow an accepted send to a terminal receipt. An unknown observation
    /// keeps the command identity (and an idempotency record) instead of
    /// inviting a resend that would duplicate the user message.
    #[cfg(test)]
    async fn observe_send_terminal(
        &self,
        host_id: &str,
        session_id: &str,
        digest: &str,
        request_id: String,
        command_id: String,
        handle: CommandHandle,
    ) -> CoreResult<CommandHandle> {
        let result = self.await_command_terminal(host_id, handle).await;
        let cache = self.cache()?;
        match result {
            Ok(handle) => {
                cache.resolve_send_intent(host_id, session_id, digest)?;
                Ok(handle)
            }
            Err(CoreError::Timeout) => {
                // The accepted command may still deliver. Report unknown and
                // retain its identity so the retry recovers it.
                let _ =
                    cache.record_send_intent(host_id, session_id, digest, &request_id, &command_id);
                Err(CoreError::UnknownResult)
            }
            Err(CoreError::UnknownResult) => {
                let _ =
                    cache.record_send_intent(host_id, session_id, digest, &request_id, &command_id);
                Err(CoreError::UnknownResult)
            }
            Err(error) => {
                // A definitive rejection cannot create a message.
                let _ = cache.resolve_send_intent(host_id, session_id, digest);
                Err(error)
            }
        }
    }

    /// Convert `host_accepted` into a confirmed terminal result by asking the
    /// host for the command result over the current channel.
    async fn await_command_terminal(
        &self,
        host_id: &str,
        handle: CommandHandle,
    ) -> CoreResult<CommandHandle> {
        let expected_session = self.cloud_session_id()?;
        let config = self.require_config()?;
        let channel = self
            .ensure_channel(host_id, false)
            .await
            .map_err(|error| self.uncertain_recovery_error(error))?;
        let commands = self
            .command_service_for_session(expected_session.as_deref())
            .await?;
        // Renewal of the original request R lives in commands, together with
        // the result polling it protects.
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

    /// Resolve the authorized Host that owns an approval identity.
    ///
    /// An explicit `--host` always wins; a single authorized Host is
    /// unambiguous; several authorized Hosts produce a bounded, actionable
    /// conflict instead of guessing. The approval identity is Host-scoped and
    /// is never probed across Hosts.
    #[cfg(test)]
    pub async fn approval_host(&self, explicit_host: Option<&str>) -> CoreResult<String> {
        if let Some(host) = explicit_host {
            return self.live_authorized_host(host).await;
        }
        let hosts = self.hosts().await?;
        self.approval_host_from(&hosts)
    }

    #[cfg(test)]
    fn approval_host_from(&self, hosts: &[HostInfo]) -> CoreResult<String> {
        match hosts {
            [host] => Ok(host.host_id.clone()),
            [] => Err(CoreError::NotFound("no authorized Host".into())),
            _ => Err(CoreError::Conflict(format!(
                "approval is Host-scoped and multiple Hosts are authorized ({}); specify a Host with --host",
                bounded_approval_host_candidates(hosts)
            ))),
        }
    }

    /// Read one approval's live or terminal state. The read path is the same
    /// Host-scoped identity used by approve/reject; it never guesses between
    /// several authorized Hosts.
    pub async fn get_approval(&self, host_id: &str, approval_id: &str) -> CoreResult<ApprovalInfo> {
        if approval_id.is_empty() {
            return Err(CoreError::InvalidInput("approval identity missing".into()));
        }
        self.ensure_authorized_host(host_id)?;
        let host = host_id;
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::GetApproval(
                        GetApproval {
                            approval_id: approval_id.to_owned(),
                        },
                    )),
                },
            )
            .await
            .map_err(|error| match error {
                CoreError::Host {
                    code: Some(whalerein_proto::ErrorCode::NotFound),
                    ..
                } => CoreError::NotFound("approval not found".into()),
                other => other,
            })?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::Approval(approval))
                if approval.approval_id == approval_id =>
            {
                Ok(ApprovalInfo::from(approval))
            }
            Some(whalerein_proto::response_payload::Payload::Approval(_)) => Err(
                CoreError::Internal("approval response identity mismatch".into()),
            ),
            _ => Err(CoreError::Internal("unexpected approval response".into())),
        }
    }

    /// List the host's pending approvals, optionally scoped to one session.
    /// An explicit `--host` scope wins; otherwise the session identifies the
    /// Host (and stays ambiguous when the session exists on several Hosts).
    /// This is the read half of the approval path; approve/reject remain
    /// encrypted control commands.
    pub async fn list_approvals(
        &self,
        host_id: &str,
        session_id: &str,
    ) -> CoreResult<Vec<ApprovalInfo>> {
        self.ensure_authorized_host(host_id)?;
        let host = host_id;
        let response = self
            .query(
                host,
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

    pub async fn list_questions(
        &self,
        key: &SessionKey,
        cursor: &str,
    ) -> CoreResult<QuestionPageInfo> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ListQuestions(
                        whalerein_proto::ListQuestions {
                            session_id: session_id.to_owned(),
                            cursor: cursor.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::QuestionPage(page)) => {
                if page
                    .batches
                    .iter()
                    .any(|batch| batch.session_id != session_id)
                {
                    return Err(CoreError::Internal(
                        "question response scope mismatch".into(),
                    ));
                }
                let encoded_bytes = prost::Message::encoded_len(&page);
                Ok(QuestionPageInfo {
                    batches: page.batches.into_iter().map(Into::into).collect(),
                    next_cursor: page.next_cursor,
                    stamp: page.stamp.map(Into::into),
                    complete: page.complete,
                    encoded_bytes,
                })
            }
            _ => Err(CoreError::Internal(
                "unexpected question list response".into(),
            )),
        }
    }

    /// Read one authoritative question page. One-shot callers share this
    /// reader with automatic observation; neither acquires a native claim.
    pub async fn read_questions(
        &self,
        key: &SessionKey,
        cursor: &str,
    ) -> CoreResult<QuestionPageInfo> {
        self.list_questions(key, cursor).await
    }

    /// Read one authoritative question batch through the shared reader.
    pub async fn read_question(
        &self,
        key: &SessionKey,
        batch_id: &str,
    ) -> CoreResult<QuestionBatchInfo> {
        self.get_question(key, batch_id).await
    }

    pub async fn get_question(
        &self,
        key: &SessionKey,
        batch_id: &str,
    ) -> CoreResult<QuestionBatchInfo> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::GetQuestion(
                        whalerein_proto::GetQuestion {
                            session_id: session_id.to_owned(),
                            batch_id: batch_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::QuestionBatch(batch))
                if batch.session_id == session_id && batch.batch_id == batch_id =>
            {
                Ok(batch.into())
            }
            _ => Err(CoreError::Internal("unexpected question response".into())),
        }
    }

    pub async fn answer_question(
        &self,
        key: &SessionKey,
        batch: &QuestionBatchInfo,
        answers: Vec<QuestionAnswer>,
    ) -> CoreResult<CommandHandle> {
        self.answer_question_with_id(key, batch, answers, None)
            .await
    }

    /// A caller-supplied command ID allows a different client to explicitly
    /// cancel the accepted open answer without exposing its content.
    pub async fn answer_question_with_id(
        &self,
        key: &SessionKey,
        batch: &QuestionBatchInfo,
        answers: Vec<QuestionAnswer>,
        command_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        self.authorized_session(key)?;
        if batch.session_id != key.session_id {
            return Err(CoreError::Conflict(
                "question batch belongs to another session".into(),
            ));
        }
        let phase = batch
            .phase_value()
            .filter(|phase| phase.is_answerable())
            .ok_or_else(|| CoreError::InvalidInput("question phase is not answerable".into()))?;
        if !batch.answerable
            || batch.content_unavailable
            || batch.source_epoch.is_empty()
            || batch.digest.len() != 32
        {
            return Err(CoreError::InvalidInput(
                "question guard is unavailable".into(),
            ));
        }
        let host = key.host_id.as_str();
        let handle = self
            .control_with_id(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::AnswerQuestion(
                        whalerein_proto::AnswerQuestion {
                            session_id: batch.session_id.clone(),
                            batch_id: batch.batch_id.clone(),
                            source_epoch: batch.source_epoch.clone(),
                            expected_version: batch.version,
                            expected_digest: batch.digest.clone(),
                            expected_phase: wire_question_phase(phase),
                            answers: answers
                                .into_iter()
                                .map(|answer| whalerein_proto::QuestionAnswerItem {
                                    id: answer.id,
                                    selected: answer.selected,
                                    custom: answer.custom,
                                })
                                .collect(),
                        },
                    )),
                },
                command_id,
            )
            .await?;
        let handle = if handle.status == OnlineStatus::HostAccepted {
            self.await_command_terminal(host, handle).await?
        } else {
            handle
        };
        if handle.status == OnlineStatus::Succeeded {
            let expected_receipt_phase = phase.receipt_phase();
            if !handle.question_receipt.as_ref().is_some_and(|receipt| {
                receipt.batch_id == batch.batch_id && receipt.phase == expected_receipt_phase
            }) {
                return Err(CoreError::UnknownResult);
            }
        }
        Ok(handle)
    }

    /// Requests cancellation of an in-flight open answer owned by this device.
    /// `false` does not establish the answer's outcome: it may have completed
    /// already or not yet reached the Host. Recover its command result instead.
    pub async fn cancel_question_answer(
        &self,
        key: &SessionKey,
        command_id: &str,
    ) -> CoreResult<bool> {
        self.authorized_session(key)?;
        if command_id.is_empty() {
            return Err(CoreError::InvalidInput(
                "session and command are required".into(),
            ));
        }
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let response = self
            .query(
                host,
                RequestPayload {
                    payload: Some(
                        whalerein_proto::request_payload::Payload::CancelQuestionAnswer(
                            whalerein_proto::CancelQuestionAnswer {
                                session_id: session_id.to_owned(),
                                command_id: command_id.to_owned(),
                            },
                        ),
                    ),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::QuestionCancelReceipt(result)) => {
                Ok(result.cancellation_requested)
            }
            _ => Err(CoreError::Internal(
                "unexpected question cancellation response".into(),
            )),
        }
    }

    pub async fn approve(
        &self,
        host_id: &str,
        approval_id: &str,
        expected_digest: &[u8],
        expected_version: i64,
    ) -> CoreResult<CommandHandle> {
        self.resolve_approval(
            host_id,
            approval_id,
            ApprovalDecision::Approve,
            expected_digest,
            expected_version,
        )
        .await
    }

    pub async fn reject(
        &self,
        host_id: &str,
        approval_id: &str,
        expected_digest: &[u8],
        expected_version: i64,
    ) -> CoreResult<CommandHandle> {
        self.resolve_approval(
            host_id,
            approval_id,
            ApprovalDecision::Reject,
            expected_digest,
            expected_version,
        )
        .await
    }

    async fn resolve_approval(
        &self,
        host_id: &str,
        approval_id: &str,
        decision: ApprovalDecision,
        expected_digest: &[u8],
        expected_version: i64,
    ) -> CoreResult<CommandHandle> {
        self.ensure_authorized_host(host_id)?;
        self.control(
            host_id,
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

    pub async fn interrupt(&self, key: &SessionKey) -> CoreResult<CommandHandle> {
        self.authorized_session(key)?;
        let host = key.host_id.as_str();
        let session_id = key.session_id.as_str();
        let handle = self
            .control(
                host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                        Interrupt {
                            session_id: session_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        // Acceptance is not proof that the Agent stopped. Follow the accepted
        // command to its terminal receipt exactly as send does, so a slow Host
        // cannot make the client report success for an unconfirmed stop.
        let handle = if matches!(
            handle.status,
            OnlineStatus::HostAccepted | OnlineStatus::Received
        ) {
            match self.await_command_terminal(host, handle).await {
                Ok(handle) => handle,
                // An accepted stop whose terminal result cannot be read may
                // still have happened; it is unknown, never a success.
                Err(CoreError::Timeout) => return Err(CoreError::UnknownResult),
                Err(error) => return Err(error),
            }
        } else {
            handle
        };
        match handle.status {
            OnlineStatus::Succeeded => Ok(handle),
            // A terminal unknown/failed/rejected interrupt never confirms the
            // stop. Keep the command identity recoverable and report it.
            OnlineStatus::Unknown | OnlineStatus::HostAccepted | OnlineStatus::Received => {
                Err(CoreError::UnknownResult)
            }
            OnlineStatus::Failed | OnlineStatus::Rejected => Err(CoreError::HostRejected),
            OnlineStatus::HostOffline => Err(CoreError::HostOffline),
        }
    }

    /// Revoke grants and stop reconnects while preserving the profile identity.
    /// Clears the token, cache key and pairing configuration. Call
    /// `revoked_identity_retention_unverified` after success to surface a
    /// possibly incomplete unconfirmed-command snapshot.
    ///
    /// A token Access already rejects uses the same terminal path as any other
    /// device-token call and returns `revoked` after the local files are gone.
    pub async fn revoke(&self) -> CoreResult<RevocationOutcome> {
        let config = self.require_config()?;
        let device_id = config.device_id.clone();
        let access = self.access_client().await?;
        let revocation_intent = {
            let _admission = crate::security::device::ProfileLock::acquire_admission_async(
                &self.options.data_dir,
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .await
            .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
            self.verify_access_generation(&access)?;
            #[cfg(any(unix, windows))]
            {
                let deadline = tokio::time::Instant::now()
                    + Duration::from_millis(crate::commands::REQUEST_TTL_MS as u64);
                while !self.try_prepare_revocation_cache(_admission.directory()) {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            match revoked_commands::marker_in(_admission.directory())? {
                Some(id) if id.starts_with("pending:revoke:") => id,
                Some(id) if id.starts_with("pending:observe:") => {
                    return Err(CoreError::Unavailable("authorization verification is pending; run hosts to reconcile before revocation".into()));
                }
                _ => {
                    let id = format!("pending:revoke:{}", crate::util::random_id(16));
                    revoked_commands::write_marker_in(_admission.directory(), &id)?;
                    id
                }
            }
        };
        #[cfg(feature = "protocol-testing")]
        if let Some(path) = std::env::var_os("WHALEREIN_TEST_BEFORE_DELETE_BARRIER") {
            let path = std::path::PathBuf::from(path);
            std::fs::write(&path, b"paused")
                .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            while path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        match access.delete_device(&device_id).await {
            Ok(()) => {
                #[cfg(feature = "protocol-testing")]
                if let Some(path) = std::env::var_os("WHALEREIN_TEST_DELETE_CONFIRMED_BARRIER") {
                    let path = std::path::PathBuf::from(path);
                    std::fs::write(&path, b"paused")
                        .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
                    while path.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                self.runtime.revoke();
                // The server grant is already gone and the local credentials
                // were cleared before the cache loop. A concurrent watch may
                // still hold a SQLite read transaction, so a busy/missing
                // cache must downgrade retention to "unverified" instead of
                // failing the whole revocation (which would leave the CLI
                // believing it is still paired).
                if let Err(error) = self
                    .clear_local_pairing_async(&device_id, &revocation_intent)
                    .await
                {
                    let cleanup_only = matches!(
                        error,
                        CoreError::Cache(_)
                            | CoreError::Unavailable(_)
                            | CoreError::RevokedRetentionUnverified
                    ) || (cfg!(windows)
                        && matches!(
                            &error,
                            CoreError::Internal(message)
                                if message == "pairing cleanup incomplete"
                                    || message.starts_with("sensitive credentials were cleared")
                        ));
                    if !cleanup_only {
                        return Err(error);
                    }
                }
                Ok(RevocationOutcome {
                    device_id,
                    retention: if self.revoked_identity_retention_unverified()? {
                        RevocationRetention::Unverified
                    } else {
                        RevocationRetention::Recorded
                    },
                })
            }
            Err(error) => {
                let rejected: CoreResult<()> = self.settle(Err(CoreError::from(error))).await;
                rejected?;
                Ok(RevocationOutcome {
                    device_id,
                    retention: if self.revoked_identity_retention_unverified()? {
                        RevocationRetention::Unverified
                    } else {
                        RevocationRetention::Recorded
                    },
                })
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
        if self.cloud_session_id()?.is_some() {
            // A managed credential rejection does not establish Device removal.
            let session = tokio::time::timeout(Duration::from_secs(3), async {
                self.account_client(false).await?.status().await
            })
            .await;
            if matches!(session, Ok(Err(CoreError::AuthenticationRequired))) {
                return Err(CoreError::AuthenticationRequired);
            }
            return Err(CoreError::Unavailable(
                "Host access could not be confirmed for the current Cloud session".into(),
            ));
        }
        let access = self.access_client().await?;
        let (observed_intent, observed_marker) = {
            let _admission = crate::security::device::ProfileLock::acquire_admission_async(
                &self.options.data_dir,
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .await
            .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
            self.verify_access_generation(&access)?;
            #[cfg(any(unix, windows))]
            {
                let deadline = tokio::time::Instant::now()
                    + Duration::from_millis(crate::commands::REQUEST_TTL_MS as u64);
                while !self.try_prepare_revocation_cache(_admission.directory()) {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            match revoked_commands::marker_in(_admission.directory())? {
                None => {
                    let id = format!("pending:observe:{}", crate::util::random_id(16));
                    revoked_commands::write_marker_in(_admission.directory(), &id)?;
                    (Some(id.clone()), id)
                }
                Some(id) => (None, id),
            }
        };
        self.diagnostics
            .record_stage("access", "authorization_check", "started", error.code());
        match access.list_hosts().await {
            Err(denied) if CoreError::from(denied.clone()).device_authorization_lost() => {
                self.diagnostics.record_stage(
                    "access",
                    "authorization_check",
                    "revoked",
                    error.code(),
                );
                #[cfg(feature = "protocol-testing")]
                if let Some(path) = std::env::var_os("WHALEREIN_TEST_OBSERVED_REVOKED_BARRIER") {
                    let path = std::path::PathBuf::from(path);
                    std::fs::write(&path, b"paused")
                        .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
                    while path.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            Ok(_) => {
                self.diagnostics.record_stage(
                    "access",
                    "authorization_check",
                    "pairing_preserved",
                    "none",
                );
                let _admission = crate::security::device::ProfileLock::acquire_admission_async(
                    &self.options.data_dir,
                    std::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
                self.verify_access_generation(&access)?;
                if let Some(id) = observed_intent {
                    revoked_commands::clear_owned_pending_in(_admission.directory(), &id)?;
                }
                return Err(error);
            }
            Err(check) => {
                self.diagnostics.record_stage(
                    "access",
                    "authorization_check",
                    "pairing_preserved",
                    CoreError::from(check).code(),
                );
                return Err(error);
            }
        }
        if let Some(config) = self.current_config() {
            self.runtime.revoke();
            self.clear_local_pairing_async(&config.device_id, &observed_marker)
                .await?;
        }
        Err(if self.revoked_identity_retention_unverified()? {
            CoreError::RevokedRetentionUnverified
        } else {
            CoreError::Revoked
        })
    }

    fn clear_local_pairing(&self, device_id: &str) -> CoreResult<()> {
        self.clear_local_pairing_inner(device_id, false, None)
    }

    #[cfg(unix)]
    async fn clear_local_pairing_async(
        &self,
        device_id: &str,
        expected_marker: &str,
    ) -> CoreResult<()> {
        self.clear_local_pairing_async_with_timeout(
            device_id,
            expected_marker,
            Duration::from_millis(crate::commands::REQUEST_TTL_MS as u64),
        )
        .await
    }

    #[cfg(unix)]
    async fn clear_local_pairing_async_with_timeout(
        &self,
        device_id: &str,
        expected_marker: &str,
        timeout: Duration,
    ) -> CoreResult<()> {
        let _ = self.device_identity();
        let deadline = std::time::Instant::now() + timeout;
        let guard = crate::security::device::ProfileLock::acquire_admission_async(
            &self.options.data_dir,
            deadline,
        )
        .await
        .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
        if revoked_commands::marker_in(guard.directory())?.as_deref() != Some(expected_marker) {
            return Err(CoreError::Conflict(
                "pairing changed while clearing authorization".into(),
            ));
        }
        match crate::security::device::existing_device_id(
            &self.options.secrets,
            &self.options.data_dir,
        )? {
            Some(id) if id == device_id => {}
            _ => {
                return Err(CoreError::Conflict(
                    "profile identity changed during cleanup".into(),
                ));
            }
        }
        #[cfg(test)]
        let confirmed = if self.fail_waiting_marker.swap(false, Ordering::SeqCst) {
            Err(CoreError::Internal("synthetic marker write failure".into()))
        } else {
            revoked_commands::write_marker_in(guard.directory(), "confirmed-waiting")
        };
        #[cfg(not(test))]
        let confirmed = revoked_commands::write_marker_in(guard.directory(), "confirmed-waiting");
        if self
            .redact_pending_pairing_after_revocation(device_id, guard.directory())
            .is_err()
        {
            let _ = revoked_commands::write_marker_in(guard.directory(), "confirmed-lost");
        }
        let _ = self.clear_local_pairing_secrets(device_id);
        if confirmed.is_err() {
            // Without a durable confirmed marker, do not wait in a state that
            // cannot be recovered after cancellation or process death.
            let cached = self.try_cached_for_revocation().ok().flatten();
            let snapshot = cached
                .as_deref()
                .ok_or(crate::cache::CacheError::Missing)
                .and_then(|cache| {
                    cache.pending_commands_from_existing(guard.directory(), &self.options.data_dir)
                });
            let cleanup = self.clear_local_pairing_prepared(
                device_id,
                false,
                None,
                Some(guard),
                Some(snapshot),
                true,
            );
            cleanup?;
            return confirmed;
        }
        let cached = loop {
            match self.try_cached_for_revocation() {
                Ok(cached) => break cached,
                Err(()) if std::time::Instant::now() >= deadline => break None,
                Err(()) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let deadline = tokio::time::Instant::from_std(deadline);
        let mut guard = Some(guard);
        loop {
            let ready = {
                let directory = guard.as_ref().expect("guard").directory();
                match cached
                    .as_deref()
                    .ok_or(crate::cache::CacheError::Missing)
                    .and_then(|cache| {
                        cache.pending_commands_from_existing(directory, &self.options.data_dir)
                    }) {
                    Err(crate::cache::CacheError::Busy) => None,
                    snapshot => Some(self.clear_local_pairing_prepared(
                        device_id,
                        false,
                        None,
                        guard.take(),
                        Some(snapshot),
                        true,
                    )),
                }
            };
            if let Some(result) = ready {
                return result;
            }
            if tokio::time::Instant::now() >= deadline {
                return self.clear_local_pairing_prepared(
                    device_id,
                    false,
                    None,
                    guard.take(),
                    Some(Err(crate::cache::CacheError::Busy)),
                    true,
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(not(unix))]
    async fn clear_local_pairing_async(
        &self,
        device_id: &str,
        expected_marker: &str,
    ) -> CoreResult<()> {
        let guard = crate::security::device::ProfileLock::acquire_admission_async(
            &self.options.data_dir,
            std::time::Instant::now()
                + Duration::from_millis(crate::commands::REQUEST_TTL_MS as u64),
        )
        .await
        .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?;
        if revoked_commands::marker_in(guard.directory())?.as_deref() != Some(expected_marker) {
            return Err(CoreError::Conflict(
                "pairing changed while clearing authorization".into(),
            ));
        }
        // Match the Unix ordering: durably confirm revocation, attempt identity
        // retention, and erase the FINISH credential before final cleanup. A
        // failed retention attempt remains visible through the sticky lost phase.
        if crate::security::device::existing_device_id(
            &self.options.secrets,
            &self.options.data_dir,
        )?
        .as_deref()
            != Some(device_id)
        {
            return Err(CoreError::Conflict(
                "profile identity changed during cleanup".into(),
            ));
        }
        let confirmed = revoked_commands::write_marker_in(guard.directory(), "confirmed-waiting");
        if self
            .redact_pending_pairing_after_revocation(device_id, guard.directory())
            .is_err()
        {
            let _ = revoked_commands::write_marker_in(guard.directory(), "confirmed-lost");
        }
        let _ = self.clear_local_pairing_secrets(device_id);
        self.clear_local_pairing_prepared(device_id, false, None, Some(guard))?;
        confirmed
    }

    fn clear_local_pairing_inner(
        &self,
        device_id: &str,
        resume: bool,
        expected_marker: Option<&str>,
    ) -> CoreResult<()> {
        self.clear_local_pairing_prepared(
            device_id,
            resume,
            expected_marker,
            None,
            #[cfg(unix)]
            None,
            #[cfg(unix)]
            false,
        )
    }

    fn clear_local_pairing_prepared(
        &self,
        device_id: &str,
        resume: bool,
        expected_marker: Option<&str>,
        prepared: Option<crate::security::device::ProfileLock>,
        #[cfg(unix)] prepared_snapshot: Option<
            Result<crate::cache::LockedPendingSnapshot, crate::cache::CacheError>,
        >,
        #[cfg(unix)] force_busy: bool,
    ) -> CoreResult<()> {
        // An existing identity is already durable; reading its ID does not need
        // to contend with another process performing a pairing operation.
        let identity = match crate::security::device::existing_device_id(
            &self.options.secrets,
            &self.options.data_dir,
        ) {
            Ok(Some(id)) if id == device_id => Ok(()),
            Ok(Some(_)) => Err(CoreError::Security("profile identity mismatch".into())),
            Ok(None) => self.device_identity().and_then(|identity| {
                if identity.device_id == device_id {
                    Ok(())
                } else {
                    Err(CoreError::Security("profile identity mismatch".into()))
                }
            }),
            Err(error) => Err(error.into()),
        };
        // Never create a new cache during revocation: a missing database cannot
        // prove that all pending identities were retained.
        #[cfg(not(unix))]
        let cache = self.cache.lock().expect("cache lock").clone();
        let _admission = match prepared {
            Some(guard) => guard,
            None => crate::security::device::ProfileLock::acquire_admission_waiting(
                &self.options.data_dir,
            )
            .map_err(|_| CoreError::Config("profile admission lock unavailable".into()))?,
        };
        // Forget and recreation hold the same admission lock. Recheck after
        // acquiring it so an old cleanup cannot act on a newer profile.
        match crate::security::device::existing_device_id(
            &self.options.secrets,
            &self.options.data_dir,
        )? {
            Some(id) if id == device_id => {}
            _ => {
                return Err(CoreError::Conflict(
                    "profile identity changed during cleanup".into(),
                ));
            }
        }
        let current_marker = revoked_commands::marker_in(_admission.directory())?;
        if resume && current_marker.is_none() {
            *self.config.write().expect("config lock") = self.config_store().load()?;
            return Ok(());
        }
        if expected_marker.is_some() && current_marker.as_deref() != expected_marker {
            return Err(CoreError::Conflict(
                "pairing changed while clearing authorization".into(),
            ));
        }
        if resume
            && current_marker
                .as_deref()
                .and_then(revoked_commands::confirmed_phase)
                == Some("check")
        {
            revoked_commands::write_marker_in(_admission.directory(), "confirmed-lost")?;
        }
        let fence = if resume
            && current_marker
                .as_deref()
                .and_then(revoked_commands::confirmed_phase)
                == Some("waiting")
        {
            Ok(())
        } else {
            revoked_commands::write_marker_in(_admission.directory(), "confirmed")
        };
        #[cfg(feature = "protocol-testing")]
        if fence.is_ok() {
            if let Some(path) = std::env::var_os("WHALEREIN_TEST_REVOCATION_BARRIER") {
                let path = std::path::PathBuf::from(path);
                std::fs::write(&path, b"paused")
                    .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
                while path.exists() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        // A missing starting marker can mean missing local credentials, not a
        // confirmed global revocation. Preserve pairing uncertainty in that case.
        let revocation_proven = current_marker.as_deref().is_some_and(|marker| {
            marker.starts_with("pending:revoke:")
                || marker.starts_with("pending:observe:")
                || revoked_commands::confirmed_phase(marker).is_some()
        });
        let pending_cleanup = if revocation_proven {
            self.redact_pending_pairing_after_revocation(device_id, _admission.directory())
        } else {
            Ok(())
        };
        if pending_cleanup.is_err() {
            let _ = revoked_commands::write_marker_in(_admission.directory(), "confirmed-lost");
        }
        // Credentials are not needed for the pinned plaintext identity snapshot.
        let _ = self.clear_local_pairing_secrets(device_id);
        #[cfg(unix)]
        let cached = if prepared_snapshot.is_none() {
            Some(
                self.cache
                    .try_lock()
                    .map_err(|_| CoreError::Unavailable("cache state is busy; retry".into()))?
                    .clone(),
            )
        } else {
            None
        };
        #[cfg(unix)]
        let locked_snapshot = match prepared_snapshot {
            Some(snapshot) => snapshot,
            None => cached
                .as_ref()
                .and_then(Option::as_deref)
                .ok_or(crate::cache::CacheError::Missing)
                .and_then(|cache| {
                    cache.pending_commands_from_existing(
                        _admission.directory(),
                        &self.options.data_dir,
                    )
                }),
        };
        #[cfg(unix)]
        if !force_busy && matches!(locked_snapshot, Err(crate::cache::CacheError::Busy)) {
            return Err(CoreError::Unavailable(
                "revocation cleanup is waiting for an active cache writer; retry".into(),
            ));
        }
        #[cfg(unix)]
        let snapshot_missing = matches!(locked_snapshot, Err(crate::cache::CacheError::Missing));
        #[cfg(unix)]
        let snapshot = match &locked_snapshot {
            Ok(locked) => revoked_commands::preserve_in(_admission.directory(), &locked.pending),
            Err(_) => Err(CoreError::Internal(
                "unconfirmed identity snapshot unavailable".into(),
            )),
        };
        #[cfg(not(unix))]
        let snapshot_missing = cache.is_none();
        #[cfg(not(unix))]
        let snapshot = cache
            .ok_or(crate::cache::CacheError::Missing)
            .map_err(CoreError::from)
            .and_then(|cache| {
                #[cfg(windows)]
                {
                    cache
                        .pending_commands_verified(_admission.directory(), &self.options.data_dir)
                        .map_err(Into::into)
                }
                #[cfg(not(windows))]
                {
                    cache.pending_commands().map_err(Into::into)
                }
            })
            .and_then(|pending| revoked_commands::preserve_in(_admission.directory(), &pending));
        let retention = if fence.is_ok() {
            revoked_commands::write_marker_in(
                _admission.directory(),
                if snapshot.is_ok() && pending_cleanup.is_ok() {
                    "confirmed-saved"
                } else {
                    "confirmed-lost"
                },
            )
        } else {
            Ok(())
        };
        #[cfg(feature = "protocol-testing")]
        if let Some(path) = std::env::var_os("WHALEREIN_TEST_SNAPSHOT_BEFORE_UNLINK") {
            let path = std::path::PathBuf::from(path);
            std::fs::write(&path, b"paused")
                .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            while path.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        #[cfg(unix)]
        let cleanup = self.clear_local_pairing_credentials(
            device_id,
            _admission.directory(),
            locked_snapshot.as_ref().ok(),
        );
        #[cfg(not(unix))]
        let cleanup = self.clear_local_pairing_credentials(device_id, _admission.directory(), None);
        if cleanup.is_err() {
            let _ = revoked_commands::write_marker_in(_admission.directory(), "confirmed-conflict");
        }
        #[cfg(unix)]
        drop(locked_snapshot);
        cleanup?;
        pending_cleanup?;
        fence?;
        retention?;
        identity?;
        if snapshot_missing && !resume {
            return Ok(());
        }
        snapshot.map_err(|_| {
            CoreError::Internal(
                "sensitive credentials were cleared; unable to verify whether unconfirmed command or pairing identities were retained"
                    .into(),
            )
        })
    }

    fn clear_local_pairing_secrets(&self, device_id: &str) -> CoreResult<()> {
        Self::clear_pairing_secrets_with(&self.options.secrets, device_id)
    }

    fn clear_pairing_secrets_with(
        secrets: &std::sync::Arc<dyn SecretStore>,
        device_id: &str,
    ) -> CoreResult<()> {
        let mut failed = false;
        for key in [
            keys::device_token(device_id),
            keys::cache_key(device_id),
            keys::diagnostic_salt(device_id),
        ] {
            failed |= secrets.delete(&key).is_err();
        }
        if failed {
            Err(CoreError::Internal(
                "pairing secret cleanup incomplete".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn clear_local_pairing_credentials(
        &self,
        device_id: &str,
        directory: &std::fs::File,
        #[cfg(unix)] pinned: Option<&crate::cache::LockedPendingSnapshot>,
        #[cfg(not(unix))] _pinned: Option<&()>,
    ) -> CoreResult<()> {
        // Attempt every cleanup independently. The fence remains in place so
        // a new process can retry failed deletions using the profile identity.
        let mut failed = self.clear_local_pairing_secrets(device_id).is_err();
        #[cfg(windows)]
        let mut database_bound = true;
        let cached = match self.cache.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => {
                failed = true;
                None
            }
        };
        if let Some(cache) = cached {
            // On Unix an already-open SQLite connection may have been opened
            // through a replaced profile path. Never mutate that connection;
            // remove only the entries bound to the held profile directory.
            #[cfg(unix)]
            drop(cache);
            #[cfg(windows)]
            {
                if cache.matches_profile(directory, &self.options.data_dir) {
                    // The SQLite rows are disposable; the token and cache key
                    // have already been removed. A Windows sharing violation
                    // must not turn a completed server revocation into a
                    // failed command.
                    let _ = cache.clear();
                } else {
                    database_bound = false;
                }
                // Closing the connection is what releases the Windows file
                // handle; clearing rows alone is not enough.
                let _ = cache.release_for_revocation();
                // Windows cannot delete an open SQLite file. Drop the last
                // connection before removing the profile entry; otherwise the
                // revocation reports incomplete cleanup and leaves the fence.
                drop(cache);
            }
            #[cfg(not(any(unix, windows)))]
            {
                failed |= cache.clear().is_err();
            }
        }
        #[cfg(unix)]
        {
            if let Some(pinned) = pinned {
                if !pinned.matches_name(directory) {
                    failed = true;
                } else {
                    failed |=
                        revoked_commands::remove_profile_entry(directory, "cache.sqlite3").is_err();
                    failed |= !pinned.removed_from_profile(directory);
                }
            } else {
                failed |=
                    revoked_commands::remove_profile_entry(directory, "cache.sqlite3").is_err();
            }
        }
        #[cfg(windows)]
        if database_bound {
            let _ = revoked_commands::remove_profile_entry(directory, "cache.sqlite3");
        }
        #[cfg(not(any(unix, windows)))]
        {
            failed |= revoked_commands::remove_profile_entry(directory, "cache.sqlite3").is_err();
        }
        for name in [
            "cache.sqlite3-journal",
            "cache.sqlite3-wal",
            "cache.sqlite3-shm",
        ] {
            #[cfg(windows)]
            let _ = revoked_commands::remove_profile_entry(directory, name);
            #[cfg(not(windows))]
            {
                failed |= revoked_commands::remove_profile_entry(directory, name).is_err();
            }
        }
        failed |= revoked_commands::remove_profile_entry(
            directory,
            self.config_store()
                .file_name()
                .to_str()
                .expect("ASCII state name"),
        )
        .is_err();
        *self.config.write().expect("config lock") = None;
        if failed {
            Err(CoreError::Internal("pairing cleanup incomplete".into()))
        } else {
            Ok(())
        }
    }

    // -----------------------------------------------------------------
    // Events / tail
    // -----------------------------------------------------------------

    /// Synchronize the Source-owned committed conversation through opaque cursors.
    #[cfg(test)]
    pub async fn sync_session(&self, key: &SessionKey) -> CoreResult<u64> {
        self.authorized_session(key)?;
        let mut conversation = crate::conversation::Conversation::default();
        self.sync_conversation(
            key,
            &mut conversation,
            true,
            true,
            "",
            &mut watch::AcceptAll,
        )
        .await
    }

    pub(crate) fn mqtt_settings(&self, config: &PersistedConfig) -> CoreResult<MqttSettings> {
        let override_url = self.mqtt_override().or_else(|| config.mqtt_url.clone());
        let settings = match override_url {
            Some(url) => MqttSettings::parse(&url)?,
            None => {
                let access = Url::parse(&config.access_url)
                    .map_err(|_| CoreError::Config("invalid Access URL".into()))?;
                MqttSettings::from_access_url(&access)?
            }
        };
        Ok(settings
            .with_ca(self.ca_pem())
            .with_platform_roots(self.platform_roots())
            .with_transport_budget(self.transport_budget()))
    }

    // -----------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------

    /// Snapshot stored on disk. This does not open a channel.
    pub fn doctor(&self) -> CoreResult<DoctorReport> {
        self.doctor_with_cache_status().map(|(report, _)| report)
    }

    /// Snapshot stored on disk plus an explicit local-cache warning.
    ///
    /// A corrupt `cache.sqlite3` must not make diagnostics unavailable: the
    /// report is built from an empty in-memory cache and the second value is
    /// `Some("cache_unavailable")`. Operations that need the real cache keep
    /// failing through [`Core::cache`]. Callers that can present the warning
    /// should use this method; [`Core::doctor`] stays source-compatible.
    pub fn doctor_with_cache_status(&self) -> CoreResult<(DoctorReport, Option<&'static str>)> {
        let address = self
            .current_config()
            .map(|config| config.access_url.clone());
        match self.cache_or_empty() {
            Ok(cache) => Ok((diagnostics::build_report(&cache, address), None)),
            Err(CoreError::Cache(_)) => {
                let empty = Cache::open_in_memory([0u8; 32])?;
                Ok((
                    diagnostics::build_report(&empty, address),
                    Some("cache_unavailable"),
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// JSON preview for an in-app diagnostic share. It reads only local state:
    /// no file is written and no channel is opened. The payload preserves the
    /// existing export redaction policy (schema/versions, Access address,
    /// redacted Host labels with in-memory connection state, command counts,
    /// metrics, last failure code and the field/redaction manifest) and never
    /// contains message or draft text, tool arguments, credentials, invitation
    /// or device tokens, private keys or full invitation URIs.
    pub fn diagnostics_preview(&self) -> CoreResult<String> {
        let (report, cache_warning) = self.doctor_with_cache_status()?;
        let failure = diagnostics::journal::load_failure(&self.options.data_dir);
        let config = self.current_config();
        let mut host_ids = Vec::new();
        if let Some(config) = &config {
            host_ids.push(config.host_id.clone());
            host_ids.extend(config.host_fingerprints.keys().cloned());
        }
        host_ids.sort();
        host_ids.dedup();
        let hosts = host_ids
            .iter()
            .enumerate()
            .map(|(index, host_id)| diagnostics::PreviewHost {
                label: format!("host-{}", index + 1),
                connection_state: self.runtime.state(host_id).as_str().to_owned(),
            })
            .collect::<Vec<_>>();
        diagnostics::render_preview(
            &report,
            cache_warning,
            failure.as_ref(),
            &hosts,
            &self.metrics(),
            config.as_ref().map(|config| config.protocol_version),
        )
    }

    /// Write a redacted JSON diagnostic bundle. The caller must confirm first.
    ///
    /// A symbolic-link target is refused before anything is written: following
    /// the link would apply the private permissions to, and overwrite, a file
    /// the user did not name.
    pub fn export_diagnostics(&self, path: &Path) -> CoreResult<ExportManifest> {
        ensure_export_target_is_not_a_symlink(path)?;
        let (report, cache_warning) = self.doctor_with_cache_status()?;
        let mut manifest = diagnostics::render_export(&report);
        let bundle = serde_json::json!({
            "schema": "whalerein/v1",
            "generated_at_unix_ms": crate::util::now_unix_ms(),
            "report": report,
            "cache_warning": cache_warning,
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
        write_private_bundle(path, &bytes)?;
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
        key: &SessionKey,
    ) -> CoreResult<Vec<crate::conversation::ConversationMessage>> {
        Ok(self.cache()?.history_messages(key)?)
    }

    // -----------------------------------------------------------------
    // Conversation drafts
    // -----------------------------------------------------------------

    /// Load the cache key that encrypts the local draft store. Reads never
    /// create a missing key unless the profile lock is held.
    fn profile_cache_key(&self) -> CoreResult<[u8; 32]> {
        let config = self.require_config()?;
        match self
            .options
            .secrets
            .load(&keys::cache_key(&config.device_id))?
        {
            Some(bytes) => bytes
                .try_into()
                .map_err(|_| CoreError::Security("cache key is invalid".into())),
            None => {
                let _guard = self.identity_lock()?;
                Ok(load_or_create_cache_key(
                    &self.options.secrets,
                    &config.device_id,
                )?)
            }
        }
    }

    /// Persist an encrypted draft for one explicit Host and session scope.
    /// Empty text deletes the draft, matching an emptied editor.
    pub fn save_draft(&self, host_id: &str, session_id: &str, text: &str) -> CoreResult<()> {
        let cache = self.cache()?;
        let key = self.profile_cache_key()?;
        crate::drafts::save(&cache, &key, host_id, session_id, text)
    }

    /// Load a draft for one explicit Host and session scope.
    pub fn load_draft(&self, host_id: &str, session_id: &str) -> CoreResult<Option<String>> {
        let cache = self.cache()?;
        let key = self.profile_cache_key()?;
        crate::drafts::load(&cache, &key, host_id, session_id)
    }

    /// Remove the draft for one explicit Host and session scope.
    pub fn clear_draft(&self, host_id: &str, session_id: &str) -> CoreResult<()> {
        let cache = self.cache()?;
        crate::drafts::clear(&cache, host_id, session_id)
    }

    /// Remove every local draft for one Host, for example when the app
    /// discards a paired Host.
    pub fn clear_host_drafts(&self, host_id: &str) -> CoreResult<()> {
        let cache = self.cache()?;
        crate::drafts::clear_host(&cache, host_id)
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
mod cloud_session_tests;
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod queue_tests;

#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod session_tests;

#[cfg(test)]
mod draft_tests;

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
pub(crate) mod watch;

mod operation;
pub use operation::{
    AccountState, ClientStatus, DeviceAccess, SendFollowUp, SendOperation, SendReply,
};
mod pairing_recovery;

#[path = "wait.rs"]
mod wait;
