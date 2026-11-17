//! Cross-runtime writer parity: Rust Core and the Node Host runtime must
//! cooperate on the same file, preserve unrelated comments and reject stale
//! revisions instead of overwriting.
//!
//! The test needs `node` and the `host/runtime` Node workspace (Toml parser and
//! range editor dependencies). It is ignored by default, so platform jobs
//! without a Node workspace do not report a false pass; CI runs the Node corpus
//! separately and local delivery runs this file explicitly.

use std::process::Command;

use whalerein_core::api::configuration::{
    ConfigOpenOptions, ConfigPatch, Configuration, ConfigurationError, EnvironmentMode,
};

const DOCUMENT: &str = "\
# Operator notes must survive every writer.
version = 1
environment = \"production\" # saved default

[environments.company]
label = \"Company Cloud\"
mode = \"account\"
api_origin = \"https://cloud.example.com\"
issuer = \"https://auth.example.com/oidc\"
native_client_id = \"whalerein-cli\"

[environments.lab]
label = \"Lab pairing\"
mode = \"standalone\"
";

fn node_script(host_root: &std::path::Path) -> String {
    let module = host_root.join("host/runtime/src/configuration/index.js");
    format!(
        r#"
import {{ open, update, resolveEnvironment }} from {module:?}
const [path, action, rev, env] = process.argv.slice(2)
try {{
  const snapshot = open({{ configFile: path, env: false }})
  if (action === 'set-default') {{
    const next = update(snapshot, {{ type: 'set_default_environment', environment: env }}, rev)
    process.stdout.write(JSON.stringify({{ revision: next.revision }}))
  }} else if (action === 'read') {{
    const resolution = resolveEnvironment(snapshot)
    process.stdout.write(JSON.stringify(resolution))
  }} else {{
    process.stderr.write('unknown action')
    process.exit(2)
  }}
}} catch (error) {{
  process.stdout.write(JSON.stringify({{ error: error?.code ?? error?.name ?? 'error' }}))
}}
"#
    )
}

fn run_node(script: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new("node")
        .arg(script)
        .args(args)
        .output()
        .expect("node is required for the cross-runtime parity test");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("node output must be JSON")
}

fn open(path: &std::path::Path) -> Configuration {
    Configuration::open(&ConfigOpenOptions {
        explicit_path: Some(path),
        ..ConfigOpenOptions::inspection()
    })
    .unwrap()
}

#[test]
#[ignore = "requires node and the host/runtime Node workspace"]
fn config_env_ac_12_rust_and_node_writers_share_revision_and_comments() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, DOCUMENT).unwrap();
    let script_path = root.path().join("writer.mjs");
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(&script_path, node_script(&repo)).unwrap();

    let snapshot = open(&path);
    let revision = snapshot.revision().unwrap().to_string();
    assert_eq!(
        snapshot.document().environment.as_deref(),
        Some("production")
    );

    // Node writes the persistent default; Rust sees it and the comments survive.
    let result = run_node(
        &script_path,
        &[path.to_str().unwrap(), "set-default", &revision, "company"],
    );
    assert!(result["revision"].is_string(), "{result}");
    let reopened = open(&path);
    assert_eq!(reopened.document().environment.as_deref(), Some("company"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# Operator notes must survive every writer."));
    assert!(text.contains("# saved default"));

    // Rust rejects the now-stale revision instead of overwriting the file.
    let stale = reopened.update(
        &ConfigPatch::SetDefaultEnvironment {
            environment: Some("lab".into()),
        },
        Some(&revision),
    );
    assert!(
        matches!(
            stale,
            Err(whalerein_core::api::CoreError::Configuration(ref error))
                if matches!(error.as_ref(), ConfigurationError::RevisionConflict { .. })
        ),
        "{stale:?}"
    );

    // Rust writes a custom environment; Node resolves the same authority.
    let current = open(&path);
    let next = current
        .update(
            &ConfigPatch::UpsertCustomEnvironment(
                whalerein_core::api::configuration::CustomEnvironmentInput {
                    environment: "partner".into(),
                    label: Some("Partner".into()),
                    mode: EnvironmentMode::Account,
                    api_origin: Some("https://partner.example.com".into()),
                    issuer: Some("https://auth.partner.example.com/oidc".into()),
                    native_client_id: Some("whalerein-cli".into()),
                    tls: None,
                },
            ),
            current.revision(),
        )
        .unwrap();
    assert_eq!(next.document().environment.as_deref(), Some("company"));

    // Node rejecting a stale revision is the same typed conflict.
    let node = run_node(
        &script_path,
        &[path.to_str().unwrap(), "set-default", &revision, "staging"],
    );
    assert_eq!(node["error"], "revision_conflict");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[environments.partner]"), "{text}");
    assert!(text.contains("# Operator notes must survive every writer."));
}
