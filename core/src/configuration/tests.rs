use std::path::Path;

use serde_json::json;

use super::document::{self, CONFIG_CATALOG_JSON};
use super::*;

fn generated_cert(host: &str) -> (Vec<u8>, rustls::pki_types::CertificateDer<'static>) {
    let key = rcgen::generate_simple_self_signed(vec![host.to_string()]).unwrap();
    (key.cert.pem().into_bytes(), key.cert.der().clone())
}

fn verify(
    roots: &[rustls::pki_types::CertificateDer<'static>],
    leaf: &rustls::pki_types::CertificateDer<'static>,
    host: &str,
) -> Result<(), rustls::Error> {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store.add(root.clone()).unwrap();
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder(std::sync::Arc::new(store))
        .build()
        .unwrap();
    use rustls::client::danger::ServerCertVerifier;
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).unwrap();
    verifier
        .verify_server_cert(leaf, &[], &name, &[], rustls::pki_types::UnixTime::now())
        .map(|_| ())
}

#[test]
fn config_env_ac_10_correct_chain_and_hostname_verify_but_wrong_host_does_not() {
    let (pem, leaf) = generated_cert("app.whalerein.test");
    let root = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    verify(&root, &leaf, "app.whalerein.test").expect("valid chain and host");
    assert!(verify(&root, &leaf, "other.whalerein.test").is_err());
    assert!(verify(&root, &leaf, "localhost").is_err());
}

#[test]
fn config_env_ac_10_wrong_ca_is_rejected() {
    let (wrong_pem, wrong_leaf) = generated_cert("app.whalerein.test");
    let (trusted_pem, _) = generated_cert("app.whalerein.test");
    let root = rustls_pemfile::certs(&mut &trusted_pem[..])
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(verify(&root, &wrong_leaf, "app.whalerein.test").is_err());
    assert!(!wrong_pem.is_empty());
}

#[test]
fn config_env_ac_05_catalog_validation_rejects_each_malformed_shape() {
    let base: serde_json::Value = serde_json::from_str(CONFIG_CATALOG_JSON).unwrap();
    let mutate = |path: &[&str], value: serde_json::Value| {
        let mut catalog = base.clone();
        let mut current = &mut catalog;
        for key in &path[..path.len() - 1] {
            current = &mut current[key];
        }
        current[path[path.len() - 1]] = value;
        catalog
    };
    let invalid = |catalog: serde_json::Value| {
        document::parse_catalog(&serde_json::to_string(&catalog).unwrap()).unwrap_err()
    };
    assert_eq!(
        invalid(mutate(&["schema"], json!("other"))).code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(&["version"], json!(2))).code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "mode"],
            json!("standalone")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "issuer"],
            json!("https://app.whalerein.test/oidc")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "tls", "ca_provider"],
            json!("disabled")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "local", "tls", "ca_file"],
            json!("/tmp/rootCA.pem")
        ))
        .code(),
        "invalid_syntax"
    );
    assert_eq!(
        invalid(mutate(
            &["environments", "production", "tls", "ca_provider"],
            json!("mkcert")
        ))
        .code(),
        "invalid_syntax"
    );
    let mut extra = base.clone();
    extra["environments"]["extra"] = base["environments"]["local"].clone();
    assert_eq!(invalid(extra).code(), "invalid_syntax");
    let mut missing = base;
    missing["environments"]
        .as_object_mut()
        .unwrap()
        .remove("local");
    assert_eq!(invalid(missing).code(), "invalid_syntax");
}

#[test]
fn config_env_ac_10_trust_definition_errors_are_typed() {
    let definition = EnvironmentDefinition {
        id: "custom".into(),
        mode: Some(EnvironmentMode::Standalone),
        label: None,
        api_origin: None,
        issuer: None,
        native_client_id: None,
        environment: None,
        tls: None,
    };
    let resolver = TrustResolver::new("/tmp/config.toml");
    assert_eq!(resolver.config_path(), Path::new("/tmp/config.toml"));
    assert_eq!(
        resolver.resolve_definition(&definition).unwrap_err().code(),
        "invalid_tls"
    );
    let mut mkcert = definition.clone();
    mkcert.tls = Some(TlsConfig {
        ca_provider: TlsProvider::Mkcert,
        ca_file: None,
    });
    assert_eq!(
        resolver.resolve_definition(&mkcert).unwrap_err().code(),
        "invalid_tls"
    );
    let mut file = definition;
    file.tls = Some(TlsConfig {
        ca_provider: TlsProvider::File,
        ca_file: None,
    });
    assert_eq!(
        resolver.resolve_definition(&file).unwrap_err().code(),
        "invalid_tls"
    );
}

#[test]
fn selected_standalone_trust_reloads_relative_ca_and_rejects_invalid_ca() {
    use crate::api::{ClientConfig, ClientService, Core, MemorySecretStore};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("selected.toml");
    let ca = directory.path().join("root-ca.pem");
    let (first, _) = generated_cert("access.example.test");
    let (second, _) = generated_cert("access.example.test");
    std::fs::write(&ca, &first).unwrap();
    let mut config = ClientConfig::new(ClientService::Standalone, "state");
    config.standalone_ca_certificate = Some("root-ca.pem".into());
    Core::write_config(&path, &config).unwrap();
    let secrets = std::sync::Arc::new(MemorySecretStore::new());
    let before = Core::open(&path, secrets.clone()).unwrap();
    assert_eq!(before.ca_pem().as_deref(), Some(first.as_slice()));
    std::fs::write(&ca, &second).unwrap();
    let after = Core::open(&path, secrets.clone()).unwrap();
    assert_eq!(after.ca_pem().as_deref(), Some(second.as_slice()));
    std::fs::write(&ca, b"invalid fixture").unwrap();
    assert!(Core::open(&path, secrets).is_err());
    assert!(!directory.path().join("state").exists());
}

#[test]
fn selected_writer_keeps_a_complete_document_when_atomic_replacement_fails() {
    use crate::api::{ClientConfig, ClientService, Core, MemorySecretStore};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("selected.toml");
    let first = ClientConfig::new(ClientService::Standalone, "first");
    let second = ClientConfig::new(ClientService::Standalone, "second");
    Core::write_config(&path, &first).unwrap();
    let original = std::fs::read(&path).unwrap();
    for fault in [1, 2, 3] {
        writer::FAULT.set(fault);
        let result = Core::write_config(&path, &second);
        writer::FAULT.set(0);
        assert!(result.is_err());
        if fault < 3 {
            assert_eq!(std::fs::read(&path).unwrap(), original);
        } else {
            let opened = Core::open(&path, std::sync::Arc::new(MemorySecretStore::new())).unwrap();
            assert_eq!(opened.client_config(), &second);
        }
    }
}
