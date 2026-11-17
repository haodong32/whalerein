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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub(crate) use activation::ActivationGuard;

use crate::account::{
    environment_authority_fingerprint, AccountBinding, CustomServiceInput, Environment,
    ProfileMode, ServiceDescriptor, ServiceKind,
};
use crate::api::{CoreError, CoreResult};
use crate::configuration::{
    ConfigurationError, DefinitionSource, EnvironmentDefinition, EnvironmentResolution,
};
use crate::security::device;
use crate::security::SecretStore;

pub const PROFILES_SCHEMA: &str = "whalerein/v1/profiles";
pub const PROFILES_FILE: &str = "profiles.json";
pub const PROFILES_DIR: &str = "profiles";
pub const LEGACY_PROFILE_ID: &str = "default";
pub const PRODUCTION_PROFILE_NAME: &str = "official-production";
pub const LOCAL_PROFILE_NAME: &str = "official-local";
pub const STAGING_PROFILE_NAME: &str = "official-staging";

/// Durable, non-secret profile registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilesRegistry {
    pub schema: String,
    pub revision: u64,
    pub activation_generation: u64,
    pub active: String,
    /// Environment-scoped identity selection. This selects an identity inside
    /// the environment named by the key; it never selects an environment and
    /// never overrides the configuration file default.
    #[serde(default)]
    pub active_by_environment: BTreeMap<String, String>,
    pub profiles: Vec<ProfileRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRecord {
    pub id: String,
    pub name: String,
    pub mode: ProfileMode,
    pub legacy: bool,
    /// Environment this identity belongs to. Old registries omit it and infer
    /// the value from the recorded service; unknown values are preserved.
    #[serde(default)]
    pub environment: String,
    /// Stable authority fingerprint of the environment definition this profile
    /// was created for. Labels are not part of it and never rotate keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
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
            active_by_environment: BTreeMap::new(),
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
            validate_environment_key(&profile.environment)?;
            if !ids.insert(profile.id.as_str()) {
                return Err(CoreError::Config("duplicate profile id".into()));
            }
            if !names.insert(profile.name.to_ascii_lowercase()) {
                return Err(CoreError::Config("duplicate profile name".into()));
            }
            if let Some(authority) = &profile.authority {
                if authority.len() != 64 || !authority.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(CoreError::Config(
                        "profile authority fingerprint is invalid".into(),
                    ));
                }
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
                let expected_environment = match service.kind {
                    ServiceKind::OfficialProduction => Some("production"),
                    ServiceKind::OfficialStaging => Some("staging"),
                    ServiceKind::OfficialLocal => Some("local"),
                    ServiceKind::Custom => None,
                };
                if expected_environment.is_some_and(|value| profile.environment != value) {
                    return Err(CoreError::Config(
                        "official profile environment does not match its service".into(),
                    ));
                }
                if (profile.name == PRODUCTION_PROFILE_NAME
                    && service.kind != ServiceKind::OfficialProduction)
                    || (profile.name == STAGING_PROFILE_NAME
                        && service.kind != ServiceKind::OfficialStaging)
                    || (profile.name == LOCAL_PROFILE_NAME
                        && service.kind != ServiceKind::OfficialLocal)
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
        for (environment, id) in &self.active_by_environment {
            validate_environment_key(environment)?;
            let profile = self
                .profiles
                .iter()
                .find(|profile| &profile.id == id)
                .ok_or_else(|| {
                    CoreError::Config("environment-scoped active identity is not registered".into())
                })?;
            if &profile.environment != environment {
                return Err(CoreError::Config(
                    "environment-scoped active identity belongs to another environment".into(),
                ));
            }
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
    /// Canonical environment key this identity is scoped to.
    pub environment_id: String,
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
            environment_id: profile.environment.clone(),
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
            let profile = legacy_record();
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
                let scoped = registry.active_by_environment.get(&profile.environment);
                let active = scoped.map_or(registry.active == profile.id, |id| id == &profile.id);
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
            let profile = legacy_record();
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

    /// Select one official environment using the existing atomic profile activation.
    pub fn select_environment(
        &self,
        environment: Environment,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<ActivationOutcome> {
        let (name, descriptor) = match environment {
            Environment::Production => (
                PRODUCTION_PROFILE_NAME,
                ServiceDescriptor::official_production(),
            ),
            Environment::Staging => (STAGING_PROFILE_NAME, ServiceDescriptor::official_staging()),
            Environment::Local => (LOCAL_PROFILE_NAME, ServiceDescriptor::official_local()),
            _ => {
                return Err(CoreError::InvalidInput(
                    "expected an official environment".into(),
                ))
            }
        };
        let profiles = self.list(secrets, true)?;
        if let Some(profile) = profiles.iter().find(|p| p.name == name) {
            if profile.service.as_ref() != Some(&descriptor) {
                return Err(CoreError::Config(
                    "official profile authority mismatch".into(),
                ));
            }
        } else {
            if environment == Environment::Production {
                let _guard = acquire_registry_lock(&self.root)?;
                let mut registry = self.load_or_bootstrap(secrets)?;
                ensure_production_profile(&mut registry, self)?;
                self.save(&mut registry)?;
            } else {
                self.add_profile(name, ProfileMode::Account, descriptor, secrets)?;
            }
        }
        self.activate(name)
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

    /// Resolve the identity profile active within one resolved environment.
    ///
    /// `create == false` returns an explicit `None` when no identity exists for
    /// that environment; it never invents a profile. `create == true` may add
    /// one deterministic profile for the environment but never activates it:
    /// no environment-scoped selection is written, so the identity is inactive
    /// until an explicit `use_profile_in_environment`.
    pub fn resolve_environment_identity(
        &self,
        resolution: &EnvironmentResolution,
        secrets: &Arc<dyn SecretStore>,
        create: bool,
    ) -> CoreResult<Option<ProfileSummary>> {
        let environment = resolution.environment.as_str();
        let expected = expected_service(resolution)?;
        let fingerprint = environment_authority_fingerprint(&resolution.definition);
        let mode = environment_profile_mode(&resolution.definition)?;

        if create {
            let _guard = acquire_registry_lock(&self.root)?;
            let registry_exists = self.root.join(PROFILES_FILE).is_file();
            let mut registry = self.load_or_bootstrap(secrets)?;
            check_environment_bindings(&registry, environment, expected.as_ref(), &fingerprint)?;
            if let Some((profile, active)) = selected_identity(&registry, environment) {
                check_environment_authority(profile, environment, expected.as_ref(), &fingerprint)?;
                if !profile.legacy {
                    require_managed_dir(&self.managed_dir(&profile.id))?;
                }
                let summary = ProfileSummary::from_record(profile, active);
                if !registry_exists {
                    self.save(&mut registry)?;
                }
                return Ok(Some(summary));
            }
            if environment_has_profiles(&registry, environment) {
                return Ok(None);
            }
            let name = deterministic_profile_name(&registry, environment);
            let summary = self.create_environment_profile(
                &mut registry,
                environment,
                name,
                mode,
                expected,
                Some(fingerprint),
            )?;
            self.save(&mut registry)?;
            return Ok(Some(summary));
        }

        if let Some(registry) = self.load()? {
            check_environment_bindings(&registry, environment, expected.as_ref(), &fingerprint)?;
            let Some((profile, active)) = selected_identity(&registry, environment) else {
                return Ok(None);
            };
            check_environment_authority(profile, environment, expected.as_ref(), &fingerprint)?;
            if !profile.legacy {
                require_managed_dir(&self.managed_dir(&profile.id))?;
            }
            return Ok(Some(ProfileSummary::from_record(profile, active)));
        }
        if self.has_legacy_state(secrets)? {
            let legacy = legacy_record();
            if legacy.environment == environment {
                return Ok(Some(ProfileSummary::from_record(&legacy, true)));
            }
        }
        Ok(None)
    }

    /// Add another identity within the same environment. Existing profiles,
    /// state directories and SecretStore keys are preserved untouched.
    pub fn add_environment_profile(
        &self,
        resolution: &EnvironmentResolution,
        label: &str,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<ProfileSummary> {
        validate_profile_name(label)?;
        let environment = resolution.environment.as_str();
        let expected = expected_service(resolution)?;
        let fingerprint = environment_authority_fingerprint(&resolution.definition);
        let mode = environment_profile_mode(&resolution.definition)?;
        let _guard = acquire_registry_lock(&self.root)?;
        let mut registry = self.load_or_bootstrap(secrets)?;
        if registry
            .profiles
            .iter()
            .any(|profile| profile.name.eq_ignore_ascii_case(label))
        {
            return Err(CoreError::Conflict(format!(
                "profile {label} already exists"
            )));
        }
        let summary = self.create_environment_profile(
            &mut registry,
            environment,
            label.to_owned(),
            mode,
            expected,
            Some(fingerprint),
        )?;
        self.save(&mut registry)?;
        Ok(summary)
    }

    /// Activate one identity inside its own environment. It never changes the
    /// saved/global environment default, never touches another environment's
    /// selection, and reuses the existing exclusive activation fence.
    pub fn use_profile_in_environment(
        &self,
        environment: &EnvironmentResolution,
        name: &str,
    ) -> CoreResult<ActivationOutcome> {
        let target = environment.environment.as_str();
        let _guard = ActivationGuard::exclusive(&self.root)?;
        let mut registry = self
            .load()?
            .ok_or_else(|| CoreError::NotFound("no local profiles are registered".into()))?;
        let profile = registry.find(name)?.clone();
        if profile.environment != target {
            return Err(CoreError::Conflict(format!(
                "profile {name} belongs to environment {}, not {target}; select it within its own environment",
                profile.environment
            )));
        }
        let expected = expected_service(environment)?;
        let fingerprint = environment_authority_fingerprint(&environment.definition);
        check_environment_authority(&profile, target, expected.as_ref(), &fingerprint)?;
        if !profile.legacy {
            require_managed_dir(&self.managed_dir(&profile.id))?;
        }
        let previous = registry.active_by_environment.get(target).cloned();
        if previous.as_deref() == Some(profile.id.as_str()) {
            return Ok(ActivationOutcome {
                profile: ProfileSummary::from_record(&profile, true),
                previous,
                rollback_warning: None,
            });
        }
        registry
            .active_by_environment
            .insert(target.to_string(), profile.id.clone());
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
            previous,
            rollback_warning,
        })
    }

    /// List identities scoped to one environment. Testing-only official
    /// identities stay hidden unless active or explicitly requested.
    pub fn environment_profiles(
        &self,
        environment: &str,
        include_testing: bool,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<Vec<ProfileSummary>> {
        validate_environment_key(environment)?;
        if let Some(registry) = self.load()? {
            let scoped = registry.active_by_environment.get(environment);
            let mut summaries = Vec::new();
            for profile in registry
                .profiles
                .iter()
                .filter(|profile| profile.environment == environment)
            {
                let active = scoped.map_or(registry.active == profile.id, |id| id == &profile.id);
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
            let legacy = legacy_record();
            if legacy.environment == environment {
                return Ok(vec![ProfileSummary::from_record(&legacy, true)]);
            }
        }
        Ok(Vec::new())
    }

    fn create_environment_profile(
        &self,
        registry: &mut ProfilesRegistry,
        environment: &str,
        name: String,
        mode: ProfileMode,
        service: Option<ServiceDescriptor>,
        authority: Option<String>,
    ) -> CoreResult<ProfileSummary> {
        let id = crate::util::random_id(16);
        let state_dir = self.managed_dir(&id);
        prepare_managed_dir(&state_dir)?;
        let record = ProfileRecord {
            id,
            name,
            mode,
            legacy: false,
            environment: environment.to_owned(),
            authority,
            service,
            account: None,
            created_unix_ms: now_unix_ms(),
        };
        let summary = ProfileSummary::from_record(&record, false);
        registry.profiles.push(record);
        Ok(summary)
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
        let environment = environment_for_service(Some(&service));
        let record = ProfileRecord {
            id,
            name: name.to_owned(),
            mode,
            legacy: false,
            environment,
            authority: None,
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
        registry
            .active_by_environment
            .insert(profile.environment.clone(), profile.id.clone());
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
        registry
            .active_by_environment
            .retain(|_, id| id != &record.id);
        if registry.active == record.id {
            let production = ensure_production_profile(&mut registry, self)?;
            registry.active = production.clone();
            registry
                .active_by_environment
                .insert("production".into(), production);
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
            return self.context_for(&profile);
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
        let mut registry: ProfilesRegistry = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Config("profile registry is not valid JSON".into()))?;
        // Backward-compatible read: old registries omit `environment`. Infer it
        // from the recorded authority; a present unknown value is preserved.
        for profile in &mut registry.profiles {
            if profile.environment.is_empty() {
                profile.environment = environment_for_service(profile.service.as_ref());
            }
        }
        registry.validate()?;
        Ok(Some(registry))
    }

    fn load_or_bootstrap(&self, secrets: &Arc<dyn SecretStore>) -> CoreResult<ProfilesRegistry> {
        if let Some(registry) = self.load()? {
            return Ok(registry);
        }
        if self.has_legacy_state(secrets)? {
            let legacy = legacy_record();
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
            environment: "production".into(),
            authority: None,
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

impl Profiles {
    /// Build the stable profile context for one registry record.
    fn context_for(&self, profile: &ProfileRecord) -> CoreResult<ProfileContext> {
        let state_dir = if profile.legacy {
            self.root.clone()
        } else {
            let state_dir = self.managed_dir(&profile.id);
            require_managed_dir(&state_dir)?;
            state_dir
        };
        Ok(ProfileContext {
            root: self.root.clone(),
            state_dir,
            id: Some(profile.id.clone()),
            name: Some(profile.name.clone()),
            mode: profile.mode,
            service: profile.service.clone(),
            legacy: profile.legacy,
            virtual_default: false,
        })
    }

    /// Resolve the active identity inside one selected environment.
    ///
    /// Environment-scoped selection wins; the legacy global active profile is
    /// used only when it belongs to the same environment or is a Standalone
    /// identity (Standalone pairing is not an account-environment selection).
    /// When no identity exists in the environment the result is an explicit
    /// no-profile error, never an invented device. A fresh installation with
    /// no state keeps the historical virtual production default so unpaired
    /// inspection commands still work.
    pub fn resolve_active_in_environment(
        &self,
        environment: &str,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<ProfileContext> {
        validate_environment_key(environment)?;
        if let Some(registry) = self.load()? {
            if let Some(id) = registry.active_by_environment.get(environment) {
                let profile = registry
                    .profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .ok_or_else(|| {
                        CoreError::Config(
                            "environment-scoped active identity is not registered".into(),
                        )
                    })?;
                return self.context_for(profile);
            }
            let active = registry.find_active()?.clone();
            if active.environment == environment || active.environment == "standalone" {
                return self.context_for(&active);
            }
            return Err(CoreError::NotFound(format!(
                "no identity profile for environment {environment}; add one with profile add"
            )));
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
        if environment == "production" {
            return Ok(ProfileContext {
                root: self.root.clone(),
                state_dir: self.root.clone(),
                id: None,
                name: None,
                mode: ProfileMode::Standalone,
                service: Some(ServiceDescriptor::official_production()),
                legacy: false,
                virtual_default: true,
            });
        }
        Err(CoreError::NotFound(format!(
            "no identity profile for environment {environment}; add one with profile add"
        )))
    }
}

fn legacy_record() -> ProfileRecord {
    ProfileRecord {
        id: LEGACY_PROFILE_ID.into(),
        name: LEGACY_PROFILE_ID.into(),
        mode: ProfileMode::Standalone,
        legacy: true,
        environment: "standalone".into(),
        authority: None,
        service: None,
        account: None,
        created_unix_ms: 0,
    }
}

/// Legacy inference for a registry record without an `environment` field.
fn environment_for_service(service: Option<&ServiceDescriptor>) -> String {
    match service.map(|service| service.kind) {
        Some(ServiceKind::OfficialProduction) => "production",
        Some(ServiceKind::OfficialStaging) => "staging",
        Some(ServiceKind::OfficialLocal) => "local",
        Some(ServiceKind::Custom) => "custom",
        None => "standalone",
    }
    .to_owned()
}

fn validate_environment_key(value: &str) -> CoreResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(CoreError::Config(
            "invalid profile environment identity".into(),
        ))
    }
}

/// Environment-scoped identity selection, then legacy global selection when it
/// belongs to the same environment. A single unselected identity still resolves
/// (as inactive) so creation followed by read does not require an activation.
fn selected_identity<'a>(
    registry: &'a ProfilesRegistry,
    environment: &str,
) -> Option<(&'a ProfileRecord, bool)> {
    if let Some(id) = registry.active_by_environment.get(environment) {
        if let Some(profile) = registry.profiles.iter().find(|profile| &profile.id == id) {
            return Some((profile, true));
        }
    }
    let active = registry
        .profiles
        .iter()
        .find(|profile| profile.id == registry.active)?;
    if active.environment == environment {
        return Some((active, true));
    }
    let mut scoped = registry
        .profiles
        .iter()
        .filter(|profile| profile.environment == environment);
    let only = scoped.next()?;
    scoped.next().is_none().then_some((only, false))
}

fn environment_has_profiles(registry: &ProfilesRegistry, environment: &str) -> bool {
    registry
        .profiles
        .iter()
        .any(|profile| profile.environment == environment)
}

fn environment_profile_mode(definition: &EnvironmentDefinition) -> CoreResult<ProfileMode> {
    definition
        .mode
        .map(ProfileMode::from_environment_mode)
        .ok_or_else(|| {
            CoreError::from(ConfigurationError::InvalidMode {
                environment: definition.id.clone(),
            })
        })
}

fn expected_service(resolution: &EnvironmentResolution) -> CoreResult<Option<ServiceDescriptor>> {
    ServiceDescriptor::from_environment_definition(
        &resolution.definition,
        resolution.definition_source == DefinitionSource::Builtin,
    )
}

fn check_environment_bindings(
    registry: &ProfilesRegistry,
    environment: &str,
    expected: Option<&ServiceDescriptor>,
    fingerprint: &str,
) -> CoreResult<()> {
    for profile in registry
        .profiles
        .iter()
        .filter(|profile| profile.environment == environment)
    {
        check_environment_authority(profile, environment, expected, fingerprint)?;
    }
    Ok(())
}

fn authority_mismatch(environment: &str) -> CoreError {
    CoreError::from(ConfigurationError::AuthorityOverride {
        environment: environment.to_owned(),
    })
}

fn check_environment_authority(
    profile: &ProfileRecord,
    environment: &str,
    expected: Option<&ServiceDescriptor>,
    fingerprint: &str,
) -> CoreResult<()> {
    if let Some(recorded) = &profile.authority {
        if recorded != fingerprint {
            return Err(authority_mismatch(environment));
        }
    }
    let descriptor_matches = match (&profile.service, expected) {
        (Some(recorded), Some(expected)) => recorded == expected,
        (None, None) => true,
        _ => false,
    };
    if !descriptor_matches {
        return Err(authority_mismatch(environment));
    }
    Ok(())
}

/// First profile created for an environment gets a stable, environment-derived
/// name. Additional identities must be added with an explicit label.
fn deterministic_profile_name(registry: &ProfilesRegistry, environment: &str) -> String {
    let base = match environment {
        "production" => PRODUCTION_PROFILE_NAME,
        "staging" => STAGING_PROFILE_NAME,
        "local" => LOCAL_PROFILE_NAME,
        other => other,
    };
    if !registry
        .profiles
        .iter()
        .any(|profile| profile.name.eq_ignore_ascii_case(base))
    {
        return base.to_owned();
    }
    let mut counter = 2u32;
    loop {
        let candidate = format!("{base}-{counter}");
        if !registry
            .profiles
            .iter()
            .any(|profile| profile.name.eq_ignore_ascii_case(&candidate))
        {
            return candidate;
        }
        counter += 1;
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
