//! Local profile registry and active selection.
//!
//! A profile is a local state directory with one stable identity and one
//! service authority. Standalone profiles keep the historical flat directory
//! layout in place; managed profiles live under `profiles/<opaque-id>/`.
//!
//! The registry is non-secret metadata. Private keys, tokens and cache keys
//! remain in `SecretStore` under the existing profile-scoped keys. Account
//! transport is not implemented here: an account-backed profile is only a
//! validated local selection until its feasibility gates pass.

mod activation;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub(crate) use activation::ActivationGuard;

use crate::account::{
    AccountBinding, CustomServiceInput, Environment, ProfileMode, ServiceDescriptor, ServiceKind,
};
use crate::api::{CoreError, CoreResult};
use crate::security::device;
use crate::security::SecretStore;

pub const PROFILES_SCHEMA: &str = "whalerein/v1/profiles";
pub const PROFILES_FILE: &str = "profiles.json";
pub const PROFILES_DIR: &str = "profiles";
pub const LEGACY_PROFILE_ID: &str = "default";
pub const PRODUCTION_PROFILE_NAME: &str = "official-production";
pub const STAGING_PROFILE_NAME: &str = "official-staging";

/// Durable, non-secret profile registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesRegistry {
    pub schema: String,
    pub revision: u64,
    pub activation_generation: u64,
    pub active: String,
    pub profiles: Vec<ProfileRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRecord {
    pub id: String,
    pub name: String,
    pub mode: ProfileMode,
    pub legacy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountBinding>,
    #[serde(default)]
    pub created_unix_ms: u64,
}

impl ProfilesRegistry {
    fn new(active: String, profiles: Vec<ProfileRecord>) -> Self {
        Self {
            schema: PROFILES_SCHEMA.into(),
            revision: 1,
            activation_generation: 0,
            active,
            profiles,
        }
    }

    pub fn validate(&self) -> CoreResult<()> {
        if self.schema != PROFILES_SCHEMA {
            return Err(CoreError::Config(
                "unsupported profile registry schema; stop older clients".into(),
            ));
        }
        if self.revision == 0 {
            return Err(CoreError::Config(
                "invalid profile registry revision".into(),
            ));
        }
        if self.active.is_empty() {
            return Err(CoreError::Config(
                "profile registry has no active profile".into(),
            ));
        }
        if self.profiles.is_empty() {
            return Err(CoreError::Config("profile registry has no profiles".into()));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        let mut legacy = 0usize;
        for profile in &self.profiles {
            validate_profile_id(&profile.id)?;
            validate_profile_name(&profile.name)?;
            if !ids.insert(profile.id.as_str()) {
                return Err(CoreError::Config("duplicate profile id".into()));
            }
            if !names.insert(profile.name.to_ascii_lowercase()) {
                return Err(CoreError::Config("duplicate profile name".into()));
            }
            if profile.legacy {
                legacy += 1;
                if profile.id != LEGACY_PROFILE_ID || profile.name != LEGACY_PROFILE_ID {
                    return Err(CoreError::Config(
                        "legacy profile must keep its reserved identity".into(),
                    ));
                }
                if profile.mode != ProfileMode::Standalone || profile.service.is_some() {
                    return Err(CoreError::Config(
                        "legacy profile must remain standalone".into(),
                    ));
                }
            }
            match (&profile.mode, &profile.service) {
                (ProfileMode::Standalone, Some(_)) => {
                    return Err(CoreError::Config(
                        "standalone profile cannot select an account service".into(),
                    ));
                }
                (ProfileMode::Account, None) => {
                    return Err(CoreError::Config(
                        "account profile requires a service description".into(),
                    ));
                }
                _ => {}
            }
            if let Some(service) = &profile.service {
                service.validate()?;
                if (profile.name == PRODUCTION_PROFILE_NAME
                    && service.kind != ServiceKind::OfficialProduction)
                    || (profile.name == STAGING_PROFILE_NAME
                        && service.kind != ServiceKind::OfficialStaging)
                {
                    return Err(CoreError::Config(
                        "reserved profile name does not match its service".into(),
                    ));
                }
            }
            if let Some(account) = &profile.account {
                account.validate(profile.service.as_ref())?;
            }
        }
        if legacy > 1 {
            return Err(CoreError::Config(
                "profile registry cannot contain multiple legacy profiles".into(),
            ));
        }
        if !ids.contains(self.active.as_str()) {
            return Err(CoreError::Config(
                "active profile is not present in the registry".into(),
            ));
        }
        Ok(())
    }

    fn find(&self, name: &str) -> CoreResult<&ProfileRecord> {
        self.profiles
            .iter()
            .find(|profile| profile.name == name)
            .ok_or_else(|| CoreError::NotFound(format!("profile {name} does not exist")))
    }

    #[cfg(test)]
    fn find_mut(&mut self, name: &str) -> CoreResult<&mut ProfileRecord> {
        self.profiles
            .iter_mut()
            .find(|profile| profile.name == name)
            .ok_or_else(|| CoreError::NotFound(format!("profile {name} does not exist")))
    }
}

/// The effective selection shown by every client surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileSelection {
    pub registry_present: bool,
    pub active: Option<ProfileSummary>,
    /// A fresh client has no local profile but its ordinary account entry
    /// selects official production. No account network is contacted yet.
    pub default_service: Option<ServiceDescriptor>,
    pub standalone_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub mode: ProfileMode,
    pub legacy: bool,
    pub service: Option<ServiceDescriptor>,
    pub account: Option<AccountBinding>,
    pub active: bool,
    pub environment: Environment,
    pub testing: bool,
}

impl ProfileSummary {
    fn from_record(profile: &ProfileRecord, active: bool) -> Self {
        let environment = profile
            .service
            .as_ref()
            .map_or(Environment::Standalone, ServiceDescriptor::environment);
        Self {
            id: profile.id.clone(),
            name: profile.name.clone(),
            mode: profile.mode,
            legacy: profile.legacy,
            service: profile.service.clone(),
            account: profile.account.clone(),
            active,
            environment,
            testing: profile
                .service
                .as_ref()
                .is_some_and(ServiceDescriptor::is_testing),
        }
    }

    pub fn is_standalone(&self) -> bool {
        self.mode == ProfileMode::Standalone
    }
}

/// Resolved profile used by `Core::new`. Paths are stable and never moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileContext {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub id: Option<String>,
    pub name: Option<String>,
    pub mode: ProfileMode,
    pub service: Option<ServiceDescriptor>,
    pub legacy: bool,
    pub virtual_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivationOutcome {
    pub profile: ProfileSummary,
    pub previous: Option<String>,
    pub rollback_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovalOutcome {
    pub removed: ProfileSummary,
    pub active: ProfileSelection,
    /// Local profile deletion never revokes remote grants or account sessions.
    pub remote_revocation: &'static str,
}

pub struct Profiles {
    root: PathBuf,
}

impl Profiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read the effective selection without creating or migrating state.
    pub fn current(&self, secrets: &Arc<dyn SecretStore>) -> CoreResult<ProfileSelection> {
        if let Some(registry) = self.load()? {
            let active = registry.find_active()?;
            return Ok(ProfileSelection {
                registry_present: true,
                active: Some(ProfileSummary::from_record(active, true)),
                default_service: None,
                standalone_available: true,
            });
        }
        if self.has_legacy_state(secrets)? {
            let profile = ProfileRecord {
                id: LEGACY_PROFILE_ID.into(),
                name: LEGACY_PROFILE_ID.into(),
                mode: ProfileMode::Standalone,
                legacy: true,
                service: None,
                account: None,
                created_unix_ms: 0,
            };
            return Ok(ProfileSelection {
                registry_present: false,
                active: Some(ProfileSummary::from_record(&profile, true)),
                default_service: None,
                standalone_available: true,
            });
        }
        Ok(ProfileSelection {
            registry_present: false,
            active: None,
            default_service: Some(ServiceDescriptor::official_production()),
            standalone_available: true,
        })
    }

    /// List local profiles. Testing-only staging profiles stay hidden unless
    /// they are active or the caller explicitly asks for them.
    pub fn list(
        &self,
        secrets: &Arc<dyn SecretStore>,
        include_testing: bool,
    ) -> CoreResult<Vec<ProfileSummary>> {
        if let Some(registry) = self.load()? {
            let mut summaries = Vec::with_capacity(registry.profiles.len());
            for profile in &registry.profiles {
                let active = registry.active == profile.id;
                if profile
                    .service
                    .as_ref()
                    .is_some_and(ServiceDescriptor::is_testing)
                    && !include_testing
                    && !active
                {
                    continue;
                }
                summaries.push(ProfileSummary::from_record(profile, active));
            }
            return Ok(summaries);
        }
        if self.has_legacy_state(secrets)? {
            let profile = ProfileRecord {
                id: LEGACY_PROFILE_ID.into(),
                name: LEGACY_PROFILE_ID.into(),
                mode: ProfileMode::Standalone,
                legacy: true,
                service: None,
                account: None,
                created_unix_ms: 0,
            };
            return Ok(vec![ProfileSummary::from_record(&profile, true)]);
        }
        Ok(Vec::new())
    }

    /// Add official staging only through the explicit testing flag. It never
    /// activates the new profile or abandons the previous selection.
    pub fn add_official_staging(
        &self,
        name: &str,
        enable_testing: bool,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<ProfileSummary> {
        if !enable_testing {
            return Err(CoreError::Unsupported);
        }
        self.add_profile(
            name,
            ProfileMode::Account,
            ServiceDescriptor::official_staging(),
            secrets,
        )
    }

    /// Add an explicit compatible service without inheriting any other
    /// service's or Standalone Host's trust.
    pub fn add_custom(
        &self,
        name: &str,
        input: CustomServiceInput,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<ProfileSummary> {
        let service = ServiceDescriptor::custom(input)?;
        self.add_profile(name, ProfileMode::Account, service, secrets)
    }

    fn add_profile(
        &self,
        name: &str,
        mode: ProfileMode,
        service: ServiceDescriptor,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<ProfileSummary> {
        validate_profile_name(name)?;
        let _guard = acquire_registry_lock(&self.root)?;
        let mut registry = self.load_or_bootstrap(secrets)?;
        if registry
            .profiles
            .iter()
            .any(|profile| profile.name.eq_ignore_ascii_case(name))
        {
            return Err(CoreError::Conflict(format!(
                "profile {name} already exists"
            )));
        }
        let id = crate::util::random_id(16);
        let state_dir = self.managed_dir(&id);
        prepare_managed_dir(&state_dir)?;
        let record = ProfileRecord {
            id,
            name: name.to_owned(),
            mode,
            legacy: false,
            service: Some(service),
            account: None,
            created_unix_ms: now_unix_ms(),
        };
        let summary = ProfileSummary::from_record(&record, false);
        registry.profiles.push(record);
        self.save(&mut registry)?;
        Ok(summary)
    }

    /// Publish a new active profile only after every running client on the
    /// previous selection has stopped. The OS releases the lock on a crash.
    pub fn activate(&self, name: &str) -> CoreResult<ActivationOutcome> {
        let _guard = ActivationGuard::exclusive(&self.root)?;
        let mut registry = self
            .load()?
            .ok_or_else(|| CoreError::NotFound("no local profiles are registered".into()))?;
        let previous = registry.active.clone();
        let profile = registry.find(name)?.clone();
        if !profile.legacy {
            require_managed_dir(&self.managed_dir(&profile.id))?;
        }
        if previous == profile.id {
            return Ok(ActivationOutcome {
                profile: ProfileSummary::from_record(&profile, true),
                previous: Some(previous),
                rollback_warning: None,
            });
        }
        registry.active = profile.id.clone();
        registry.activation_generation = registry.activation_generation.saturating_add(1);
        let rollback_warning = if !profile.legacy && self.has_legacy_files() {
            Some(
                "Older binaries ignore the profile registry and would open the legacy Standalone state; stop them before relying on this selection."
                    .to_owned(),
            )
        } else {
            None
        };
        self.save(&mut registry)?;
        Ok(ActivationOutcome {
            profile: ProfileSummary::from_record(&profile, true),
            previous: Some(previous),
            rollback_warning,
        })
    }

    /// Remove only a managed profile's local state. The legacy Standalone
    /// profile keeps its historical state and cannot be deleted here.
    pub fn remove(&self, name: &str, secrets: &Arc<dyn SecretStore>) -> CoreResult<RemovalOutcome> {
        let _guard = ActivationGuard::exclusive(&self.root)?;
        let mut registry = self
            .load()?
            .ok_or_else(|| CoreError::NotFound("no local profiles are registered".into()))?;
        let record = registry.find(name)?.clone();
        if record.legacy {
            return Err(CoreError::Conflict(
                "the legacy Standalone profile state cannot be removed here".into(),
            ));
        }
        let state_dir = self.managed_dir(&record.id);
        self.clean_profile_secrets(&state_dir, secrets)?;
        remove_managed_dir(&self.root, &record.id)?;
        registry.profiles.retain(|profile| profile.id != record.id);
        if registry.active == record.id {
            let production = ensure_production_profile(&mut registry, self)?;
            registry.active = production;
            registry.activation_generation = registry.activation_generation.saturating_add(1);
        }
        if registry.profiles.is_empty() {
            return Err(CoreError::Internal(
                "removing the last profile would leave no active selection".into(),
            ));
        }
        self.save(&mut registry)?;
        Ok(RemovalOutcome {
            removed: ProfileSummary::from_record(&record, false),
            active: self.current(secrets)?,
            remote_revocation: "not_attempted",
        })
    }

    /// Resolve the active profile for Core startup. This creates a managed
    /// profile directory if the registry already selected one; it never moves
    /// legacy state and never initializes an account network client.
    pub fn resolve_active(&self, secrets: &Arc<dyn SecretStore>) -> CoreResult<ProfileContext> {
        if let Some(registry) = self.load()? {
            let profile = registry.find_active()?.clone();
            let state_dir = if profile.legacy {
                self.root.clone()
            } else {
                let state_dir = self.managed_dir(&profile.id);
                require_managed_dir(&state_dir)?;
                state_dir
            };
            return Ok(ProfileContext {
                root: self.root.clone(),
                state_dir,
                id: Some(profile.id),
                name: Some(profile.name),
                mode: profile.mode,
                service: profile.service,
                legacy: profile.legacy,
                virtual_default: false,
            });
        }
        if self.has_legacy_state(secrets)? {
            return Ok(ProfileContext {
                root: self.root.clone(),
                state_dir: self.root.clone(),
                id: Some(LEGACY_PROFILE_ID.into()),
                name: Some(LEGACY_PROFILE_ID.into()),
                mode: ProfileMode::Standalone,
                service: None,
                legacy: true,
                virtual_default: false,
            });
        }
        Ok(ProfileContext {
            root: self.root.clone(),
            state_dir: self.root.clone(),
            id: None,
            name: None,
            mode: ProfileMode::Standalone,
            service: Some(ServiceDescriptor::official_production()),
            legacy: false,
            virtual_default: true,
        })
    }

    fn load(&self) -> CoreResult<Option<ProfilesRegistry>> {
        let path = self.root.join(PROFILES_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(CoreError::Config(
                    "profile registry could not be read".into(),
                ))
            }
        };
        let registry: ProfilesRegistry = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Config("profile registry is not valid JSON".into()))?;
        registry.validate()?;
        Ok(Some(registry))
    }

    fn load_or_bootstrap(&self, secrets: &Arc<dyn SecretStore>) -> CoreResult<ProfilesRegistry> {
        if let Some(registry) = self.load()? {
            return Ok(registry);
        }
        if self.has_legacy_state(secrets)? {
            let legacy = ProfileRecord {
                id: LEGACY_PROFILE_ID.into(),
                name: LEGACY_PROFILE_ID.into(),
                mode: ProfileMode::Standalone,
                legacy: true,
                service: None,
                account: None,
                created_unix_ms: 0,
            };
            return Ok(ProfilesRegistry::new(legacy.id.clone(), vec![legacy]));
        }
        let production = self.new_production_record()?;
        Ok(ProfilesRegistry::new(
            production.id.clone(),
            vec![production],
        ))
    }

    fn new_production_record(&self) -> CoreResult<ProfileRecord> {
        let id = crate::util::random_id(16);
        let state_dir = self.managed_dir(&id);
        prepare_managed_dir(&state_dir)?;
        Ok(ProfileRecord {
            id,
            name: PRODUCTION_PROFILE_NAME.into(),
            mode: ProfileMode::Account,
            legacy: false,
            service: Some(ServiceDescriptor::official_production()),
            account: None,
            created_unix_ms: now_unix_ms(),
        })
    }

    fn save(&self, registry: &mut ProfilesRegistry) -> CoreResult<()> {
        registry.revision = registry.revision.saturating_add(1);
        registry.validate()?;
        ensure_private_dir(&self.root)?;
        let temporary = self.root.join("profiles.json.tmp");
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|_| CoreError::Config("profile registry could not encode".into()))?;
        let mut file = std::fs::File::create(&temporary)
            .map_err(|_| CoreError::Config("profile registry could not be written".into()))?;
        use std::io::Write;
        file.write_all(&bytes)
            .map_err(|_| CoreError::Config("profile registry could not be written".into()))?;
        file.sync_all()
            .map_err(|_| CoreError::Config("profile registry could not be synced".into()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| CoreError::Config("profile registry permissions failed".into()))?;
        }
        drop(file);
        std::fs::rename(&temporary, self.root.join(PROFILES_FILE))
            .map_err(|_| CoreError::Config("profile registry could not be installed".into()))?;
        Ok(())
    }

    fn has_legacy_state(&self, secrets: &Arc<dyn SecretStore>) -> CoreResult<bool> {
        if self.has_legacy_files() {
            return Ok(true);
        }
        if !self.root.is_dir() {
            return Ok(false);
        }
        match device::existing_device_id(secrets, &self.root) {
            Ok(identity) => Ok(identity.is_some()),
            Err(error) => Err(CoreError::from(error)),
        }
    }

    fn has_legacy_files(&self) -> bool {
        const MARKERS: &[&str] = &[
            "config.json",
            "cache.sqlite3",
            "cache.sqlite3-journal",
            "cache.sqlite3-wal",
            "cache.sqlite3-shm",
            "commands.sqlite",
            "unconfirmed-commands",
            "admission-revoked",
            "revocation-retention-warning",
            "pairing-epoch",
            "identity.lock",
            "admission.lock",
            "last-failure.json",
        ];
        MARKERS.iter().any(|name| self.root.join(name).exists())
    }

    fn clean_profile_secrets(
        &self,
        state_dir: &Path,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<()> {
        if !state_dir.is_dir() {
            return Ok(());
        }
        if let Some(_device_id) = device::existing_device_id(secrets, state_dir)? {
            device::forget(secrets, state_dir)?;
        }
        Ok(())
    }

    fn managed_dir(&self, id: &str) -> PathBuf {
        self.root.join(PROFILES_DIR).join(id)
    }
}

impl ProfilesRegistry {
    fn find_active(&self) -> CoreResult<&ProfileRecord> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active)
            .ok_or_else(|| CoreError::Config("active profile is unavailable".into()))
    }
}

fn ensure_production_profile(
    registry: &mut ProfilesRegistry,
    profiles: &Profiles,
) -> CoreResult<String> {
    if let Some(existing) = registry
        .profiles
        .iter()
        .find(|profile| profile.name == PRODUCTION_PROFILE_NAME)
    {
        return Ok(existing.id.clone());
    }
    let production = profiles.new_production_record()?;
    let id = production.id.clone();
    registry.profiles.push(production);
    Ok(id)
}

fn validate_profile_id(id: &str) -> CoreResult<()> {
    if id == LEGACY_PROFILE_ID {
        return Ok(());
    }
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreError::Config("invalid profile identity".into()));
    }
    Ok(())
}

fn validate_profile_name(name: &str) -> CoreResult<()> {
    if name.is_empty()
        || name.len() > 64
        || name.trim() != name
        || name
            .chars()
            .any(|value| value.is_control() || value.is_whitespace())
    {
        return Err(CoreError::Config("invalid profile name".into()));
    }
    Ok(())
}

fn require_managed_dir(state_dir: &Path) -> CoreResult<()> {
    let metadata = std::fs::symlink_metadata(state_dir).map_err(|_| {
        CoreError::Config(
            "profile state is missing; restore the profile directory or remove the profile".into(),
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(CoreError::Config(
            "profile state is not a plain directory".into(),
        ));
    }
    Ok(())
}

fn prepare_managed_dir(state_dir: &Path) -> CoreResult<()> {
    if let Some(parent) = state_dir.parent() {
        ensure_private_dir(parent)?;
    }
    if state_dir.exists() {
        let metadata = std::fs::symlink_metadata(state_dir)
            .map_err(|_| CoreError::Config("profile state could not be inspected".into()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CoreError::Config(
                "profile state is not a plain directory".into(),
            ));
        }
        return Ok(());
    }
    std::fs::create_dir_all(state_dir)
        .map_err(|_| CoreError::Config("profile state could not be created".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| CoreError::Config("profile state permissions failed".into()))?;
    }
    Ok(())
}

fn ensure_private_dir(directory: &Path) -> CoreResult<()> {
    if directory.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(directory)
        .map_err(|_| CoreError::Config("profile directory could not be created".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| CoreError::Config("profile directory permissions failed".into()))?;
    }
    Ok(())
}

fn remove_managed_dir(root: &Path, id: &str) -> CoreResult<()> {
    if id == LEGACY_PROFILE_ID || id.len() != 32 {
        return Err(CoreError::Config("invalid managed profile identity".into()));
    }
    let managed_root = root.join(PROFILES_DIR);
    let state_dir = managed_root.join(id);
    let metadata = match std::fs::symlink_metadata(&state_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(CoreError::Config(
                "profile state could not be inspected".into(),
            ))
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(CoreError::Config(
            "refusing to remove a profile state that is not a plain directory".into(),
        ));
    }
    std::fs::remove_dir_all(&state_dir)
        .map_err(|_| CoreError::Config("profile state could not be removed".into()))?;
    if managed_root
        .read_dir()
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(&managed_root);
    }
    Ok(())
}

fn acquire_registry_lock(root: &Path) -> CoreResult<std::fs::File> {
    ensure_private_dir(root)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("profiles.lock"))
        .map_err(|_| CoreError::Config("profile registry lock could not be opened".into()))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    loop {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(file),
            Err(fs4::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(fs4::TryLockError::WouldBlock) => {
                return Err(CoreError::Conflict(
                    "profile registry is busy; retry".into(),
                ))
            }
            Err(fs4::TryLockError::Error(_)) => {
                return Err(CoreError::Config(
                    "profile registry lock could not be acquired".into(),
                ))
            }
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
