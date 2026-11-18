//! Provider-issued native account sessions. Cloud only accepts access tokens
//! addressed to its native client API. Regional device authorization is independent.

#[cfg(test)]
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::time::{sleep, timeout_at, Instant};
use url::Url;

use crate::api::instance::{AccountScope, Instance};
use crate::api::{CoreError, CoreResult, SecretStore};

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MAX_BODY: usize = 1024 * 1024;
const MAX_LOGIN: u64 = 20 * 60;
const ACCOUNT_SESSION: &str = "account-session";

#[derive(Clone, Serialize, Deserialize)]
pub struct SignedInAccount {
    pub account: AccountIdentity,
    pub workspace: AccountWorkspace,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub subject: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountWorkspace {
    pub id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all(deserialize = "camelCase"))]
pub struct AccountHost {
    pub host_id: String,
    pub display_name: String,
    pub enrolled: bool,
    pub online: bool,
    #[serde(default)]
    pub observation: String,
    #[serde(default)]
    pub enrollment_state: String,
    #[serde(default)]
    pub pairing_eligible: bool,
    #[serde(default)]
    pub device_access: String,
}

pub struct AccountLoginPrompt {
    pub verification_uri: Url,
    pub user_code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    issuer: Url,
    resource: Url,
    client_id: String,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: Url,
    device_authorization_endpoint: Url,
    token_endpoint: Url,
    revocation_endpoint: Url,
}

#[derive(Deserialize)]
struct DeviceGrant {
    device_code: String,
    user_code: String,
    verification_uri: Url,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    token_type: String,
    expires_in: u64,
}

#[derive(Serialize, Deserialize)]
struct Session {
    client_id: String,
    access: String,
    refresh: String,
    expires: u64,
    #[serde(default)]
    verified: Option<SignedInAccount>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

pub struct AccountClient {
    http: Client,
    api: Url,
    issuer: Url,
    secrets: Arc<dyn SecretStore>,
    slot: String,
    instance: Arc<Instance>,
    _session_lock: std::fs::File,
}

impl AccountClient {
    pub(crate) async fn for_instance(
        instance: Arc<Instance>,
        secrets: Arc<dyn SecretStore>,
        create: bool,
    ) -> CoreResult<Self> {
        let definition = &instance
            .cloud
            .as_ref()
            .ok_or(CoreError::Unsupported)?
            .definition;
        let api = configured_url(definition.api_origin.as_deref())?;
        let issuer = configured_url(definition.issuer.as_deref())?;
        if create {
            instance.prepare_runtime()?;
        } else if instance.manifest()?.is_none() || secrets.load(ACCOUNT_SESSION)?.is_none() {
            return Err(CoreError::Unauthorized);
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(instance.runtime_dir().join("account.lock"))
            .map_err(|_| CoreError::Config("account session lock is unavailable".into()))?;
        let deadline = Instant::now() + Duration::from_secs(MAX_LOGIN);
        loop {
            match fs4::FileExt::try_lock(&lock) {
                Ok(()) => break,
                Err(fs4::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(CoreError::Conflict(
                            "another account operation is still running".into(),
                        ));
                    }
                    sleep(Duration::from_millis(20)).await;
                }
                Err(fs4::TryLockError::Error(_)) => {
                    return Err(CoreError::Config(
                        "account session lock is unavailable".into(),
                    ));
                }
            }
        }
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15));
        if let Some(pem) = &instance.ca_pem {
            for ca in reqwest::Certificate::from_pem_bundle(pem)
                .map_err(|_| CoreError::Config("account CA bundle is invalid".into()))?
            {
                builder = builder.add_root_certificate(ca);
            }
        }
        Ok(Self {
            http: builder.build().map_err(|_| unavailable())?,
            api,
            issuer,
            // Logto assigns application IDs at registration. The trusted Cloud
            // endpoint publishes the actual ID and checks the token's authorized party.
            secrets,
            slot: ACCOUNT_SESSION.into(),
            instance,
            _session_lock: lock,
        })
    }

    pub(crate) fn local_status(
        instance: &Instance,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<Option<SignedInAccount>> {
        let Some(manifest) = instance.manifest()? else {
            return Ok(None);
        };
        let Some(bytes) = secrets.load(ACCOUNT_SESSION)? else {
            return Ok(None);
        };
        let session: Session = serde_json::from_slice(&bytes)
            .map_err(|_| CoreError::Security("stored account session is invalid".into()))?;
        let account = session
            .verified
            .ok_or_else(|| CoreError::Security("account binding is missing".into()))?;
        if manifest.account.as_ref() != Some(&account.scope()) {
            return Err(CoreError::Unauthorized);
        }
        Ok(Some(account))
    }

    pub(crate) async fn hosts_for_device(
        &self,
        device_id: Option<&str>,
    ) -> CoreResult<Vec<AccountHost>> {
        #[derive(Deserialize)]
        struct Listing {
            hosts: Vec<AccountHost>,
            workspace: AccountWorkspace,
        }
        let mut url = self
            .api
            .join("api/v1/client/hosts")
            .map_err(|_| CoreError::ProtocolIncompatible)?;
        if let Some(id) = device_id {
            url.query_pairs_mut().append_pair("deviceId", id);
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(self.access().await?)
            .send()
            .await
            .map_err(request_error)?;
        let listing: Listing = decode_success(response).await?;
        if self
            .instance
            .manifest()?
            .and_then(|manifest| manifest.account)
            .is_none_or(|scope| scope.workspace != listing.workspace.id)
        {
            return Err(CoreError::Unauthorized);
        }
        Ok(listing.hosts)
    }

    pub(crate) async fn post_scoped<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &impl Serialize,
    ) -> CoreResult<T> {
        let mut target = self
            .api
            .join("api/v1/client/")
            .map_err(|_| CoreError::ProtocolIncompatible)?;
        target
            .path_segments_mut()
            .map_err(|_| CoreError::ProtocolIncompatible)?
            .pop_if_empty()
            .extend(segments.iter().copied());
        let response = self
            .http
            .post(target)
            .bearer_auth(self.access().await?)
            .json(body)
            .send()
            .await
            .map_err(request_error)?;
        decode_scoped(response).await
    }

    pub async fn login(
        &self,
        present: impl FnOnce(AccountLoginPrompt),
    ) -> CoreResult<SignedInAccount> {
        match self.status().await {
            Ok(account) => return Ok(account),
            Err(CoreError::Unauthorized) => {}
            Err(error) => return Err(error),
        }
        let registration = self.registration().await?;
        let discovery = self.discovery().await?;
        let grant: DeviceGrant = decode_success(
            self.http
                .post(discovery.device_authorization_endpoint)
                .form(&[
                    ("client_id", registration.client_id.as_str()),
                    ("scope", "openid offline_access"),
                    ("resource", registration.resource.as_str()),
                ])
                .send()
                .await
                .map_err(request_error)?,
        )
        .await?;
        if !same_authority(&grant.verification_uri, &self.issuer)
            || grant.expires_in == 0
            || grant.device_code.is_empty()
            || grant.user_code.is_empty()
            || grant.user_code.len() > 128
            || grant.user_code.chars().any(char::is_control)
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        let end = Instant::now() + Duration::from_secs(grant.expires_in.min(MAX_LOGIN));
        present(AccountLoginPrompt {
            verification_uri: grant.verification_uri.clone(),
            user_code: grant.user_code.clone(),
        });
        let initial = timeout_at(
            end,
            self.poll(&grant, &registration, &discovery.token_endpoint),
        )
        .await
        .map_err(|_| CoreError::Timeout)??;
        let refresh = initial
            .refresh_token
            .filter(|token| !token.is_empty())
            .ok_or(CoreError::ProtocolIncompatible)?;
        let mut session = self
            .refresh(&registration, &discovery.token_endpoint, &refresh)
            .await?;
        let account = self.me(&session.access).await?;
        self.instance.bind_account(&account.scope())?;
        session.verified = Some(account.clone());
        self.save(&session)?;
        Ok(account)
    }

    pub async fn status(&self) -> CoreResult<SignedInAccount> {
        let account = self.me(&self.access().await?).await?;
        verify_bound_scope(&self.instance, &account)?;
        Ok(account)
    }

    pub async fn logout(&self) -> CoreResult<()> {
        let Some(session) = self.load()? else {
            return Ok(());
        };
        let remote = async {
            let discovery = self.discovery().await?;
            let response = self
                .http
                .post(discovery.revocation_endpoint)
                .form(&[
                    ("client_id", session.client_id.as_str()),
                    ("token", session.refresh.as_str()),
                    ("token_type_hint", "refresh_token"),
                ])
                .send()
                .await
                .map_err(request_error)?;
            if !response.status().is_success() {
                return Err(unavailable());
            }
            Ok(())
        }
        .await;
        self.secrets.delete(&self.slot)?;
        remote
    }

    async fn registration(&self) -> CoreResult<Registration> {
        let registration: Registration = self.get("api/v1/client/login", None).await?;
        if registration.issuer != self.issuer
            || registration.resource
                != self
                    .api
                    .join("api/v1/client")
                    .map_err(|_| CoreError::ProtocolIncompatible)?
            || registration.client_id.is_empty()
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        Ok(registration)
    }

    async fn discovery(&self) -> CoreResult<Discovery> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.as_str().trim_end_matches('/')
        );
        let discovery: Discovery =
            decode_success(self.http.get(url).send().await.map_err(request_error)?).await?;
        if discovery.issuer != self.issuer
            || [
                &discovery.device_authorization_endpoint,
                &discovery.token_endpoint,
                &discovery.revocation_endpoint,
            ]
            .iter()
            .any(|endpoint| !same_authority(endpoint, &self.issuer))
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        Ok(discovery)
    }

    async fn poll(
        &self,
        grant: &DeviceGrant,
        registration: &Registration,
        endpoint: &Url,
    ) -> CoreResult<Tokens> {
        let mut interval = Duration::from_secs(grant.interval.clamp(1, MAX_LOGIN));
        loop {
            sleep(interval).await;
            let response = self
                .http
                .post(endpoint.clone())
                .form(&[
                    ("client_id", registration.client_id.as_str()),
                    ("grant_type", DEVICE_GRANT),
                    ("device_code", grant.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(request_error)?;
            if response.status().is_success() {
                return decode(response).await;
            }
            if response.status() != StatusCode::BAD_REQUEST {
                return Err(unavailable());
            }
            let error: OAuthError = decode(response).await?;
            match error.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += Duration::from_secs(5),
                "expired_token" => return Err(CoreError::Timeout),
                "access_denied" => return Err(CoreError::Unauthorized),
                _ => return Err(unavailable()),
            }
        }
    }

    async fn refresh(
        &self,
        registration: &Registration,
        endpoint: &Url,
        refresh: &str,
    ) -> CoreResult<Session> {
        let response = self
            .http
            .post(endpoint.clone())
            .form(&[
                ("client_id", registration.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("resource", registration.resource.as_str()),
            ])
            .send()
            .await
            .map_err(request_error)?;
        if response.status() == StatusCode::BAD_REQUEST {
            let error: OAuthError = decode(response).await?;
            return Err(if error.error == "invalid_grant" {
                CoreError::Unauthorized
            } else {
                unavailable()
            });
        }
        let tokens: Tokens = decode_success(response).await?;
        if !tokens.token_type.eq_ignore_ascii_case("Bearer")
            || tokens.access_token.is_empty()
            || tokens.expires_in == 0
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        Ok(Session {
            client_id: registration.client_id.clone(),
            access: tokens.access_token,
            refresh: tokens
                .refresh_token
                .filter(|token| !token.is_empty())
                .unwrap_or_else(|| refresh.into()),
            expires: unix_now().saturating_add(tokens.expires_in),
            verified: None,
        })
    }

    async fn access(&self) -> CoreResult<String> {
        let session = self.load()?.ok_or(CoreError::Unauthorized)?;
        let account = session.verified.as_ref().ok_or(CoreError::Unauthorized)?;
        verify_bound_scope(&self.instance, account)?;
        if session.expires > unix_now().saturating_add(30) {
            return Ok(session.access);
        }
        let registration = self.registration().await?;
        if session.client_id != registration.client_id {
            return Err(CoreError::Unauthorized);
        }
        let discovery = self.discovery().await?;
        let mut updated = self
            .refresh(&registration, &discovery.token_endpoint, &session.refresh)
            .await?;
        let account = self.me(&updated.access).await?;
        verify_bound_scope(&self.instance, &account)?;
        updated.verified = Some(account);
        self.save(&updated)?;
        Ok(updated.access)
    }

    fn load(&self) -> CoreResult<Option<Session>> {
        self.secrets
            .load(&self.slot)?
            .map(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|_| CoreError::Security("stored account session is invalid".into()))
            })
            .transpose()
    }

    fn save(&self, session: &Session) -> CoreResult<()> {
        let bytes = serde_json::to_vec(session)
            .map_err(|_| CoreError::Internal("could not encode account session".into()))?;
        self.secrets.save(&self.slot, &bytes)?;
        Ok(())
    }

    async fn me(&self, token: &str) -> CoreResult<SignedInAccount> {
        self.get("api/v1/client/me", Some(token)).await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, token: Option<&str>) -> CoreResult<T> {
        let url = self
            .api
            .join(path)
            .map_err(|_| CoreError::ProtocolIncompatible)?;
        let request = self.http.request(Method::GET, url);
        let request = match token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        decode_success(request.send().await.map_err(request_error)?).await
    }
}

impl SignedInAccount {
    fn scope(&self) -> AccountScope {
        AccountScope {
            issuer: self.account.issuer.clone(),
            subject: self.account.subject.clone(),
            workspace: self.workspace.id.clone(),
        }
    }
}

fn verify_bound_scope(instance: &Instance, account: &SignedInAccount) -> CoreResult<()> {
    if instance
        .manifest()?
        .and_then(|manifest| manifest.account)
        .as_ref()
        != Some(&account.scope())
    {
        return Err(CoreError::Conflict(
            "this instance belongs to another account or workspace; choose a fresh data_dir".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn account_secret_slot(state_dir: &Path) -> CoreResult<String> {
    let canonical = state_dir
        .canonicalize()
        .map_err(|_| CoreError::Config("account profile state is unavailable".into()))?;
    Ok(format!(
        "whalerein:v1:account:{}",
        crate::util::sha256_hex(canonical.to_string_lossy().as_bytes())
    ))
}

fn configured_url(value: Option<&str>) -> CoreResult<Url> {
    let url = value
        .and_then(|value| Url::parse(value).ok())
        .ok_or_else(|| CoreError::Config("select an account environment to sign in".into()))?;
    if url.scheme() != "https" {
        return Err(CoreError::Config("account sign-in requires HTTPS".into()));
    }
    Ok(url)
}

fn same_authority(url: &Url, issuer: &Url) -> bool {
    url.origin() == issuer.origin()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn decode_scoped<T: DeserializeOwned>(response: Response) -> CoreResult<T> {
    if matches!(response.status(), StatusCode::OK | StatusCode::ACCEPTED) {
        return decode(response).await;
    }
    #[derive(Deserialize)]
    struct Problem {
        code: String,
    }
    let status = response.status();
    let code = decode::<Problem>(response)
        .await
        .ok()
        .map(|problem| problem.code);
    let error = match code.as_deref() {
        Some("device_revoked") => CoreError::Revoked,
        Some("authorization_expired") => CoreError::AuthorizationExpired,
        Some("step_up_required") => CoreError::AuthenticationRequired,
        Some("intent_conflict") => {
            CoreError::Conflict("the operation belongs to a different pairing intent".into())
        }
        Some("host_ineligible") => CoreError::Conflict("Host is not eligible for pairing".into()),
        Some("device_quota_exceeded") => {
            CoreError::Conflict("workspace Device quota is exhausted".into())
        }
        _ if status == StatusCode::NOT_FOUND => {
            CoreError::NotFound("Host or Device identity".into())
        }
        _ if status == StatusCode::BAD_REQUEST => {
            CoreError::InvalidInput("invalid Host or Device intent".into())
        }
        _ if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
            CoreError::Unauthorized
        }
        _ => unavailable(),
    };
    Err(error)
}

async fn decode_success<T: DeserializeOwned>(response: Response) -> CoreResult<T> {
    match response.status() {
        StatusCode::OK => decode(response).await,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(CoreError::Unauthorized),
        _ => Err(unavailable()),
    }
}

async fn decode<T: DeserializeOwned>(mut response: Response) -> CoreResult<T> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if body.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(CoreError::ProtocolIncompatible);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| CoreError::ProtocolIncompatible)
}

fn request_error(error: reqwest::Error) -> CoreError {
    if error.is_timeout() {
        CoreError::Timeout
    } else {
        unavailable()
    }
}

fn unavailable() -> CoreError {
    CoreError::Unavailable("account service request failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{MockResponse, MockServer};
    use crate::api::{ClientConfig, ClientService, MemorySecretStore};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn scoped_device_rejections_keep_stable_client_outcomes() {
        let server = MockServer::start(Arc::new(|request| {
            let status = match request.path.as_str() {
                "/device_revoked" | "/step_up_required" | "/forbidden" => 403,
                "/authorization_expired"
                | "/intent_conflict"
                | "/host_ineligible"
                | "/device_quota_exceeded" => 409,
                "/missing" => 404,
                "/invalid" => 400,
                _ => 503,
            };
            MockResponse::json(
                status,
                &json!({"code": request.path.trim_start_matches('/')}),
            )
        }))
        .await;
        let client = Client::new();
        for (path, expected) in [
            ("device_revoked", "revoked"),
            ("step_up_required", "authentication_required"),
            ("authorization_expired", "authorization_expired"),
            ("intent_conflict", "conflict"),
            ("host_ineligible", "conflict"),
            ("device_quota_exceeded", "conflict"),
            ("missing", "not_found"),
            ("invalid", "invalid_argument"),
            ("forbidden", "unauthorized"),
            ("outage", "unavailable"),
        ] {
            let response = client
                .get(server.url().join(path).unwrap())
                .send()
                .await
                .unwrap();
            let error = decode_scoped::<serde_json::Value>(response)
                .await
                .unwrap_err();
            assert_eq!(error.code(), expected, "{path}");
            if matches!(path, "step_up_required" | "authorization_expired") {
                assert_eq!(error.action(), "reauth");
                assert_eq!(error.exit_code(), 4);
                assert_eq!(error.source_layer(), "cloud");
            }
        }
    }

    fn native_test_instance(
        root: &std::path::Path,
    ) -> (Arc<Instance>, Arc<dyn SecretStore>, Arc<MemorySecretStore>) {
        let path = root.join("cloud.toml");
        Instance::write(
            &path,
            &ClientConfig::new(ClientService::WhaleReinCloud, "app"),
        )
        .unwrap();
        let instance = Arc::new(Instance::open(&path).unwrap());
        let backend = Arc::new(MemorySecretStore::new());
        let secrets = Arc::new(crate::api::instance::InstanceSecrets {
            instance: instance.clone(),
            backend: backend.clone(),
        });
        (instance, secrets, backend)
    }

    async fn native_test_client(
        instance: &Arc<Instance>,
        secrets: &Arc<dyn SecretStore>,
        server: &MockServer,
        create: bool,
    ) -> AccountClient {
        // Keep the production instance, storage, and file lock. Redirect this
        // test's HTTP requests to the existing loopback provider fixture.
        let mut client = AccountClient::for_instance(instance.clone(), secrets.clone(), create)
            .await
            .unwrap();
        client.api = server.url();
        client.issuer = server.url().join("oidc").unwrap();
        client
    }

    #[tokio::test]
    async fn account_device_flow_denial_expiry_and_cancellation_leave_no_session() {
        for (provider_error, expires_in, expected, polls) in [
            ("access_denied", 60, "unauthorized", 1),
            ("expired_token", 60, "timeout", 1),
            ("authorization_pending", 1, "timeout", 0),
            ("cancel", 60, "cancel", 0),
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let received = count.clone();
            let server = MockServer::start(Arc::new(move |request| {
                let origin = format!("http://{}", request.headers["host"]);
                match (request.method.as_str(), request.path.as_str()) {
                    ("GET", "/api/v1/client/login") => MockResponse::json(200, &json!({
                        "issuer": format!("{origin}/oidc"), "clientId": "device-app",
                        "resource": format!("{origin}/api/v1/client"),
                    })),
                    ("GET", "/oidc/.well-known/openid-configuration") => MockResponse::json(200, &json!({
                        "issuer": format!("{origin}/oidc"),
                        "device_authorization_endpoint": format!("{origin}/device/auth"),
                        "token_endpoint": format!("{origin}/token"),
                        "revocation_endpoint": format!("{origin}/revoke"),
                    })),
                    ("POST", "/device/auth") => MockResponse::json(200, &json!({
                        "device_code": "fixture-device-code", "user_code": "ABCD",
                        "verification_uri": format!("{origin}/device"), "expires_in": expires_in,
                        "interval": 3.min(expires_in + 1),
                    })),
                    ("POST", "/token") => {
                        received.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(400, &json!({"error": provider_error}))
                    }
                    _ => MockResponse::empty(404),
                }
            })).await;

            let directory = tempfile::tempdir().unwrap();
            let (instance, secrets, backend) = native_test_instance(directory.path());
            let client = native_test_client(&instance, &secrets, &server, true).await;
            let (presented, prompt) = tokio::sync::oneshot::channel();
            let login =
                tokio::spawn(
                    async move { client.login(|_| assert!(presented.send(()).is_ok())).await },
                );
            prompt.await.unwrap();
            if expected == "cancel" {
                login.abort();
                assert!(matches!(login.await, Err(error) if error.is_cancelled()));
            } else {
                assert!(matches!(login.await.unwrap(), Err(error) if error.code() == expected));
            }
            assert_eq!(count.load(Ordering::SeqCst), polls);
            assert!(AccountClient::local_status(&instance, &secrets)
                .unwrap()
                .is_none());
            assert!(instance.manifest().unwrap().unwrap().account.is_none());
            assert!(backend.is_empty());
            let reopened = native_test_client(&instance, &secrets, &server, true).await;
            assert!(matches!(
                reopened.status().await,
                Err(CoreError::Unauthorized)
            ));
        }
    }

    #[tokio::test]
    async fn account_device_flow_refreshes_and_revokes_the_profile_session() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let revoked = Arc::new(AtomicUsize::new(0));
        let count_refreshes = refreshes.clone();
        let count_revoked = revoked.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let origin = format!("http://{}", request.headers["host"]);
            let form: std::collections::HashMap<String, String> =
                url::form_urlencoded::parse(&request.body).into_owned().collect();
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/v1/client/login") => MockResponse::json(200, &json!({
                    "issuer": format!("{origin}/oidc"), "clientId": "device-app",
                    "resource": format!("{origin}/api/v1/client"),
                })),
                ("GET", "/oidc/.well-known/openid-configuration") => MockResponse::json(200, &json!({
                    "issuer": format!("{origin}/oidc"),
                    "device_authorization_endpoint": format!("{origin}/device/auth"),
                    "token_endpoint": format!("{origin}/token"),
                    "revocation_endpoint": format!("{origin}/revoke"),
                })),
                ("POST", "/device/auth") => {
                    assert_eq!(form["client_id"], "device-app");
                    assert_eq!(form["resource"], format!("{origin}/api/v1/client"));
                    MockResponse::json(200, &json!({
                        "device_code": "fixture-device-code", "user_code": "ABCD",
                        "verification_uri": format!("{origin}/device"),
                        "expires_in": 60, "interval": 1,
                    }))
                }
                ("POST", "/token") if form["grant_type"] == DEVICE_GRANT => {
                    assert_eq!(form["device_code"], "fixture-device-code");
                    MockResponse::json(200, &json!({
                        "access_token": "opaque-fixture", "refresh_token": "fixture-refresh-0",
                        "token_type": "Bearer", "expires_in": 600,
                    }))
                }
                ("POST", "/token") => {
                    let revision = count_refreshes.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(form["grant_type"], "refresh_token");
                    assert_eq!(form["refresh_token"], format!("fixture-refresh-{revision}"));
                    assert_eq!(form["resource"], format!("{origin}/api/v1/client"));
                    MockResponse::json(200, &json!({
                        "access_token": "resource-fixture",
                        "refresh_token": format!("fixture-refresh-{}", revision + 1),
                        "token_type": "Bearer",
                        "expires_in": if revision == 0 { 1 } else { 600 },
                    }))
                }
                ("GET", "/api/v1/client/me") => {
                    assert_eq!(request.headers["authorization"], "Bearer resource-fixture");
                    MockResponse::json(200, &json!({
                        "account": {"id": "acct", "email": "", "issuer": "https://login.whalerein.com/oidc", "subject": "subject"}, "workspace": {"id": "ws"},
                    }))
                }
                ("GET", "/api/v1/client/hosts") => {
                    assert_eq!(request.headers["authorization"], "Bearer resource-fixture");
                    MockResponse::json(200, &json!({
                        "workspace": {"id": "ws"},
                        "hosts": [{"hostId":"host-a","displayName":"Computer","enrolled":true,"online":false}],
                    }))
                }
                ("POST", "/revoke") => {
                    assert_eq!(form["token"], "fixture-refresh-2");
                    count_revoked.fetch_add(1, Ordering::SeqCst);
                    MockResponse::empty(200)
                }
                _ => MockResponse::empty(404),
            }
        })).await;

        let root = tempfile::tempdir().unwrap();
        let (instance, namespaced, secrets) = native_test_instance(root.path());
        let client = native_test_client(&instance, &namespaced, &server, true).await;

        assert!(matches!(
            client.status().await,
            Err(CoreError::Unauthorized)
        ));
        let account = client
            .login(|prompt| {
                assert_eq!(prompt.verification_uri.path(), "/device");
                assert_eq!(prompt.user_code, "ABCD");
            })
            .await
            .unwrap();
        assert_eq!(account.workspace.id, "ws");
        assert_eq!(
            instance.manifest().unwrap().unwrap().account,
            Some(account.scope())
        );
        assert!(AccountClient::local_status(&instance, &namespaced)
            .unwrap()
            .is_some());
        assert!(!secrets.is_empty());
        drop(client);
        let list = async {
            let client = native_test_client(&instance, &namespaced, &server, false).await;
            client.hosts_for_device(None).await.unwrap()[0]
                .host_id
                .clone()
        };
        let status = async {
            let client = native_test_client(&instance, &namespaced, &server, false).await;
            client.status().await.unwrap().account.id
        };
        assert_eq!(tokio::join!(list, status), ("host-a".into(), "acct".into()));
        assert_eq!(refreshes.load(Ordering::SeqCst), 2);
        let client = native_test_client(&instance, &namespaced, &server, false).await;
        client.logout().await.unwrap();
        assert_eq!(revoked.load(Ordering::SeqCst), 1);
        assert!(secrets.is_empty());
        assert!(AccountClient::local_status(&instance, &namespaced)
            .unwrap()
            .is_none());
        assert_eq!(
            instance.manifest().unwrap().unwrap().account,
            Some(account.scope())
        );
        assert!(matches!(
            client.status().await,
            Err(CoreError::Unauthorized)
        ));
    }

    #[test]
    fn account_endpoints_reject_untrusted_browser_origins_and_credentials() {
        let issuer = Url::parse("https://issuer.example/oidc").unwrap();
        assert!(same_authority(
            &Url::parse("https://issuer.example/device").unwrap(),
            &issuer
        ));
        for invalid in [
            "https://another.example/device",
            "http://issuer.example/device",
            "https://user@issuer.example/device",
            "https://issuer.example/device#fragment",
        ] {
            assert!(!same_authority(&Url::parse(invalid).unwrap(), &issuer));
        }
    }
}
