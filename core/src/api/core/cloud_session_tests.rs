use super::*;
use crate::api::test_support::{FakeHost, MockRequest, MockResponse, MockServer};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use whalerein_proto::{ExternalAuthorization, PairingClaimRequest, PairingPhase};
use x25519_dalek::{PublicKey, StaticSecret};

struct RegionalFixture {
    host: FakeHost,
    drop_finish_reply: bool,
    outcome: &'static str,
    original_operation: Option<String>,
}

impl RegionalFixture {
    fn handle(&mut self, request: MockRequest) -> MockResponse {
        assert!(request.headers.contains_key("authorization"));
        if request.path == "/v1/enrollments/devices/pairing" {
            #[derive(serde::Deserialize)]
            struct Pairing {
                authorization: ExternalAuthorization,
                claim: PairingClaimRequest,
            }
            let Pairing {
                authorization: scope,
                claim,
            } = serde_json::from_slice(&request.body).unwrap();
            let host_key = hex::encode(self.host.identity.public_key());
            let device_key = hex::encode(&scope.device_public_keys[0]);
            let fields = [
                scope.issuer.as_str(),
                &scope.audience,
                &scope.workspace_id,
                &scope.region_id,
                &scope.enrollment_revision,
                &scope.host_id,
                &scope.device_id,
                &scope.operation_id,
                &host_key,
                &device_key,
            ];
            let hash = crate::util::sha256_hex(
                format!("whalerein/v1/cloud-pairing\n{}", fields.join("\n")).as_bytes(),
            );
            let finishing = claim.phase == PairingPhase::Finish as i32;
            let id = claim.claim_id.clone();
            let original_host = std::mem::replace(&mut self.host.host_id, scope.host_id);
            let response = self.host.cloud_pair_claim(claim, &hash);
            self.host.host_id = original_host;
            assert_eq!(response.status, 200);
            if finishing {
                self.original_operation = Some(id);
                if self.drop_finish_reply {
                    return MockResponse::empty(503);
                }
            }
            return response;
        }
        if request
            .path
            .starts_with("/v1/external/hosts/host-1/pairings/")
        {
            assert_eq!(
                request.path.rsplit('/').next(),
                self.original_operation.as_deref()
            );
            return MockResponse::json(200, &json!({"state": self.outcome}));
        }
        self.host.handle(request)
    }
}

struct CloudFixture {
    access_origin: Url,
    host_key: Vec<u8>,
    session_number: u64,
    revision: u64,
    logout_requests: u64,
    host_unavailable: bool,
}

impl CloudFixture {
    fn handle(&mut self, request: MockRequest) -> MockResponse {
        let origin = format!("https://{}", request.headers["host"]);
        if request.path == "/api/v1/client/login" {
            self.session_number += 1;
            return MockResponse::json(
                200,
                &json!({
                    "verificationUri": format!("{origin}/device"),
                    "userCode": "FIXTURE",
                    "expiresAt": now_seconds() + 60,
                    "intervalSeconds": 1,
                }),
            );
        }
        assert!(request.headers.contains_key("authorization"));
        let active = || {
            json!({
                "state": "active",
                "sessionId": format!("fixture-session-{}", self.session_number),
                "expiresAt": now_seconds() + 3600,
                "account": {"id":"account","email":"","issuer":format!("{origin}/oidc"),"subject":"subject"},
                "workspace": {"id":"workspace"},
            })
        };
        match request.path.as_str() {
            "/api/v1/client/login/poll" | "/api/v1/client/me" => MockResponse::json(200, &active()),
            path if path.starts_with("/api/v1/client/hosts?") || path == "/api/v1/client/hosts" => {
                MockResponse::json(
                    200,
                    &json!({
                        "workspace": {"id":"workspace"},
                        "hosts": [{"hostId":"host-1","displayName":"Fixture","enrolled":true,"online":true}],
                    }),
                )
            }
            "/api/v1/client/session/permission/challenge" => MockResponse::json(
                200,
                &json!({
                    "challengeId": "fixture-challenge",
                    "publicKey": URL_SAFE_NO_PAD.encode(PublicKey::from(&StaticSecret::from([61; 32])).as_bytes()),
                    "expiresAt": now_seconds() + 60,
                }),
            ),
            "/api/v1/client/session/permission" => {
                self.revision += 1;
                MockResponse::json(
                    200,
                    &json!({
                        "region":"fixture-region",
                        "accessOrigin": self.access_origin,
                        "expiresAt": now_seconds() + 45,
                        "revision": self.revision,
                    }),
                )
            }
            "/api/v1/client/hosts/host-2/pairing" if self.host_unavailable => {
                MockResponse::empty(503)
            }
            "/api/v1/client/hosts/host-1/pairing" | "/api/v1/client/hosts/host-2/pairing" => {
                let host = request.path.split('/').nth(5).unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let key = URL_SAFE_NO_PAD
                    .decode(body["deviceNoiseKey"].as_str().unwrap())
                    .unwrap();
                let scope = ExternalAuthorization {
                    auth_version: "whalerein/v1/ext-auth-v1".into(),
                    purpose: "device_pairing".into(),
                    issuer: format!("{origin}/"),
                    audience: self.access_origin.to_string(),
                    workspace_id: "workspace".into(),
                    region_id: "fixture-region".into(),
                    host_id: host.into(),
                    device_id: body["deviceId"].as_str().unwrap().into(),
                    operation_id: body["operationId"].as_str().unwrap().into(),
                    enrollment_revision: "1".into(),
                    host_public_keys: vec![self.host_key.clone()],
                    device_public_keys: vec![key],
                    ..Default::default()
                };
                MockResponse::json(
                    200,
                    &json!({
                        "operationId": scope.operation_id,
                        "authorization": scope,
                        "route": {"region": "fixture-region", "accessOrigin": self.access_origin},
                        "hostNoisePublicKey": URL_SAFE_NO_PAD.encode(&self.host_key),
                    }),
                )
            }
            path if path.starts_with("/api/v1/client/hosts/host-2/devices/")
                && path.ends_with("/revoke") =>
            {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                MockResponse::json(
                    200,
                    &json!({"operationId": body["operationId"], "state":"revoked"}),
                )
            }
            "/api/v1/client/sessions" => MockResponse::json(
                200,
                &json!({
                    "sessions":[{"sessionId":"remote-fixture-session","instanceId":"remote-fixture-instance","expiresAt":now_seconds()+60,"state":"active"}]
                }),
            ),
            "/api/v1/client/sessions/remote-fixture-session/revoke" => {
                MockResponse::json(202, &json!({"state":"pending"}))
            }
            path if path.starts_with("/api/v1/client/sessions/") && path.ends_with("/revoke") => {
                self.logout_requests += 1;
                if self.logout_requests == 1 {
                    return MockResponse::json(202, &json!({"state":"pending"}));
                }
                MockResponse::json(200, &json!({"state":"revoked"}))
            }
            "/api/v1/client/session-revocations" => {
                MockResponse::json(200, &json!({"state":"revoked"}))
            }
            _ => MockResponse::empty(404),
        }
    }
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn cloud_session_connect_recovers_original_pairing_after_logout_and_route_change() {
    let mut host = FakeHost::new();
    host.extra_host = true;
    let regional = Arc::new(Mutex::new(RegionalFixture {
        host,
        drop_finish_reply: true,
        outcome: "pending",
        original_operation: None,
    }));
    let host_key = regional.lock().unwrap().host.identity.public_key().to_vec();
    let first_region = regional.clone();
    let (access, access_ca) = MockServer::start_tls(Arc::new(move |request| {
        first_region.lock().unwrap().handle(request)
    }))
    .await;
    let second_region = regional.clone();
    let (standby, standby_ca) = MockServer::start_tls(Arc::new(move |request| {
        second_region.lock().unwrap().handle(request)
    }))
    .await;
    let cloud = Arc::new(Mutex::new(CloudFixture {
        access_origin: access.url(),
        host_key,
        session_number: 0,
        revision: 0,
        logout_requests: 0,
        host_unavailable: true,
    }));
    let handler = cloud.clone();
    let (account, mut cloud_ca) = MockServer::start_tls(Arc::new(move |request| {
        handler.lock().unwrap().handle(request)
    }))
    .await;
    cloud_ca.extend(access_ca);
    cloud_ca.extend(standby_ca);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cloud.toml");
    Core::write_config(
        &path,
        &ClientConfig::new(ClientService::WhaleReinCloud, "instance"),
    )
    .unwrap();
    let mut instance = Instance::open(&path).unwrap();
    let definition = &mut instance.cloud.as_mut().unwrap().definition;
    definition.api_origin = Some(account.url().to_string());
    definition.issuer = Some(account.url().join("oidc").unwrap().to_string());
    instance.ca_pem = Some(cloud_ca);
    let instance = Arc::new(instance);
    let secrets = Arc::new(InstanceSecrets {
        backend: Arc::new(crate::api::MemorySecretStore::new()),
        instance: instance.clone(),
    });
    let core = Core::from_parts(
        CoreOptions {
            data_dir: instance.runtime_dir(),
            secrets,
        },
        None,
        Some(instance),
        Arc::default(),
        None,
    );

    assert!(matches!(
        core.status().await.unwrap().device_access,
        DeviceAccess::SignInRequired
    ));
    core.login(|prompt| assert_eq!(prompt.user_code, "FIXTURE"))
        .await
        .unwrap();
    assert_eq!(core.account_hosts().await.unwrap()[0].host_id, "host-1");
    let other_session = core.cloud_account_sessions().await.unwrap().remove(0);
    assert_eq!(
        core.revoke_cloud_account_session(&other_session.session_id)
            .await
            .unwrap(),
        crate::api::AccountSessionState::Pending
    );
    assert!(matches!(
        core.connect_cloud("missing").await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        core.connect_cloud("host-2").await,
        Err(CoreError::Unavailable(_))
    ));
    assert!(core.connect_cloud("host-1").await.is_err());
    let pending = core.pending_cloud_pairings().unwrap();
    assert_eq!(pending.len(), 2);
    let (partial, _) = &pending[0];
    assert_eq!(
        regional.lock().unwrap().original_operation.as_ref(),
        Some(partial)
    );
    let session = core.cloud_session_id().unwrap().unwrap();

    assert!(!core.logout().await.unwrap());
    assert!(core.account_status().unwrap().is_none());
    core.login(|_| {}).await.unwrap();
    assert_ne!(core.cloud_session_id().unwrap().as_ref(), Some(&session));
    assert!(matches!(
        core.recover_cloud_pairing(partial).await,
        Err(CoreError::Conflict(_))
    ));
    cloud.lock().unwrap().access_origin = standby.url();
    assert!(matches!(
        core.recover_cloud_pairing(partial).await,
        Err(CoreError::Conflict(_))
    ));
    cloud.lock().unwrap().access_origin = access.url();
    {
        let mut region = regional.lock().unwrap();
        region.outcome = "ready";
        region.host.hide_primary_from_hosts = true;
    }
    assert!(matches!(
        core.recover_cloud_pairing(partial).await,
        Err(CoreError::Security(_))
    ));
    regional.lock().unwrap().host.hide_primary_from_hosts = false;
    let recovered = core.recover_cloud_pairing(partial).await.unwrap();
    assert_eq!(recovered.operation_id.as_ref(), Some(partial));
    assert_eq!(
        core.recover_cloud_pairing(partial).await.unwrap().device_id,
        recovered.device_id
    );
    assert_eq!(
        core.pending_cloud_pairings().unwrap(),
        vec![pending[1].clone()]
    );
    assert!(matches!(
        core.connect_cloud("host-2").await,
        Err(CoreError::Unavailable(_))
    ));
    assert_ne!(
        core.pending_cloud_pairings().unwrap(),
        vec![pending[1].clone()]
    );
    assert_eq!(
        core.connect_cloud("host-1").await.unwrap().operation_id,
        None
    );

    let retry = core.pending_cloud_pairings().unwrap().remove(0).0;
    regional.lock().unwrap().drop_finish_reply = false;
    cloud.lock().unwrap().host_unavailable = false;
    let second = core.connect_cloud("host-2").await.unwrap();
    assert_eq!(second.operation_id.as_deref(), Some(retry.as_str()));
    assert_eq!(second.device_id, recovered.device_id);
    assert!(core.pending_cloud_pairings().unwrap().is_empty());
    let revoked = core.revoke_cloud("host-2").await.unwrap();
    assert_eq!(revoked.state, cloud_pairing::CloudRevocationState::Revoked);
    core.set_default_host("host-1").unwrap();
    cloud.lock().unwrap().access_origin = standby.url();
    assert_eq!(core.hosts().await.unwrap()[0].host_id, "host-1");
    assert_eq!(
        core.access_address().as_deref(),
        Some(standby.url().as_str())
    );
    assert!(matches!(
        core.status().await.unwrap().device_access,
        DeviceAccess::Ready
    ));
    let mut watch = core
        .observe_session(
            &SessionKey::new("host-1", "session-1").unwrap(),
            crate::api::ObservationInterests::questions(),
            crate::api::Cancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        core.revoke_all_cloud_account_sessions().await.unwrap(),
        crate::api::AccountSessionState::Revoked
    );
    let status = core.status().await.unwrap();
    assert!(matches!(status.device_access, DeviceAccess::SignInRequired));
    assert!(status.default_host.is_none());
    core.login(|_| {}).await.unwrap();
    let mut replacement = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match core
                .observe_session(
                    &SessionKey::new("host-1", "session-1").unwrap(),
                    crate::api::ObservationInterests::questions(),
                    crate::api::Cancellation::new(),
                )
                .await
            {
                Ok(replacement) => break replacement,
                Err(CoreError::Conflict(_)) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(error) => panic!("a current session could not attach an observation: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(watch.next_event().await.is_none());
    assert!(matches!(
        watch.take_terminal_error(),
        Some(CoreError::AuthenticationRequired)
    ));
    replacement.close().await;
}
