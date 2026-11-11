use super::test_support::core;
use crate::api::test_support::FakeAccess;
use crate::api::PairingDecision;

#[tokio::test]
async fn doctor_export_omits_process_state() {
    let access = FakeAccess::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = core(dir.path());
    core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
        .await
        .unwrap();
    let report = core.doctor().unwrap();
    assert!(report.address.is_some());
    let path = dir.path().join("diagnostic.json");
    let manifest = core.export_diagnostics(&path).unwrap();
    assert_eq!(
        manifest.fields,
        vec![
            "address".to_owned(),
            "commands.non_terminal".to_owned(),
            "commands.unknown".to_owned(),
            "last_failure".to_owned(),
        ]
    );
    let bundle = std::fs::read_to_string(path).unwrap();
    assert!(!bundle.contains("handshake"));
    assert!(!bundle.contains("reconnect"));
    assert!(!bundle.contains("host-1"));
}
