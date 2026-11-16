use std::sync::Arc;

use super::*;
use crate::account::CustomServiceInput;
use crate::security::{device, MemorySecretStore, SecretStore};

fn memory() -> Arc<dyn SecretStore> {
    Arc::new(MemorySecretStore::new())
}

fn profiles_at(dir: &tempfile::TempDir) -> Profiles {
    Profiles::new(dir.path())
}

#[test]
fn fresh_selection_defaults_to_official_production_without_writing_state() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    let selection = profiles.current(&secrets).unwrap();
    assert!(!selection.registry_present);
    assert!(selection.active.is_none());
    assert_eq!(
        selection
            .default_service
            .as_ref()
            .map(ServiceDescriptor::kind_name),
        Some("official-production")
    );
    assert!(selection.standalone_available);
    assert!(!dir.path().join(PROFILES_FILE).exists());
    assert!(!dir.path().join(PROFILES_DIR).exists());
}

#[test]
fn legacy_config_and_identity_stay_standalone_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join("config.json"), b"{}").unwrap();
    let profiles = profiles_at(&dir);
    let selection = profiles.current(&secrets).unwrap();
    assert!(!selection.registry_present);
    let active = selection.active.unwrap();
    assert_eq!(active.name, LEGACY_PROFILE_ID);
    assert!(active.legacy);
    assert!(active.is_standalone());
    assert_eq!(active.environment, Environment::Standalone);
    assert!(profiles
        .resolve_active(&secrets)
        .unwrap()
        .runtime_state_is_root());
}

trait RootState {
    fn runtime_state_is_root(&self) -> bool;
}

impl RootState for ProfileContext {
    fn runtime_state_is_root(&self) -> bool {
        self.state_dir == self.root
    }
}

#[test]
fn pending_identity_alone_is_preserved_as_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join("config.json"), b"{}").unwrap();
    let identity = device::load_or_create(&secrets, dir.path(), None).unwrap();
    std::fs::remove_file(dir.path().join("config.json")).unwrap();
    let profiles = profiles_at(&dir);
    let selection = profiles.current(&secrets).unwrap();
    assert_eq!(selection.active.unwrap().name, LEGACY_PROFILE_ID);
    assert_eq!(
        device::existing_device_id(&secrets, dir.path())
            .unwrap()
            .as_deref(),
        Some(identity.device_id.as_str())
    );
}

#[test]
fn staging_add_requires_explicit_testing_and_never_activates() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    assert!(matches!(
        profiles.add_official_staging("official-staging", false, &secrets),
        Err(CoreError::Unsupported)
    ));
    let added = profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    assert!(added.testing);
    assert!(!added.active);
    let current = profiles.current(&secrets).unwrap();
    assert_eq!(
        current.active.as_ref().map(|value| value.name.as_str()),
        Some(PRODUCTION_PROFILE_NAME)
    );
    // Ordinary setup hides the staging profile; the active profile is always shown.
    let visible = profiles.list(&secrets, false).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].name, PRODUCTION_PROFILE_NAME);
    let all = profiles.list(&secrets, true).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|value| value.name == "official-staging"));
    assert!(
        all.iter()
            .any(|value| value.name == "official-staging"
                && value.environment == Environment::Staging)
    );
}

#[test]
fn activation_labels_staging_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    let outcome = profiles.activate("official-staging").unwrap();
    assert_eq!(outcome.profile.environment, Environment::Staging);
    assert_eq!(outcome.profile.name, "official-staging");
    assert!(outcome.profile.testing);
    assert!(outcome.rollback_warning.is_none());
    drop(profiles);
    let reopened = profiles_at(&dir);
    let current = reopened.current(&secrets).unwrap();
    assert_eq!(
        current.active.as_ref().map(|value| value.name.as_str()),
        Some("official-staging")
    );
    assert_eq!(current.active.unwrap().environment, Environment::Staging);
    let staging = reopened
        .list(&secrets, false)
        .unwrap()
        .into_iter()
        .find(|value| value.name == "official-staging")
        .unwrap();
    assert!(staging.active);
}

#[test]
fn activation_conflicts_while_another_client_holds_the_lease() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    let guard = ActivationGuard::shared(dir.path()).unwrap();
    assert!(matches!(
        profiles.activate("official-staging"),
        Err(CoreError::Conflict(_))
    ));
    drop(guard);
    assert!(profiles.activate("official-staging").is_ok());
}

#[test]
fn competing_switches_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    let guard = ActivationGuard::exclusive(dir.path()).unwrap();
    assert!(matches!(
        profiles.activate("official-staging"),
        Err(CoreError::Conflict(_))
    ));
    drop(guard);
    assert!(profiles.activate("official-staging").is_ok());
}

#[test]
fn removing_active_staging_clears_only_that_profile_and_returns_production() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    let current = profiles.current(&secrets).unwrap();
    let staging_dir = profiles.managed_dir(&current.active.as_ref().unwrap().id);
    let identity = device::load_or_create(&secrets, &staging_dir, None).unwrap();
    let device_id = identity.device_id.clone();

    let removed = profiles.remove("official-staging", &secrets).unwrap();
    assert_eq!(removed.remote_revocation, "not_attempted");
    assert!(!staging_dir.exists());
    assert!(!dir
        .path()
        .join(PROFILES_DIR)
        .join(&current.active.unwrap().id)
        .exists());
    assert_eq!(
        removed
            .active
            .active
            .as_ref()
            .map(|value| value.name.as_str()),
        Some(PRODUCTION_PROFILE_NAME)
    );
    assert!(secrets
        .load(&crate::security::keys::device_private_key(&device_id))
        .unwrap()
        .is_none());
    // The production profile is materialized and the staging registration is gone.
    let all = profiles.list(&secrets, true).unwrap();
    assert!(!all.iter().any(|value| value.name == "official-staging"));
    assert!(all
        .iter()
        .any(|value| value.name == PRODUCTION_PROFILE_NAME));
}

#[test]
fn legacy_profile_state_cannot_be_removed_here() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join("config.json"), b"{}").unwrap();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    assert!(matches!(
        profiles.remove(LEGACY_PROFILE_ID, &secrets),
        Err(CoreError::Conflict(_))
    ));
    assert!(dir.path().join("config.json").exists());
}

#[test]
fn custom_service_is_validated_and_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    assert!(profiles
        .add_custom(
            "corp",
            CustomServiceInput {
                api_origin: "https://api.corp.example".into(),
                issuer: "https://api.corp.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            },
            &secrets
        )
        .is_err());
    let added = profiles
        .add_custom(
            "corp",
            CustomServiceInput {
                api_origin: "https://api.corp.example".into(),
                issuer: "https://id.corp.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: Some("https://access.corp.example".into()),
            },
            &secrets,
        )
        .unwrap();
    assert_eq!(added.environment, Environment::Custom);
    assert_eq!(
        added.service.unwrap().issuer_display(),
        "https://id.corp.example"
    );
    profiles.activate("corp").unwrap();
    let current = profiles.current(&secrets).unwrap();
    assert_eq!(
        current.active.unwrap().service.unwrap().issuer_display(),
        "https://id.corp.example"
    );
}

#[test]
fn activation_reports_rollback_warning_once_managed_state_coexists_with_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join("config.json"), b"{}").unwrap();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    let outcome = profiles.activate("official-staging").unwrap();
    assert!(outcome.rollback_warning.is_some());
}

#[test]
fn corrupted_or_future_registry_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join(PROFILES_FILE), b"not json").unwrap();
    let profiles = profiles_at(&dir);
    assert!(profiles.current(&secrets).is_err());
    assert!(profiles
        .add_official_staging("official-staging", true, &secrets)
        .is_err());
    assert!(std::fs::read(dir.path().join(PROFILES_FILE))
        .unwrap()
        .starts_with(b"not json"));

    let future = r#"{"schema":"whalerein/v2/profiles","revision":1,"activation_generation":0,"active":"a","profiles":[]}"#;
    std::fs::write(dir.path().join(PROFILES_FILE), future).unwrap();
    assert!(profiles.current(&secrets).is_err());
}

#[test]
fn interrupted_registry_write_is_ignored_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join("profiles.json.tmp"), b"partial").unwrap();
    let profiles = profiles_at(&dir);
    let selection = profiles.current(&secrets).unwrap();
    assert!(selection.active.is_none());
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    assert!(dir.path().join(PROFILES_FILE).exists());
    assert!(!dir.path().join("profiles.json.tmp").exists());
}

#[test]
fn account_bound_profile_refuses_a_different_service_authority() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_custom(
            "corp",
            CustomServiceInput {
                api_origin: "https://api.corp.example".into(),
                issuer: "https://id.corp.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            },
            &secrets,
        )
        .unwrap();
    let registry = profiles.load().unwrap().unwrap();
    let mut bound = registry.clone();
    let record = bound.find_mut("corp").unwrap();
    record.account = Some(AccountBinding {
        subject: "subject-1".into(),
        workspace: "workspace-1".into(),
        issuer: "https://id.corp.example".into(),
        bound_unix_ms: 0,
    });
    bound.validate().unwrap();
    bound.find_mut("corp").unwrap().service = Some(
        ServiceDescriptor::custom(CustomServiceInput {
            api_origin: "https://api.other.example".into(),
            issuer: "https://id.other.example".into(),
            native_client_id: "whalerein-cli".into(),
            access_url: None,
        })
        .unwrap(),
    );
    assert!(bound.validate().is_err());
}

#[test]
fn managed_profiles_get_distinct_identity_directories_and_device_ids() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    let first = profiles
        .add_custom(
            "first",
            CustomServiceInput {
                api_origin: "https://api.one.example".into(),
                issuer: "https://id.one.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            },
            &secrets,
        )
        .unwrap();
    let second = profiles
        .add_custom(
            "second",
            CustomServiceInput {
                api_origin: "https://api.two.example".into(),
                issuer: "https://id.two.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            },
            &secrets,
        )
        .unwrap();
    let first_dir = profiles.managed_dir(&first.id);
    let second_dir = profiles.managed_dir(&second.id);
    assert_ne!(first_dir, second_dir);
    let first_identity = device::load_or_create(&secrets, &first_dir, None).unwrap();
    let second_identity = device::load_or_create(&secrets, &second_dir, None).unwrap();
    assert_ne!(first_identity.device_id, second_identity.device_id);
    profiles.activate("second").unwrap();
    assert_eq!(
        profiles.resolve_active(&secrets).unwrap().state_dir,
        second_dir
    );
}

#[test]
fn core_uses_the_selected_managed_profile_directory() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    let staging_id = profiles.current(&secrets).unwrap().active.unwrap().id;
    let core = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    assert_eq!(
        core.profile()
            .service
            .as_ref()
            .map(ServiceDescriptor::kind_name),
        Some("official-staging")
    );
    assert_eq!(
        core.data_dir(),
        dir.path().join(PROFILES_DIR).join(staging_id)
    );
    assert_eq!(core.profile().mode, ProfileMode::Account);
}

#[test]
fn fresh_core_startup_is_standalone_and_creates_no_account_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemorySecretStore::new());
    let secrets: Arc<dyn SecretStore> = store.clone();
    let core = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    let profile = core.profile();
    assert!(profile.virtual_default);
    assert_eq!(profile.mode, ProfileMode::Standalone);
    assert_eq!(
        profile.service.as_ref().map(ServiceDescriptor::kind_name),
        Some("official-production")
    );
    assert!(!core.is_paired());
    assert_eq!(core.data_dir(), dir.path());
    assert!(!dir.path().join(PROFILES_FILE).exists());
    assert!(!dir.path().join(PROFILES_DIR).exists());
    assert!(store.is_empty());
}

#[test]
fn failed_mutations_never_change_the_active_selection() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    let before = profiles.current(&secrets).unwrap().active.unwrap().id;
    assert!(profiles.activate("missing").is_err());
    assert_eq!(
        profiles.current(&secrets).unwrap().active.unwrap().id,
        before
    );
    assert!(profiles
        .add_official_staging("official-staging", true, &secrets)
        .is_err());
    assert_eq!(
        profiles.current(&secrets).unwrap().active.unwrap().id,
        before
    );
}

#[test]
fn switching_preserves_pending_pairing_and_uncertain_records() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    std::fs::write(dir.path().join("config.json"), b"{}").unwrap();
    let identity = device::load_or_create(&secrets, dir.path(), None).unwrap();
    let device_id = identity.device_id.clone();
    let pending_key = crate::security::keys::pending_pairing(&device_id);
    let pending = b"redacted-pending-finish".to_vec();
    secrets.save(&pending_key, &pending).unwrap();
    let uncertain = dir
        .path()
        .join("unconfirmed-commands")
        .join("a".repeat(32) + ".json");
    std::fs::create_dir_all(uncertain.parent().unwrap()).unwrap();
    std::fs::write(&uncertain, b"redacted-command-identity").unwrap();

    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    assert_eq!(secrets.load(&pending_key).unwrap(), Some(pending));
    assert_eq!(
        std::fs::read(&uncertain).unwrap(),
        b"redacted-command-identity"
    );
    let resolved = profiles.resolve_active(&secrets).unwrap();
    assert_eq!(resolved.mode, ProfileMode::Account);
    assert_eq!(
        device::existing_device_id(&secrets, dir.path())
            .unwrap()
            .as_deref(),
        Some(device_id.as_str())
    );
}

#[test]
fn switching_does_not_touch_managed_profile_state() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    let staging_id = profiles.current(&secrets).unwrap().active.unwrap().id;
    let staging_dir = profiles.managed_dir(&staging_id);
    let marker = staging_dir.join("state-marker");
    std::fs::write(&marker, b"unchanged").unwrap();
    profiles.activate(PRODUCTION_PROFILE_NAME).unwrap();
    assert_eq!(std::fs::read(&marker).unwrap(), b"unchanged");
    profiles.activate("official-staging").unwrap();
    assert_eq!(std::fs::read(&marker).unwrap(), b"unchanged");
}

#[test]
fn missing_managed_state_blocks_activation_and_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    let staging_id = profiles.current(&secrets).unwrap().active.unwrap().id;
    let staging_dir = profiles.managed_dir(&staging_id);
    std::fs::remove_dir_all(&staging_dir).unwrap();

    assert!(profiles.resolve_active(&secrets).is_err());
    assert!(profiles.activate("official-staging").is_err());
    // The missing profile can still be removed explicitly; local cleanup does
    // not fabricate a replacement identity.
    let removed = profiles.remove("official-staging", &secrets).unwrap();
    assert_eq!(
        removed
            .active
            .active
            .as_ref()
            .map(|value| value.name.as_str()),
        Some(PRODUCTION_PROFILE_NAME)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_profile_state_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();
    profiles.activate("official-staging").unwrap();
    let staging_id = profiles.current(&secrets).unwrap().active.unwrap().id;
    let staging_dir = profiles.managed_dir(&staging_id);
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_dir_all(&staging_dir).unwrap();
    std::os::unix::fs::symlink(outside.path(), &staging_dir).unwrap();
    assert!(profiles.resolve_active(&secrets).is_err());
}

#[tokio::test]
async fn managed_profiles_pair_with_independent_identities_and_state() {
    use crate::api::test_support::FakeAccess;
    use crate::api::PairingDecision;

    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    for (name, api, issuer) in [
        ("first", "https://api.one.example", "https://id.one.example"),
        (
            "second",
            "https://api.two.example",
            "https://id.two.example",
        ),
    ] {
        profiles
            .add_custom(
                name,
                CustomServiceInput {
                    api_origin: api.into(),
                    issuer: issuer.into(),
                    native_client_id: "whalerein-cli".into(),
                    access_url: None,
                },
                &secrets,
            )
            .unwrap();
    }
    let access = FakeAccess::start().await;

    profiles.activate("first").unwrap();
    let first_dir = profiles.resolve_active(&secrets).unwrap().state_dir;
    let first = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let first_outcome = first
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_eq!(first.profile().name.as_deref(), Some("first"));
    assert_eq!(first.data_dir(), first_dir);
    drop(first);

    profiles.activate("second").unwrap();
    let second_dir = profiles.resolve_active(&secrets).unwrap().state_dir;
    let second = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    let second_outcome = second
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert_ne!(first_outcome.device_id, second_outcome.device_id);
    assert_eq!(second.profile().name.as_deref(), Some("second"));
    assert_eq!(second.data_dir(), second_dir);
    assert_ne!(first_dir, second_dir);
    drop(second);

    profiles.activate("first").unwrap();
    let reopened = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    assert_eq!(reopened.profile().name.as_deref(), Some("first"));
    assert!(reopened.is_paired());
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(first_dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["device_id"], first_outcome.device_id);
}

#[tokio::test]
async fn active_watch_blocks_profile_switch_until_stopped() {
    use crate::api::test_support::FakeAccess;
    use crate::api::PairingDecision;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    let access = FakeAccess::start().await;
    let core = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    profiles
        .add_official_staging("official-staging", true, &secrets)
        .unwrap();

    let key = core.session_key("session-1").await.unwrap();
    let watching = core.clone();
    let handle = tokio::spawn(async move {
        let _ = watching
            .watch_session(&key, Some(Duration::from_secs(2)), |_| true)
            .await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!handle.is_finished(), "watch ended before the switch check");
    assert!(matches!(
        profiles.activate("official-staging"),
        Err(CoreError::Conflict(_))
    ));
    handle.abort();
    let _ = handle.await;
    drop(core);
    assert!(profiles.activate("official-staging").is_ok());
}

#[test]
fn existing_standalone_core_startup_keeps_the_legacy_profile_active() {
    use crate::api::config::{ConfigStore, PersistedConfig};

    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let config = PersistedConfig::new(
        "https://access.test".into(),
        None,
        "h-1".into(),
        "d-1".into(),
    );
    ConfigStore::new(dir.path()).save(&config).unwrap();
    let core = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    let profile = core.profile();
    assert!(profile.legacy);
    assert_eq!(profile.mode, ProfileMode::Standalone);
    assert_eq!(profile.name.as_deref(), Some(LEGACY_PROFILE_ID));
    assert_eq!(core.data_dir(), dir.path());
    assert!(core.is_paired());
    assert!(!dir.path().join(PROFILES_FILE).exists());
}

#[tokio::test]
async fn fresh_invitation_pairing_works_without_account_configuration() {
    use crate::api::test_support::FakeAccess;
    use crate::api::PairingDecision;

    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let access = FakeAccess::start().await;
    let core = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets: secrets.clone(),
    })
    .unwrap();
    assert!(core.profile().virtual_default);
    let outcome = core
        .pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    assert!(!outcome.device_id.is_empty());
    assert!(!dir.path().join(PROFILES_FILE).exists());
    drop(core);

    let reopened = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    assert!(reopened.profile().legacy);
    assert!(reopened.is_paired());
    assert_eq!(reopened.data_dir(), dir.path());
}

#[test]
fn custom_service_selection_preserves_the_legacy_host_pin() {
    use crate::api::config::{ConfigStore, PersistedConfig};

    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let mut config = PersistedConfig::new(
        "https://access.test".into(),
        None,
        "h-1".into(),
        "d-1".into(),
    );
    let pin = "AB".repeat(32);
    config.host_fingerprint = Some(pin.clone());
    config.host_fingerprints.insert("h-1".into(), pin.clone());
    ConfigStore::new(dir.path()).save(&config).unwrap();
    let original = std::fs::read(dir.path().join("config.json")).unwrap();

    let profiles = profiles_at(&dir);
    profiles
        .add_custom(
            "corp",
            CustomServiceInput {
                api_origin: "https://api.corp.example".into(),
                issuer: "https://id.corp.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            },
            &secrets,
        )
        .unwrap();
    profiles.activate("corp").unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        original
    );

    profiles.activate(LEGACY_PROFILE_ID).unwrap();
    let core = crate::Core::new(crate::CoreOptions {
        data_dir: dir.path().to_path_buf(),
        secrets,
    })
    .unwrap();
    assert!(core.profile().legacy);
    assert!(core.is_paired());
    let reopened: PersistedConfig =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(reopened.host_fingerprint.as_deref(), Some(pin.as_str()));
}

#[test]
fn profile_registry_contains_no_secret_material() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = memory();
    let profiles = profiles_at(&dir);
    profiles
        .add_custom(
            "corp",
            CustomServiceInput {
                api_origin: "https://api.corp.example".into(),
                issuer: "https://id.corp.example".into(),
                native_client_id: "whalerein-cli".into(),
                access_url: None,
            },
            &secrets,
        )
        .unwrap();
    profiles.activate("corp").unwrap();
    secrets
        .save(
            &crate::security::keys::pending_pairing("synthetic-device"),
            b"SENSITIVE-MARKER",
        )
        .unwrap();
    let registry = std::fs::read_to_string(dir.path().join(PROFILES_FILE)).unwrap();
    assert!(!registry.contains("SENSITIVE-MARKER"));
    assert!(!registry.contains("whalerein:v1:"));
    assert!(!registry.contains("private_key"));
    assert!(!registry.contains("device-token"));
}
