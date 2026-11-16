//! Core orchestration: lifecycle, pairing, snapshot queries, commands, sync
//! and diagnostics. CLI calls this type only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use url::Url;
use whalerein_proto::{
    ApprovalDecision, GetApproval, GetSession, Interrupt, ListSessions, RequestPayload,
    ResolveApproval, ResponsePayload, SendMessage,
};

use super::config::{self, ConfigStore, PersistedConfig};
use super::error::{CoreError, CoreResult};
use super::types::{
    ApprovalInfo, CommandHandle, CreateSessionInput, DoctorReport, ExportManifest,
    HostCapabilities, HostInfo, ModelCatalog, ModelChoice, ModelSelectionReceipt, OnlineStatus,
    PairingOutcome, PairingPrompt, PendingCommand, PendingQueue, PendingQueueAction,
    QuestionAnswer, QuestionBatchInfo, QuestionPageInfo, RevocationOutcome, RevocationRetention,
    SessionList, SessionModel, SessionSummary,
};
use crate::access::{AccessClient, DeviceMqttClient, MqttConnectionEvent, MqttSettings};
use crate::cache::Cache;
use crate::commands::CommandService;
use crate::diagnostics;
use crate::pairing::{default_capabilities, parse_pairing_uri, PairingDecision, PairingInitiator};
use crate::runtime::channel::{ensure_channel, ensure_channel_after, require_payload_capability};
use crate::runtime::{ConnectionState, Runtime};
use crate::security::{keys, load_device_token, load_or_create_cache_key, SecretStore};
use crate::sync::SyncService;

pub(crate) mod revoked_commands;

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

#[derive(Clone)]
pub struct Core {
    options: CoreOptions,
    session_host: Option<String>,
    config: Arc<RwLock<Option<PersistedConfig>>>,
    runtime: Arc<Runtime>,
    journal: Arc<diagnostics::journal::Journal>,
    identity: Arc<Mutex<Option<Arc<crate::security::device::DeviceIdentity>>>>,
    identity_init: Arc<Mutex<()>>,
    cache: Arc<Mutex<Option<Arc<Cache>>>>,
    watch_active: Arc<AtomicBool>,
    #[cfg(all(test, unix))]
    fail_waiting_marker: Arc<AtomicBool>,
}

impl Core {
    /// Return an independently scoped facade for session operations on one Host.
    pub fn with_session_host(&self, host_id: &str) -> CoreResult<Self> {
        if host_id.trim().is_empty() {
            return Err(CoreError::InvalidInput("Host identity missing".into()));
        }
        let mut scoped = self.clone();
        scoped.session_host = Some(host_id.to_owned());
        Ok(scoped)
    }

    /// Allocate a non-secret command identity before starting a control.
    pub fn new_command_id() -> String {
        crate::util::random_id(16)
    }
    pub fn new(options: CoreOptions) -> CoreResult<Self> {
        let existed = options.data_dir.exists();
        std::fs::create_dir_all(&options.data_dir)
            .map_err(|_| CoreError::Config("data directory could not be created".into()))?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut open = std::fs::OpenOptions::new();
            open.read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let directory = open.open(&options.data_dir).map_err(|_| {
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
        let core = Self {
            options,
            session_host: None,
            config: Arc::new(RwLock::new(config)),
            runtime: Arc::new(Runtime::new()),
            journal: Arc::new(diagnostics::journal::Journal::default()),
            identity: Arc::new(Mutex::new(None)),
            identity_init: Arc::new(Mutex::new(())),
            cache: Arc::new(Mutex::new(None)),
            watch_active: Arc::new(AtomicBool::new(false)),
            #[cfg(all(test, unix))]
            fail_waiting_marker: Arc::new(AtomicBool::new(false)),
        };
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

    fn verify_access_generation(&self, access: &AccessClient) -> CoreResult<()> {
        let config = self.config_store().load()?.ok_or(CoreError::NotPaired)?;
        let token = load_device_token(&self.options.secrets, &config.device_id)?
            .ok_or(CoreError::NotPaired)?;
        if self
            .current_config()
            .as_ref()
            .is_none_or(|original| original.device_id != config.device_id)
            || !access.has_token(&token)
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
        let _guard = self.identity_lock()?;
        if !local_only && self.is_paired() && !self.runtime.is_revoked() {
            return Err(CoreError::Conflict(
                "revoke the active pairing before forgetting this device".into(),
            ));
        }
        let _admission =
            crate::security::device::ProfileLock::acquire_admission_waiting(&self.options.data_dir)
                .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
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
            self.clear_local_pairing_credentials(&config.device_id, _guard.directory(), None)?;
        }
        crate::security::device::forget(&self.options.secrets, &self.options.data_dir)?;
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

    fn cache(&self) -> CoreResult<Arc<Cache>> {
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
            self.record_cache_rebuild(reason);
        }
        let cache = Arc::new(cache);
        *slot = Some(cache.clone());
        Ok(cache)
    }

    /// Leave a bounded, secret-free marker when a cache is quarantined and
    /// rebuilt. `doctor` and diagnostics can report the storage state without
    /// opening the cache, and the pairing identity is explicitly preserved.
    fn record_cache_rebuild(&self, reason: crate::cache::CacheRebuild) {
        self.journal
            .record("storage", "cache_rebuild", "succeeded", reason.reason());
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

    fn sync_service(&self) -> CoreResult<SyncService> {
        Ok(SyncService::new(self.cache()?))
    }

    fn command_service(&self) -> CoreResult<CommandService> {
        let access = Arc::new(self.access_client()?);
        Ok(CommandService::new(access, self.cache()?)
            .with_admission(self.options.data_dir.clone(), self.options.secrets.clone()))
    }

    fn cache_or_empty(&self) -> CoreResult<Arc<Cache>> {
        if self.is_paired() {
            self.cache()
        } else {
            Ok(Arc::new(Cache::open_in_memory([0u8; 32])?))
        }
    }

    async fn host_for_session(&self, session_id: &str) -> CoreResult<String> {
        Ok(self.session_key(session_id).await?.host_id)
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

    async fn ensure_channel(
        &self,
        host_id: &str,
        force_new: bool,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.ensure_channel_inner(host_id, force_new, None, None)
            .await
    }

    async fn ensure_channel_after(
        &self,
        host_id: &str,
        stale_channel_id: &str,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.ensure_channel_inner(host_id, true, Some(stale_channel_id), None)
            .await
    }

    async fn ensure_wait_channel(
        &self,
        host_id: &str,
        force_new: bool,
        stale_channel_id: Option<&str>,
        transport: Arc<crate::access::http::TransportObservation>,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.ensure_channel_inner(host_id, force_new, stale_channel_id, Some(transport))
            .await
    }

    async fn ensure_channel_inner(
        &self,
        host_id: &str,
        force_new: bool,
        stale_channel_id: Option<&str>,
        transport: Option<Arc<crate::access::http::TransportObservation>>,
    ) -> CoreResult<crate::security::SecureChannel> {
        self.journal.record("core", "handshake", "started", "none");
        let opened = async {
            let config = self.require_config()?;
            let identity = self.device_identity()?;
            let access = self.access_client()?;
            let access = if let Some(transport) = transport {
                access.with_transport_observation(transport)
            } else {
                access
            };
            let expected = self.confirmed_host_fingerprint(host_id)?;
            if let Some(stale) = stale_channel_id {
                ensure_channel_after(
                    &self.runtime,
                    &access,
                    &identity.key,
                    host_id,
                    &config.device_id,
                    stale,
                    Some(&expected),
                )
                .await
            } else {
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
            require_payload_capability(&self.runtime, host_id, &payload)?;
            let commands = self.command_service()?;
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
                    self.journal.record(
                        error.source_layer(),
                        "query_channel_recovery",
                        "retrying",
                        error.code(),
                    );
                    let fresh = self
                        .ensure_channel_after(host_id, channel.channel_id())
                        .await?;
                    require_payload_capability(&self.runtime, host_id, &payload)?;
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
        self.control_with_id(host_id, payload, None).await
    }

    async fn control_with_id(
        &self,
        host_id: &str,
        payload: RequestPayload,
        command_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        let result = async {
            let config = self.require_config()?;
            let channel = self.ensure_channel(host_id, false).await?;
            require_payload_capability(&self.runtime, host_id, &payload)?;
            let commands = self
                .command_service()?
                .with_host_fingerprint(Some(self.confirmed_host_fingerprint(host_id)?));
            let submitted = match command_id {
                Some(id) => {
                    commands
                        .control_with_id(host_id, &config.device_id, &channel, payload, Some(id))
                        .await
                }
                None => {
                    commands
                        .control(host_id, &config.device_id, &channel, payload)
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
        self.journal.record(
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
        self.journal
            .record("host", "command_result_recovery", "started", "none");
        let config = self.require_config()?;
        let mut channel = self
            .ensure_channel(host_id, true)
            .await
            .map_err(|error| self.uncertain_recovery_error(error))?;
        let commands = self.command_service()?;
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

        let ca = config::ca_pem_from_env();
        let access = AccessClient::with_ca(parsed.access_url.clone(), None, ca.as_deref())?
            .with_trace_id(self.diagnostic_id());
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
            config::mqtt_url_from_env().or(parsed.mqtt_url.clone()),
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
        // Two explicit scopes must not silently disagree about which Host the
        // listing describes.
        if let Some(scoped) = &self.session_host {
            if scoped != host_id {
                return Err(CoreError::Conflict(format!(
                    "--host {scoped} and sessions --host {host_id} disagree"
                )));
            }
        }
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
    pub async fn list_sessions(&self, host_id: &str) -> CoreResult<SessionList> {
        self.sessions(host_id).await
    }

    pub async fn get_session(&self, session_id: &str) -> CoreResult<SessionSummary> {
        let key = self.session_key(session_id).await?;
        let cache = self.cache()?;
        if let Some(session) = cache.get_session(&key)? {
            return Ok(session);
        }
        let host = key.host_id;
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
    pub async fn read_session_model(&self, session_id: &str) -> CoreResult<SessionModel> {
        let host = self.host_for_session(session_id).await?;
        if !self.host_capabilities(&host).await?.model_read {
            return Err(CoreError::Unsupported);
        }
        let response = self
            .query(
                &host,
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
        session_id: &str,
        provider: &str,
        model: &str,
        effort: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        let host = self.host_for_session(session_id).await?;
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
        if let Ok(current) = self.read_session_model(session_id).await {
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
                &host,
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
        self.await_command_terminal(&host, handle).await
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
    pub async fn fork_session(&self, session_id: &str) -> CoreResult<CommandHandle> {
        Ok(self.fork_session_outcome(session_id).await?.handle)
    }

    /// Fork and resolve the confirmed child's working directory.
    ///
    /// Fork creates a child that shares the parent's working directory; the
    /// child summary is a second, best-effort read. Its failure cannot undo the
    /// fork, so it is returned as an explicit error code next to the handle
    /// rather than dropped.
    pub async fn fork_session_outcome(&self, session_id: &str) -> CoreResult<ForkOutcome> {
        let handle = self.fork_session_receipt(session_id).await?;
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
        match self.get_session(&child).await {
            Ok(summary) if !summary.cwd.is_empty() => outcome.working_directory = Some(summary.cwd),
            Ok(_) => outcome.working_directory_error = Some("not_available".into()),
            Err(error) => outcome.working_directory_error = Some(error.code().to_owned()),
        }
        Ok(outcome)
    }

    async fn fork_session_receipt(&self, session_id: &str) -> CoreResult<CommandHandle> {
        let host = self.host_for_session(session_id).await?;
        let response = self
            .query(
                &host,
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
                &host,
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
            self.await_command_terminal(&host, handle).await?
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
    pub async fn pending_queue(&self, session_id: &str) -> CoreResult<PendingQueue> {
        let host = self.host_for_session(session_id).await?;
        let response = self
            .query(
                &host,
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
        session_id: &str,
        item_id: &str,
        action: PendingQueueAction,
        text: &str,
    ) -> CoreResult<CommandHandle> {
        let host = self.host_for_session(session_id).await?;
        let handle = self
            .control(
                &host,
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
        self.await_command_terminal(&host, handle).await
    }

    pub async fn send_message(&self, session_id: &str, text: &str) -> CoreResult<CommandHandle> {
        self.send_message_with_delivery(
            session_id,
            text,
            whalerein_proto::DeliveryMode::EnqueueAfterRun,
        )
        .await
    }

    pub async fn send_message_with_delivery(
        &self,
        session_id: &str,
        text: &str,
        mode: whalerein_proto::DeliveryMode,
    ) -> CoreResult<CommandHandle> {
        let host = self.host_for_session(session_id).await?;
        // A non-default delivery mode is a source capability. Reject one the
        // Host does not advertise before any command is submitted; an empty
        // advertisement is a legacy Host and keeps the old pass-through path.
        if matches!(
            mode,
            whalerein_proto::DeliveryMode::StartIfIdle | whalerein_proto::DeliveryMode::Steer
        ) {
            // Read the Source's current advertisement rather than the cached
            // session summary, which may predate this Host build.
            let list = self.sessions(&host).await?;
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
        if let Some(retained) = cache.unresolved_send_intent(&host, session_id, &digest)? {
            match self.recover_pending_command(&retained).await {
                Ok(handle) => {
                    if handle.status == OnlineStatus::HostAccepted
                        || handle.status == OnlineStatus::Received
                    {
                        let request_id = handle.request_id.clone();
                        let command_id = handle.command_id.clone();
                        return self
                            .observe_send_terminal(
                                &host, session_id, &digest, request_id, command_id, handle,
                            )
                            .await;
                    }
                    cache.resolve_send_intent(&host, session_id, &digest)?;
                    return Ok(handle);
                }
                Err(CoreError::NotFound(_)) => {
                    // The original command is gone; a fresh send cannot
                    // duplicate a message the Host no longer has.
                    cache.resolve_send_intent(&host, session_id, &digest)?;
                }
                Err(error) => return Err(error),
            }
        }
        let handle = self
            .control(
                &host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                        SendMessage {
                            session_id: session_id.to_owned(),
                            text: text.to_owned(),
                            delivery_mode: mode as i32,
                        },
                    )),
                },
            )
            .await?;
        if handle.status != OnlineStatus::HostAccepted {
            cache.resolve_send_intent(&host, session_id, &digest)?;
            return Ok(handle);
        }
        let request_id = handle.request_id.clone();
        let command_id = handle.command_id.clone();
        self.observe_send_terminal(&host, session_id, &digest, request_id, command_id, handle)
            .await
    }

    /// Follow an accepted send to a terminal receipt. An unknown observation
    /// keeps the command identity (and an idempotency record) instead of
    /// inviting a resend that would duplicate the user message.
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
        let config = self.require_config()?;
        let channel = self
            .ensure_channel(host_id, false)
            .await
            .map_err(|error| self.uncertain_recovery_error(error))?;
        let commands = self.command_service()?;
        let access = self.access_client()?;
        let heartbeat_id = handle.request_id.clone();
        let terminal = commands.await_command_result(
            host_id,
            &config.device_id,
            &channel,
            &handle.request_id,
            &handle.command_id,
        );
        tokio::pin!(terminal);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let result = loop {
            tokio::select! {
                result = &mut terminal => break result,
                _ = heartbeat.tick() => {
                    let _ = access.heartbeat_request(&heartbeat_id).await;
                }
            }
        };
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
    async fn approval_host(&self) -> CoreResult<String> {
        let hosts = self.hosts().await?;
        self.approval_host_from(&hosts)
    }

    fn approval_host_from(&self, hosts: &[HostInfo]) -> CoreResult<String> {
        if let Some(host) = &self.session_host {
            if !hosts.iter().any(|item| &item.host_id == host) {
                return Err(CoreError::NotFound("Host is not authorized".into()));
            }
            return Ok(host.clone());
        }
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
    pub async fn get_approval(&self, approval_id: &str) -> CoreResult<ApprovalInfo> {
        if approval_id.is_empty() {
            return Err(CoreError::InvalidInput("approval identity missing".into()));
        }
        let host = self.approval_host().await?;
        let response = self
            .query(
                &host,
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
    pub async fn list_approvals(&self, session_id: &str) -> CoreResult<Vec<ApprovalInfo>> {
        let host = if self.session_host.is_some() {
            let hosts = self.hosts().await?;
            self.approval_host_from(&hosts)?
        } else {
            self.host_for_session(session_id).await?
        };
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

    pub async fn list_questions(
        &self,
        session_id: &str,
        cursor: &str,
    ) -> CoreResult<QuestionPageInfo> {
        let host = self.host_for_session(session_id).await?;
        let response = self
            .query(
                &host,
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
                Ok(QuestionPageInfo {
                    batches: page.batches.into_iter().map(Into::into).collect(),
                    next_cursor: page.next_cursor,
                })
            }
            _ => Err(CoreError::Internal(
                "unexpected question list response".into(),
            )),
        }
    }

    pub async fn get_question(
        &self,
        session_id: &str,
        batch_id: &str,
    ) -> CoreResult<QuestionBatchInfo> {
        let host = self.host_for_session(session_id).await?;
        let response = self
            .query(
                &host,
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
        batch: &QuestionBatchInfo,
        answers: Vec<QuestionAnswer>,
    ) -> CoreResult<CommandHandle> {
        self.answer_question_with_id(batch, answers, None).await
    }

    /// A caller-supplied command ID allows a different client to explicitly
    /// cancel the accepted open answer without exposing its content.
    pub async fn answer_question_with_id(
        &self,
        batch: &QuestionBatchInfo,
        answers: Vec<QuestionAnswer>,
        command_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        let phase = whalerein_proto::QuestionPhase::from_str_name(&batch.phase)
            .filter(|phase| {
                matches!(
                    phase,
                    whalerein_proto::QuestionPhase::QuestionOpen
                        | whalerein_proto::QuestionPhase::QuestionContinued
                )
            })
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
        let host = self.host_for_session(&batch.session_id).await?;
        let handle = self
            .control_with_id(
                &host,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::AnswerQuestion(
                        whalerein_proto::AnswerQuestion {
                            session_id: batch.session_id.clone(),
                            batch_id: batch.batch_id.clone(),
                            source_epoch: batch.source_epoch.clone(),
                            expected_version: batch.version,
                            expected_digest: batch.digest.clone(),
                            expected_phase: phase as i32,
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
            self.await_command_terminal(&host, handle).await?
        } else {
            handle
        };
        if handle.status == OnlineStatus::Succeeded {
            let expected_receipt_phase = match phase {
                whalerein_proto::QuestionPhase::QuestionOpen => "QUESTION_SETTLED",
                _ => "QUESTION_REPLY_QUEUED",
            };
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
        session_id: &str,
        command_id: &str,
    ) -> CoreResult<bool> {
        if session_id.is_empty() || command_id.is_empty() {
            return Err(CoreError::InvalidInput(
                "session and command are required".into(),
            ));
        }
        let host = self.host_for_session(session_id).await?;
        let response = self
            .query(
                &host,
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
        let host = self.approval_host().await?;
        self.control(
            &host,
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
        let host = self.host_for_session(session_id).await?;
        let handle = self
            .control(
                &host,
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
            match self.await_command_terminal(&host, handle).await {
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
        let access = self.access_client()?;
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
                    if !matches!(error, CoreError::Cache(_) | CoreError::Unavailable(_)) {
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
        let access = self.access_client()?;
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
        self.journal
            .record("access", "authorization_check", "started", error.code());
        match access.list_hosts().await {
            Err(denied) if CoreError::from(denied.clone()).device_authorization_lost() => {
                self.journal
                    .record("access", "authorization_check", "revoked", error.code());
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
                self.journal
                    .record("access", "authorization_check", "pairing_preserved", "none");
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
                    failed |= cache.clear().is_err();
                } else {
                    database_bound = false;
                    failed = true;
                    let _ = cache.release_for_revocation();
                }
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
            failed |= revoked_commands::remove_profile_entry(directory, "cache.sqlite3").is_err();
        }
        #[cfg(not(any(unix, windows)))]
        {
            failed |= revoked_commands::remove_profile_entry(directory, "cache.sqlite3").is_err();
        }
        for name in [
            "cache.sqlite3-journal",
            "cache.sqlite3-wal",
            "cache.sqlite3-shm",
            "config.json",
        ] {
            failed |= revoked_commands::remove_profile_entry(directory, name).is_err();
        }
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
    pub async fn sync_session(&self, session_id: &str) -> CoreResult<u64> {
        let key = self.session_key(session_id).await?;
        let mut conversation = crate::conversation::Conversation::default();
        self.sync_conversation(&key, &mut conversation, true, true, &mut |_| true)
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
                connection_state: self.get_connection_state(host_id).as_str().to_owned(),
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
        key: &whalerein_proto::SessionKey,
    ) -> CoreResult<Vec<whalerein_proto::ConversationMessage>> {
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
mod watch;

mod pairing_recovery;

#[path = "wait.rs"]
mod wait;
