//! A native caller supplies its selected configuration and SecretStore to Core.

use std::sync::Arc;

use whalerein_core::api::{ClientConfig, ClientService, Core, MemorySecretStore};

#[test]
fn selected_configuration_can_move_without_retargeting_an_absolute_instance() {
    let sandbox = tempfile::tempdir().unwrap();
    let directory = sandbox.path().join("instance");
    let old = sandbox.path().join("old-instance");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("config.json"), b"old-state-fixture").unwrap();

    let first = sandbox.path().join("selected.toml");
    let options = ClientConfig::new(ClientService::Standalone, &directory);
    Core::write_config(&first, &options).unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let client = Core::open(&first, secrets.clone()).unwrap();
    assert_eq!(client.client_config().service, ClientService::Standalone);
    assert_eq!(
        client.configured_data_dir(),
        sandbox.path().canonicalize().unwrap().join("instance")
    );
    assert!(client.instance_id().unwrap().is_none());
    assert!(!directory.exists());

    let subdir = sandbox.path().join("configuration");
    std::fs::create_dir(&subdir).unwrap();
    let moved = subdir.join("selected.toml");
    std::fs::rename(first, &moved).unwrap();
    let reopened = Core::open(&moved, secrets).unwrap();
    assert_eq!(reopened.configured_data_dir(), client.configured_data_dir());
    assert_eq!(reopened.config_path(), moved.canonicalize().unwrap());
    assert_eq!(
        std::fs::read(old.join("config.json")).unwrap(),
        b"old-state-fixture"
    );
    assert!(!directory.exists());
}
