//! Core-only configuration consumer test.
//!
//! This integration test imports only `whalerein_core::api`; it never imports
//! the CLI, constructs an account client, or uses a desktop-home default. Sandbox
//! paths are explicit and moving the configuration file leaves identity state
//! untouched.

use std::path::Path;

use whalerein_core::api::{
    ConfigOpenOptions, ConfigPatch, Configuration, DefinitionSource, SelectionSource,
};

fn write_config(path: &Path) {
    std::fs::write(
        path,
        "version = 1\nenvironment = \"company\"\n\n[environments.company]\n\
         label = \"Company Cloud\"\nmode = \"account\"\n\
         api_origin = \"https://cloud.example.com\"\n\
         issuer = \"https://auth.example.com/oidc\"\n\
         native_client_id = \"whalerein-cli\"\n",
    )
    .unwrap();
}

#[test]
fn config_env_ac_18_core_consumer_uses_sandbox_paths_and_keeps_state_on_move() {
    let state = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("existing-identity"), b"synthetic").unwrap();
    let before: Vec<_> = std::fs::read_dir(state.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();

    let first_dir = tempfile::tempdir().unwrap();
    let first = first_dir.path().join("device.toml");
    write_config(&first);
    let configuration = Configuration::open(&ConfigOpenOptions {
        explicit_path: Some(&first),
        ..ConfigOpenOptions::default()
    })
    .unwrap();
    let resolution = configuration.resolve(None).unwrap();
    assert_eq!(resolution.environment, "company");
    assert_eq!(resolution.selection_source, SelectionSource::File);
    assert_eq!(resolution.definition_source, DefinitionSource::File);
    assert_eq!(
        configuration.source().path,
        std::fs::canonicalize(&first).unwrap()
    );

    let patch = ConfigPatch::SetDefaultEnvironment {
        environment: Some("production".into()),
    };
    let updated = configuration
        .update(&patch, configuration.revision())
        .unwrap();
    assert_eq!(updated.resolve(None).unwrap().environment, "production");

    let second_dir = tempfile::tempdir().unwrap();
    let moved = second_dir.path().join("moved.toml");
    std::fs::rename(&first, &moved).unwrap();
    let reopened = Configuration::open(&ConfigOpenOptions {
        explicit_path: Some(&moved),
        ..ConfigOpenOptions::default()
    })
    .unwrap();
    assert_eq!(reopened.resolve(None).unwrap().environment, "production");
    assert_eq!(
        reopened.resolve(Some("company")).unwrap().environment,
        "company"
    );

    let after: Vec<_> = std::fs::read_dir(state.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
    assert!(state.path().join("existing-identity").exists());
    assert!(!state.path().join("profiles.json").exists());
    assert!(!state.path().join("config.json").exists());
}
