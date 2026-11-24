//! Native App sessions owned by the configured Cloud account authority.

#[cfg(test)]
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use reqwest::{Client, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::{sleep, timeout_at, Instant};
use url::Url;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::api::instance::{AccountScope, Instance};
use crate::api::{CoreError, CoreResult, SecretStore};
use crate::security::device::DeviceIdentity;

const MAX_BODY: usize = 1024 * 1024;
const MAX_LOGIN: u64 = 20 * 60;
const ACCOUNT_SESSION: &str = "cloud-app-session";
/// Fixed native callback registered for the iOS authorization-code flow.
const IOS_REDIRECT_URI: &str = "whalerein://auth/callback";
/// Fixed Android App Link callback for the pinned local native client.
pub const ANDROID_REDIRECT_URI: &str = "https://app.whalerein.test/client/android/callback";
/// App Link callback registered on the staging console origin.
const ANDROID_STAGING_REDIRECT_URI: &str =
    "https://app.staging.whalerein.com/client/android/callback";
/// App Link callback registered on the production console origin.
const ANDROID_PRODUCTION_REDIRECT_URI: &str = "https://app.whalerein.com/client/android/callback";

/// The App Link callback registered for one official Cloud environment.
///
/// Cloud validates the exact redirect it is asked to use, so the client cannot
/// keep one fixed callback once more than the local environment is reachable.
pub fn android_redirect_uri(environment: &str) -> &'static str {
    match environment {
        "staging" => ANDROID_STAGING_REDIRECT_URI,
        "production" => ANDROID_PRODUCTION_REDIRECT_URI,
        _ => ANDROID_REDIRECT_URI,
    }
}

/// Host component of the callback for one official Cloud environment.
fn android_redirect_host(environment: &str) -> &'static str {
    android_redirect_uri(environment)
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
}

/// Native authorization-code platform. The platform decides the Cloud route,
/// the registered redirect and the callback shape; an attempt can only be
/// completed by its own platform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudAuthPlatform {
    #[default]
    Ios,
    Android,
}

impl CloudAuthPlatform {
    fn attempts_path(self) -> &'static str {
        match self {
            Self::Ios => "ios/attempts",
            Self::Android => "android/attempts",
        }
    }

    fn exchange_path(self) -> &'static str {
        match self {
            Self::Ios => "ios/exchange",
            Self::Android => "android/exchange",
        }
    }

    fn redirect_uri(self, environment: &str) -> &'static str {
        match self {
            Self::Ios => IOS_REDIRECT_URI,
            Self::Android => android_redirect_uri(environment),
        }
    }

    /// Reject a callback that is not the exact registered redirect for this
    /// platform before any attempt state is read.
    fn accepts_callback(self, parsed: &Url, environment: &str) -> bool {
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return false;
        }
        match self {
            Self::Ios => {
                parsed.scheme() == "whalerein"
                    && parsed.host_str() == Some("auth")
                    && parsed.path() == "/callback"
            }
            Self::Android => {
                parsed.scheme() == "https"
                    && parsed.host_str() == Some(android_redirect_host(environment))
                    && parsed.port_or_known_default() == Some(443)
                    && parsed.path() == "/client/android/callback"
            }
        }
    }

    /// Android pins the authorization page to the configured issuer authority;
    /// iOS keeps its existing hosted-page contract.
    fn validate_authorization_url(self, url: &Url, issuer: &Url) -> bool {
        match self {
            Self::Ios => true,
            Self::Android => same_authority(url, issuer),
        }
    }
}

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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub presence: String,
    #[serde(default)]
    pub observation: String,
    #[serde(default)]
    pub enrollment_state: String,
    #[serde(default)]
    pub pairing_eligible: bool,
    #[serde(default)]
    pub device_access: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSessionInfo {
    pub session_id: String,
    pub instance_id: String,
    pub expires_at: u64,
    pub state: AccountSessionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSessionState {
    Active,
    Pending,
    Revoked,
}

impl AccountSessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Revoked => "revoked",
        }
    }
}

pub struct AccountLoginPrompt {
    pub verification_uri: Url,
    pub user_code: String,
}

/// One native authorization-code attempt. The URL is the official hosted
/// sign-in page; the attempt identity, PKCE material, Cloud state and the
/// locally generated credential material never leave Core's SecretStore.
pub struct CloudAuthAttempt {
    pub url: String,
    pub attempt_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginPrompt {
    user_code: String,
    verification_uri: Url,
    expires_at: u64,
    interval_seconds: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveSession {
    session_id: String,
    expires_at: u64,
    #[serde(flatten)]
    account: SignedInAccount,
}

/// iOS exchange success. This is the same native app-session shape the device
/// grant returns, so Cloud never issues a token and the local credential stays
/// in the SecretStore.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IOSSessionResponse {
    state: String,
    session_id: String,
    expires_at: u64,
    account: AccountIdentity,
    workspace: AccountWorkspace,
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    id: String,
    credential: String,
    revocation: String,
    expires: u64,
    verified: SignedInAccount,
    regional: Option<RegionalSession>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RegionalSession {
    pub id: String,
    pub token: String,
    device_id: String,
    pub permission: Option<RegionalPermission>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RegionalPermission {
    pub region: String,
    #[serde(default)]
    pub revision: u64,
    pub access_origin: Url,
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct RevocationStatus {
    state: AccountSessionState,
}

#[derive(Clone, Serialize, Deserialize)]
struct Logout {
    id: String,
    revocation: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct LoginAttempt {
    credential: String,
    revocation: String,
    token: String,
    prompt: Option<LoginPrompt>,
}

/// Pending native authorization-code attempt. `state` is the exact value
/// Cloud embedded in the authorization URL; a callback carrying any other
/// value belongs to a cancelled, expired or different attempt and is rejected.
/// `platform` binds the Cloud route, redirect and callback shape.
#[derive(Clone, Serialize, Deserialize)]
struct IOSAttempt {
    attempt_id: String,
    credential: String,
    revocation: String,
    verifier: String,
    challenge: String,
    state: String,
    expires: u64,
    #[serde(default)]
    platform: CloudAuthPlatform,
}

#[derive(Default, Serialize, Deserialize)]
struct AccountState {
    active: Option<Session>,
    attempt: Option<LoginAttempt>,
    /// Added after the device flow shipped; older stored states remain readable.
    #[serde(default)]
    ios: Option<IOSAttempt>,
    logouts: Vec<Logout>,
}

pub struct AccountClient {
    http: Client,
    api: Url,
    issuer: Url,
    secrets: Arc<dyn SecretStore>,
    instance: Arc<Instance>,
}

impl AccountClient {
    pub(crate) async fn for_instance(
        instance: Arc<Instance>,
        secrets: Arc<dyn SecretStore>,
        create: bool,
        platform_roots: Option<Vec<u8>>,
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
        // The account endpoints follow the same two trust modes as Access:
        // platform verification by default, or exactly the configured CA.
        let tls = crate::access::tls::client_config(
            instance.ca_pem.as_deref(),
            platform_roots.as_deref(),
            &[],
        )
        .map_err(CoreError::from)?;
        let builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .use_preconfigured_tls((*tls).clone());
        Ok(Self {
            http: builder.build().map_err(|_| unavailable())?,
            api,
            issuer,
            secrets,
            instance,
        })
    }

    /// The official Cloud environment this client was opened for.
    fn cloud_environment(&self) -> &str {
        self.instance
            .cloud
            .as_ref()
            .map_or("local", |cloud| cloud.environment.as_str())
    }

    async fn lock(&self, name: &str) -> CoreResult<std::fs::File> {
        let instance = &self.instance;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(instance.runtime_dir().join(name))
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
        Ok(lock)
    }

    pub(crate) fn local_status(
        instance: &Instance,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<Option<SignedInAccount>> {
        let Some(manifest) = instance.manifest()? else {
            return Ok(None);
        };
        let Some(session) = load_state(secrets)?.active else {
            return Ok(None);
        };
        if session.expires <= unix_now() {
            return Ok(None);
        }
        let account = session.verified;
        if manifest.account.as_ref() != Some(&account.scope()) {
            return Err(CoreError::Unauthorized);
        }
        Ok(Some(account))
    }

    pub(crate) fn local_session_id(
        instance: &Instance,
        secrets: &Arc<dyn SecretStore>,
    ) -> CoreResult<Option<String>> {
        if instance.manifest()?.is_none() {
            return Ok(None);
        }
        Ok(load_state(secrets)?
            .active
            .filter(|session| session.expires > unix_now())
            .map(|session| session.id))
    }

    pub(crate) fn local_logout_pending(secrets: &Arc<dyn SecretStore>) -> CoreResult<bool> {
        Ok(!load_state(secrets)?.logouts.is_empty())
    }

    pub(crate) fn local_regional_session(
        secrets: &Arc<dyn SecretStore>,
        device_id: Option<&str>,
    ) -> CoreResult<Option<RegionalSession>> {
        Ok(load_state(secrets)?
            .active
            .filter(|session| session.expires > unix_now())
            .and_then(|session| session.regional)
            .filter(|regional| device_id.is_none_or(|device| regional.device_id == device)))
    }

    pub(crate) async fn regional_session(
        &self,
        identity: &DeviceIdentity,
    ) -> CoreResult<RegionalSession> {
        let (session, regional) = {
            let _lock = self.lock("account.lock").await?;
            let mut state = load_state(&self.secrets)?;
            let active = state
                .active
                .as_mut()
                .ok_or(CoreError::AuthenticationRequired)?;
            if active.expires <= unix_now() {
                return Err(CoreError::AuthenticationRequired);
            }
            verify_bound_scope(&self.instance, &active.verified)?;
            let created = active.regional.is_none();
            let regional = active.regional.get_or_insert_with(|| RegionalSession {
                id: active.id.clone(),
                token: new_token(),
                device_id: identity.device_id.clone(),
                permission: None,
            });
            if regional.device_id != identity.device_id {
                return Err(CoreError::Conflict(
                    "Cloud session belongs to a different Device".into(),
                ));
            }
            if regional.permission.as_ref().is_some_and(|permit| {
                permit.expires_at > unix_now().saturating_add(60)
                    || (permit.expires_at > unix_now() && permit.expires_at == active.expires)
            }) {
                return Ok(regional.clone());
            }
            let regional = regional.clone();
            let session = active.clone();
            if created {
                self.save(&state)?;
            }
            (session, regional)
        };

        self.retry_remote_signout().await?;
        let renewed = self
            .renew_region(identity, &regional.token, &session.credential)
            .await;
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        let active = state
            .active
            .as_mut()
            .filter(|active| active.id == session.id && active.expires > unix_now())
            .ok_or(CoreError::AuthenticationRequired)?;
        let expires = active.expires;
        let regional = active
            .regional
            .as_mut()
            .filter(|current| current.token == regional.token)
            .ok_or(CoreError::AuthenticationRequired)?;
        let permission = match renewed {
            Ok(permission) => permission,
            Err(error @ (CoreError::Timeout | CoreError::Unavailable(_))) => {
                if regional
                    .permission
                    .as_ref()
                    .is_some_and(|permit| permit.expires_at > unix_now())
                {
                    return Ok(regional.clone());
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if permission.expires_at <= unix_now()
            || permission.expires_at > expires
            || permission.region.is_empty()
            || permission.access_origin.scheme() != "https"
            || !permission.access_origin.username().is_empty()
            || permission.access_origin.password().is_some()
            || permission.access_origin.fragment().is_some()
            || permission.access_origin.query().is_some()
            || permission.access_origin.path() != "/"
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        if regional.permission.as_ref().is_none_or(|installed| {
            if installed.revision != 0 || permission.revision != 0 {
                installed.revision <= permission.revision
            } else {
                installed.expires_at <= permission.expires_at
            }
        }) {
            regional.permission = Some(permission);
        }
        let ready = regional.clone();
        self.save(&state)?;
        Ok(ready)
    }

    async fn renew_region(
        &self,
        identity: &DeviceIdentity,
        regional_token: &str,
        credential: &str,
    ) -> CoreResult<RegionalPermission> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Device<'a> {
            device_id: &'a str,
            device_noise_key: String,
            credential_digest: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Challenge {
            challenge_id: String,
            public_key: String,
            expires_at: u64,
        }
        let response = self
            .http
            .post(self.endpoint("session/permission/challenge")?)
            .bearer_auth(credential)
            .json(&Device {
                device_id: &identity.device_id,
                device_noise_key: URL_SAFE_NO_PAD.encode(identity.key.public_key()),
                credential_digest: token_digest(regional_token),
            })
            .send()
            .await
            .map_err(request_error)?;
        let challenge: Challenge =
            decode_scoped(self.check_active(response, credential).await?).await?;
        let key: [u8; 32] = URL_SAFE_NO_PAD
            .decode(challenge.public_key)
            .map_err(|_| CoreError::ProtocolIncompatible)?
            .try_into()
            .map_err(|_| CoreError::ProtocolIncompatible)?;
        if challenge.expires_at <= unix_now() || challenge.challenge_id.is_empty() {
            return Err(CoreError::ProtocolIncompatible);
        }
        let shared =
            StaticSecret::from(*identity.key.private_key()).diffie_hellman(&PublicKey::from(key));
        if !shared.was_contributory() {
            return Err(CoreError::ProtocolIncompatible);
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(shared.as_bytes())
            .map_err(|_| CoreError::ProtocolIncompatible)?;
        mac.update(b"whalerein/device-session/v1:");
        mac.update(challenge.challenge_id.as_bytes());
        let proof = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let response = self
            .http
            .post(self.endpoint("session/permission")?)
            .bearer_auth(credential)
            .json(&serde_json::json!({ "challengeId": challenge.challenge_id, "proof": proof }))
            .send()
            .await
            .map_err(request_error)?;
        decode_scoped(self.check_active(response, credential).await?).await
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
        let credential = self.access().await?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&credential)
            .send()
            .await
            .map_err(request_error)?;
        let listing: Listing =
            decode_success(self.check_active(response, &credential).await?).await?;
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
        self.post_scoped_for_session(segments, body, None).await
    }

    pub(crate) async fn post_scoped_for_session<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &impl Serialize,
        expected_session: Option<&str>,
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
        let credential = self.active_credential(expected_session).await?;
        let response = self
            .http
            .post(target)
            .bearer_auth(&credential)
            .json(body)
            .send()
            .await
            .map_err(request_error)?;
        decode_scoped(self.check_active(response, &credential).await?).await
    }

    pub async fn login(
        &self,
        present: impl FnOnce(AccountLoginPrompt),
    ) -> CoreResult<SignedInAccount> {
        let _login = self.lock("account-login.lock").await?;
        match self.status().await {
            Ok(account) => return Ok(account),
            Err(CoreError::Unauthorized | CoreError::AuthenticationRequired) => {}
            Err(error) => return Err(error),
        }
        let mut attempt = {
            let _lock = self.lock("account.lock").await?;
            let mut state = load_state(&self.secrets)?;
            if state.attempt.as_ref().is_none_or(|attempt| {
                attempt
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.expires_at <= unix_now())
            }) {
                state.attempt = Some(LoginAttempt {
                    credential: new_token(),
                    revocation: new_token(),
                    token: new_token(),
                    prompt: None,
                });
                self.save(&state)?;
            }
            state.attempt.expect("login attempt allocated")
        };
        if attempt.prompt.is_none() {
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct StartLogin<'a> {
                instance_id: &'a str,
                credential_digest: String,
                revocation_digest: String,
                attempt_digest: String,
            }

            let manifest = self.instance.manifest()?.ok_or(CoreError::Unauthorized)?;
            let body = StartLogin {
                instance_id: &manifest.id,
                credential_digest: token_digest(&attempt.credential),
                revocation_digest: token_digest(&attempt.revocation),
                attempt_digest: token_digest(&attempt.token),
            };
            let response = self
                .http
                .post(self.endpoint("login")?)
                .json(&body)
                .send()
                .await
                .map_err(request_error)?;
            if matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ) {
                return Err(CoreError::ProtocolIncompatible);
            }
            let prompt: LoginPrompt = decode_scoped(response).await?;
            if !same_authority(&prompt.verification_uri, &self.issuer)
                || prompt.expires_at <= unix_now()
                || prompt.user_code.is_empty()
                || prompt.user_code.len() > 128
                || prompt.user_code.chars().any(char::is_control)
            {
                return Err(CoreError::ProtocolIncompatible);
            }
            attempt.prompt = Some(prompt);
            let _lock = self.lock("account.lock").await?;
            let mut state = load_state(&self.secrets)?;
            if state
                .attempt
                .as_ref()
                .is_none_or(|pending| pending.token != attempt.token)
            {
                return Err(CoreError::Unauthorized);
            }
            state.attempt = Some(attempt.clone());
            self.save(&state)?;
        }

        let prompt = attempt.prompt.as_ref().expect("login prompt allocated");
        let end = Instant::now()
            + Duration::from_secs(prompt.expires_at.saturating_sub(unix_now()).min(MAX_LOGIN));
        present(AccountLoginPrompt {
            verification_uri: prompt.verification_uri.clone(),
            user_code: prompt.user_code.clone(),
        });
        let result = timeout_at(end, self.poll(&attempt.token, prompt.interval_seconds))
            .await
            .map_err(|_| CoreError::Timeout)?;
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        let current = state
            .attempt
            .as_ref()
            .is_some_and(|pending| pending.token == attempt.token);
        let active = match result {
            Ok(active) => active,
            Err(error @ (CoreError::Unauthorized | CoreError::Timeout)) => {
                if current {
                    state.attempt = None;
                    self.save(&state)?;
                }
                return Err(error);
            }
            Err(other) => return Err(other),
        };
        self.validate_active(&active)?;
        if !current {
            state.logouts.push(Logout {
                id: active.session_id,
                revocation: attempt.revocation,
            });
            self.save(&state)?;
            return Err(CoreError::Unauthorized);
        }
        self.instance.bind_account(&active.account.scope())?;
        state.attempt = None;
        let verified = active.account;
        state.active = Some(Session {
            id: active.session_id,
            credential: attempt.credential,
            revocation: attempt.revocation,
            expires: active.expires_at,
            verified: verified.clone(),
            regional: None,
        });
        self.save(&state)?;
        Ok(verified)
    }

    /// Begin one native iOS authorization-code attempt.
    ///
    /// Core generates the attempt ID, PKCE verifier/challenge and the local
    /// credential/revocation pair, asks Cloud for the hosted URL, then retains
    /// the URL's state and deadline with the attempt in the SecretStore. Any
    /// previous pending attempt is replaced, so its late callback cannot win.
    pub async fn auth_start(&self) -> CoreResult<CloudAuthAttempt> {
        self.auth_start_platform(CloudAuthPlatform::Ios).await
    }

    /// Begin one native authorization-code attempt for `platform`.
    pub async fn auth_start_platform(
        &self,
        platform: CloudAuthPlatform,
    ) -> CoreResult<CloudAuthAttempt> {
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        if state
            .ios
            .as_ref()
            .is_some_and(|attempt| attempt.expires <= unix_now())
        {
            state.ios = None;
        }
        let attempt_id = hex::encode(crate::util::random_bytes(16));
        let credential = new_token();
        let revocation = new_token();
        let verifier = new_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Begin<'a> {
            attempt_id: &'a str,
            credential_digest: String,
            revocation_digest: String,
            code_challenge: &'a str,
            code_challenge_method: &'a str,
            redirect_uri: &'a str,
            /// Android Cloud routes bind the attempt to the app instance.
            /// iOS keeps its existing body.
            #[serde(skip_serializing_if = "Option::is_none")]
            instance_id: Option<&'a str>,
        }
        let manifest = self.instance.manifest()?;
        let android_instance = match platform {
            CloudAuthPlatform::Android => Some(
                manifest
                    .as_ref()
                    .map(|manifest| manifest.id.as_str())
                    .ok_or(CoreError::Unauthorized)?,
            ),
            CloudAuthPlatform::Ios => None,
        };
        let response = self
            .http
            .post(self.endpoint(platform.attempts_path())?)
            .json(&Begin {
                attempt_id: &attempt_id,
                credential_digest: token_digest(&credential),
                revocation_digest: token_digest(&revocation),
                code_challenge: &challenge,
                code_challenge_method: "S256",
                redirect_uri: platform.redirect_uri(self.cloud_environment()),
                instance_id: android_instance,
            })
            .send()
            .await
            .map_err(request_error)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BeginResponse {
            authorization_url: String,
            expires_at: u64,
        }
        let begin: BeginResponse = decode_created(response).await?;
        let url =
            Url::parse(&begin.authorization_url).map_err(|_| CoreError::ProtocolIncompatible)?;
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !platform.validate_authorization_url(&url, &self.issuer)
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        let cloud_state = single_query_value(&url, "state")
            .filter(|value| !value.is_empty() && value.len() <= 512)
            .ok_or(CoreError::ProtocolIncompatible)?;
        let now = unix_now();
        if begin.expires_at <= now || begin.expires_at > now.saturating_add(MAX_LOGIN) {
            return Err(CoreError::ProtocolIncompatible);
        }
        state.ios = Some(IOSAttempt {
            attempt_id: attempt_id.clone(),
            credential,
            revocation,
            verifier,
            challenge,
            state: cloud_state,
            expires: begin.expires_at,
            platform,
        });
        self.save(&state)?;
        Ok(CloudAuthAttempt {
            url: url.into(),
            attempt_id,
        })
    }

    /// Complete the exact pending iOS attempt from one callback URL.
    ///
    /// The attempt leaves the pending slot only after Cloud returns a valid
    /// session, so an expired, cancelled, replayed or mismatched callback leaves
    /// the instance signed out (or in its prior verified state) and can never
    /// establish a session. The URL's raw code is never persisted or logged.
    pub async fn auth_complete(&self, callback_url: &str) -> CoreResult<SignedInAccount> {
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        let Some(attempt) = state.ios.clone() else {
            return Err(CoreError::Unauthorized);
        };
        let platform = attempt.platform;
        let parsed = Url::parse(callback_url)
            .map_err(|_| CoreError::InvalidInput("invalid Cloud sign-in callback".into()))?;
        if !platform.accepts_callback(&parsed, self.cloud_environment()) {
            return Err(CoreError::Unauthorized);
        }
        if parsed
            .query_pairs()
            .any(|(key, _)| key == "error" || key == "error_description")
        {
            return Err(CoreError::Unauthorized);
        }
        let code = single_query_value(&parsed, "code")
            .filter(|value| !value.is_empty() && value.len() <= 4096)
            .ok_or(CoreError::Unauthorized)?;
        let cloud_state = single_query_value(&parsed, "state")
            .filter(|value| !value.is_empty() && value.len() <= 512)
            .ok_or(CoreError::Unauthorized)?;
        if attempt.expires <= unix_now() {
            state.ios = None;
            self.save(&state)?;
            return Err(CoreError::Unauthorized);
        }
        if !same_secret(&attempt.state, &cloud_state)
            || URL_SAFE_NO_PAD.encode(Sha256::digest(attempt.verifier.as_bytes()))
                != attempt.challenge
        {
            return Err(CoreError::Unauthorized);
        }

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Exchange<'a> {
            attempt_id: &'a str,
            code: &'a str,
            state: &'a str,
            code_verifier: &'a str,
        }
        let response = self
            .http
            .post(self.endpoint(platform.exchange_path())?)
            .json(&Exchange {
                attempt_id: &attempt.attempt_id,
                code: &code,
                state: &cloud_state,
                code_verifier: &attempt.verifier,
            })
            .send()
            .await
            .map_err(request_error)?;
        let active: IOSSessionResponse = match decode_scoped(response).await {
            Ok(session) => session,
            Err(error @ (CoreError::Timeout | CoreError::Unavailable(_))) => {
                // The exchange may have committed the App Session while its
                // response was lost. Keep the pending material so recovery can
                // use the original credential and never replay the code.
                return Err(error);
            }
            Err(error) => {
                // Cloud consumed the attempt even for a rejected exchange. Drop
                // it so the same attempt can never be retried.
                state.ios = None;
                self.save(&state)?;
                return Err(error);
            }
        };
        if active.state != "active"
            || active.expires_at <= unix_now()
            || !valid_session_id(&active.session_id)
            || active.account.id.is_empty()
            || active.account.issuer.is_empty()
            || active.account.subject.is_empty()
            || active.workspace.id.is_empty()
            || Url::parse(&active.account.issuer).is_err()
        {
            state.ios = None;
            self.save(&state)?;
            return Err(CoreError::ProtocolIncompatible);
        }
        let verified = SignedInAccount {
            account: active.account,
            workspace: active.workspace,
        };
        // The instance manifest is the local scope authority. A response for a
        // different account or workspace cannot replace an existing binding.
        if let Err(error) = self.instance.bind_account(&verified.scope()) {
            state.ios = None;
            self.save(&state)?;
            return Err(error);
        }
        state.ios = None;
        state.active = Some(Session {
            id: active.session_id,
            credential: attempt.credential,
            revocation: attempt.revocation,
            expires: active.expires_at,
            verified: verified.clone(),
            regional: None,
        });
        self.save(&state)?;
        Ok(verified)
    }

    /// Recover an exchange whose response was lost after Cloud may have
    /// committed the App Session. Uses only the attempt's original credential
    /// with the scoped `/me`, clears the attempt on success and never replays
    /// the authorization code. An inconclusive lookup keeps the attempt.
    pub async fn auth_recover(&self) -> CoreResult<SignedInAccount> {
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        let Some(attempt) = state.ios.clone() else {
            return Err(CoreError::Unauthorized);
        };
        if attempt.expires <= unix_now() {
            state.ios = None;
            self.save(&state)?;
            return Err(CoreError::Unauthorized);
        }
        let response = self
            .http
            .get(self.endpoint("me")?)
            .bearer_auth(&attempt.credential)
            .send()
            .await;
        let active: ActiveSession = match response {
            Ok(response) => match decode_success(response).await {
                Ok(active) => active,
                Err(error @ (CoreError::Timeout | CoreError::Unavailable(_))) => {
                    return Err(error);
                }
                // A definitive rejection means no session was committed under
                // the original attempt credential yet. Keep the attempt so the
                // callback path can still complete or cancel it.
                Err(error) => return Err(error),
            },
            Err(error) => return Err(request_error(error)),
        };
        if active.expires_at <= unix_now()
            || !valid_session_id(&active.session_id)
            || active.account.account.id.is_empty()
            || active.account.account.issuer.is_empty()
            || active.account.account.subject.is_empty()
            || active.account.workspace.id.is_empty()
            || Url::parse(&active.account.account.issuer).is_err()
        {
            return Err(CoreError::ProtocolIncompatible);
        }
        let verified = active.account;
        self.instance.bind_account(&verified.scope())?;
        state.ios = None;
        state.active = Some(Session {
            id: active.session_id,
            credential: attempt.credential,
            revocation: attempt.revocation,
            expires: active.expires_at,
            verified: verified.clone(),
            regional: None,
        });
        self.save(&state)?;
        Ok(verified)
    }

    /// Cancel the pending iOS attempt. A callback that arrives afterwards can
    /// no longer complete the exchange locally.
    pub async fn auth_cancel(&self) -> CoreResult<()> {
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        state.ios = None;
        self.save(&state)
    }

    pub async fn status(&self) -> CoreResult<SignedInAccount> {
        self.retry_remote_signout().await?;
        let session = load_state(&self.secrets)?
            .active
            .ok_or(CoreError::Unauthorized)?;
        let active: ActiveSession = self
            .get("api/v1/client/me", Some(&session.credential))
            .await?;
        self.validate_active(&active)?;
        if session.id != active.session_id {
            return Err(CoreError::ProtocolIncompatible);
        }
        verify_bound_scope(&self.instance, &active.account)?;
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        let session = state
            .active
            .as_mut()
            .filter(|current| current.id == session.id)
            .ok_or(CoreError::AuthenticationRequired)?;
        session.expires = active.expires_at;
        session.verified = active.account.clone();
        self.save(&state)?;
        Ok(active.account)
    }

    pub(crate) async fn sessions(&self) -> CoreResult<Vec<AccountSessionInfo>> {
        #[derive(Deserialize)]
        struct Listing {
            sessions: Vec<AccountSessionInfo>,
        }
        let credential = self.access().await?;
        let listing: Listing = self
            .get("api/v1/client/sessions", Some(&credential))
            .await?;
        Ok(listing.sessions)
    }

    pub(crate) async fn revoke_session(&self, session_id: &str) -> CoreResult<AccountSessionState> {
        if !valid_session_id(session_id) {
            return Err(CoreError::InvalidInput("invalid Cloud session ID".into()));
        }
        if self.begin_local_logout(Some(session_id)).await? {
            self.retry_remote_signout().await?;
            let pending = load_state(&self.secrets)?
                .logouts
                .iter()
                .any(|logout| logout.id == session_id);
            return Ok(if !pending {
                AccountSessionState::Revoked
            } else {
                AccountSessionState::Pending
            });
        }
        let response = self
            .http
            .post(self.endpoint(&format!("sessions/{session_id}/revoke"))?)
            .bearer_auth(self.access().await?)
            .send()
            .await
            .map_err(request_error)?;
        let result: RevocationStatus = decode_scoped(response).await?;
        Ok(result.state)
    }

    /// Revoke one client installation and every Host grant it owns.
    pub(crate) async fn revoke_client_installation(
        &self,
        client_id: &str,
        operation_id: &str,
    ) -> CoreResult<String> {
        #[derive(Deserialize)]
        struct Status {
            state: String,
        }
        let response = self
            .http
            .post(self.endpoint(&format!("clients/{client_id}/revoke"))?)
            .bearer_auth(self.access().await?)
            .json(&serde_json::json!({ "operationId": operation_id }))
            .send()
            .await
            .map_err(request_error)?;
        let status: Status = decode_scoped(response).await?;
        Ok(status.state)
    }

    /// Replace the Noise key of an explicitly revoked client installation.
    pub(crate) async fn rotate_client_installation(
        &self,
        client_id: &str,
        operation_id: &str,
        public_key: &[u8; 32],
    ) -> CoreResult<String> {
        #[derive(Deserialize)]
        struct Status {
            state: String,
        }
        let response = self
            .http
            .post(self.endpoint(&format!("clients/{client_id}/keys"))?)
            .bearer_auth(self.access().await?)
            .json(&serde_json::json!({
                "operationId": operation_id,
                "deviceNoiseKey": URL_SAFE_NO_PAD.encode(public_key),
            }))
            .send()
            .await
            .map_err(request_error)?;
        let status: Status = decode_scoped(response).await?;
        Ok(status.state)
    }

    pub(crate) async fn revoke_all_sessions(&self) -> CoreResult<AccountSessionState> {
        let credential = self.access().await?;
        let response = self
            .http
            .post(self.endpoint("session-revocations")?)
            .bearer_auth(&credential)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        let rejected = response
            .as_ref()
            .is_ok_and(|response| response.status().is_client_error());
        let result = match response {
            Ok(response) => decode_scoped::<RevocationStatus>(response)
                .await
                .map(|value| value.state),
            Err(error) => Err(request_error(error)),
        };
        if rejected {
            return result;
        }
        // An ambiguous result can mean Cloud committed the account-wide
        // revocation. End the initiating login locally and reconcile its
        // exact ID with the separate revocation credential.
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        if let Some(active) = state
            .active
            .take_if(|active| active.credential == credential)
        {
            if !matches!(result, Ok(AccountSessionState::Revoked)) {
                state.logouts.push(Logout {
                    id: active.id,
                    revocation: active.revocation,
                });
            }
            self.save(&state)?;
        }
        result.map_err(|_| CoreError::Unavailable(
            "Account-wide sign-out could not be confirmed. Check your other app sessions from the console.".into(),
        ))
    }

    pub async fn logout(&self) -> CoreResult<bool> {
        self.begin_local_logout(None).await?;
        self.retry_remote_signout().await?;
        Ok(!Self::local_logout_pending(&self.secrets)?)
    }

    async fn begin_local_logout(&self, expected: Option<&str>) -> CoreResult<bool> {
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        if expected.is_some_and(|id| state.active.as_ref().is_none_or(|active| active.id != id)) {
            return Ok(false);
        }
        if let Some(session) = state.active.take() {
            state.logouts.push(Logout {
                id: session.id,
                revocation: session.revocation,
            });
        }
        state.attempt = None;
        self.save(&state)?;
        Ok(true)
    }

    pub(crate) async fn retry_remote_signout(&self) -> CoreResult<()> {
        let logouts = load_state(&self.secrets)?.logouts;
        for logout in logouts {
            let url = self.endpoint(&format!("sessions/{}/revoke", logout.id))?;
            let response = self
                .http
                .post(url)
                .bearer_auth(&logout.revocation)
                .timeout(Duration::from_secs(1))
                .send()
                .await;
            let complete = match response {
                Ok(response) if response.status() == StatusCode::OK => {
                    decode::<RevocationStatus>(response)
                        .await
                        .is_ok_and(|status| status.state == AccountSessionState::Revoked)
                }
                _ => false,
            };
            if complete {
                let _lock = self.lock("account.lock").await?;
                let mut state = load_state(&self.secrets)?;
                state.logouts.retain(|pending| pending.id != logout.id);
                self.save(&state)?;
            }
        }
        Ok(())
    }

    async fn poll(&self, attempt: &str, mut interval: u64) -> CoreResult<ActiveSession> {
        loop {
            sleep(Duration::from_secs(interval.clamp(1, MAX_LOGIN))).await;
            let response = self
                .http
                .post(self.endpoint("login/poll")?)
                .bearer_auth(attempt)
                .send()
                .await
                .map_err(request_error)?;
            match response.status() {
                StatusCode::ACCEPTED => {
                    #[derive(Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Pending {
                        interval_seconds: u64,
                    }
                    interval = decode::<Pending>(response).await?.interval_seconds;
                }
                StatusCode::GONE => return Err(CoreError::Timeout),
                _ => return decode_scoped(response).await,
            }
        }
    }

    fn validate_active(&self, active: &ActiveSession) -> CoreResult<()> {
        if active.expires_at <= unix_now() || !valid_session_id(&active.session_id) {
            return Err(CoreError::ProtocolIncompatible);
        }
        Ok(())
    }

    async fn access(&self) -> CoreResult<String> {
        self.active_credential(None).await
    }

    async fn active_credential(&self, expected: Option<&str>) -> CoreResult<String> {
        self.retry_remote_signout().await?;
        let session = load_state(&self.secrets)?
            .active
            .ok_or(CoreError::AuthenticationRequired)?;
        verify_bound_scope(&self.instance, &session.verified)?;
        if session.expires <= unix_now() || expected.is_some_and(|id| id != session.id) {
            return Err(CoreError::AuthenticationRequired);
        }
        Ok(session.credential)
    }

    fn save(&self, state: &AccountState) -> CoreResult<()> {
        let bytes = serde_json::to_vec(state)
            .map_err(|_| CoreError::Internal("could not encode account state".into()))?;
        self.secrets.save(ACCOUNT_SESSION, &bytes)?;
        Ok(())
    }

    async fn check_active(&self, response: Response, credential: &str) -> CoreResult<Response> {
        let state = load_state(&self.secrets)?;
        let current = state
            .active
            .as_ref()
            .is_some_and(|active| active.credential == credential);
        if !current {
            return Err(CoreError::AuthenticationRequired);
        }
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        let _lock = self.lock("account.lock").await?;
        let mut state = load_state(&self.secrets)?;
        if let Some(session) = state
            .active
            .take_if(|active| active.credential == credential)
        {
            state.logouts.push(Logout {
                id: session.id,
                revocation: session.revocation,
            });
            self.save(&state)?;
        }
        Err(CoreError::AuthenticationRequired)
    }

    fn endpoint(&self, relative: &str) -> CoreResult<Url> {
        self.api
            .join(&format!("api/v1/client/{relative}"))
            .map_err(|_| CoreError::ProtocolIncompatible)
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
        let response = request.send().await.map_err(request_error)?;
        let response = match token {
            Some(credential) => self.check_active(response, credential).await?,
            None => response,
        };
        decode_success(response).await
    }
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(crate::util::random_bytes(32))
}

fn token_digest(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

/// Constant-time comparison of two callback/attempt values. Length differences
/// are not secret for the fixed-format state and attempt values.
fn same_secret(first: &str, second: &str) -> bool {
    if first.len() != second.len() {
        return false;
    }
    first
        .bytes()
        .zip(second.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Reads one query value, rejecting duplicates so an ambiguous callback cannot
/// smuggle two different `state` or `code` values past validation.
fn single_query_value(url: &Url, name: &str) -> Option<String> {
    let mut value = None;
    for (key, candidate) in url.query_pairs() {
        if key != name {
            continue;
        }
        if value.is_some() || candidate.chars().any(char::is_control) {
            return None;
        }
        value = Some(candidate.into_owned());
    }
    value
}

/// Decodes a Cloud `201 Created` response. Other statuses map to the same
/// stable families the native device flow uses.
async fn decode_created<T: DeserializeOwned>(response: Response) -> CoreResult<T> {
    match response.status() {
        StatusCode::CREATED => decode(response).await,
        StatusCode::BAD_REQUEST => Err(CoreError::InvalidInput(
            "the Cloud sign-in request was rejected".into(),
        )),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(CoreError::Unauthorized),
        _ => Err(unavailable()),
    }
}

fn load_state(secrets: &Arc<dyn SecretStore>) -> CoreResult<AccountState> {
    secrets
        .load(ACCOUNT_SESSION)?
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|_| CoreError::Security("stored account state is invalid".into()))
        })
        .transpose()
        .map(Option::unwrap_or_default)
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
        Some("client_upgrade_required") => CoreError::ProtocolIncompatible,
        _ if status == StatusCode::UPGRADE_REQUIRED => CoreError::ProtocolIncompatible,
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
        let mut client =
            AccountClient::for_instance(instance.clone(), secrets.clone(), create, None)
                .await
                .unwrap();
        client.api = server.url();
        client.issuer = server.url().join("oidc").unwrap();
        client
    }

    #[tokio::test]
    async fn account_device_flow_denial_expiry_and_cancellation_leave_no_active_session() {
        for outcome in ["denied", "expired", "cancelled"] {
            let cancel = outcome == "cancelled";
            let status = if outcome == "expired" { 410 } else { 401 };
            let server = MockServer::start(Arc::new(move |request| {
                let origin = format!("http://{}", request.headers["host"]);
                match (request.method.as_str(), request.path.as_str()) {
                    ("POST", "/api/v1/client/login") => MockResponse::json(
                        200,
                        &json!({
                            "verificationUri": format!("{origin}/device"), "userCode": "ABCD",
                            "expiresAt": unix_now() + 60, "intervalSeconds": 1,
                        }),
                    ),
                    ("POST", "/api/v1/client/login/poll") => MockResponse::empty(status),
                    _ => MockResponse::empty(404),
                }
            }))
            .await;
            let directory = tempfile::tempdir().unwrap();
            let (instance, secrets, _) = native_test_instance(directory.path());
            let client = native_test_client(&instance, &secrets, &server, true).await;
            let (presented, prompt) = tokio::sync::oneshot::channel();
            let login =
                tokio::spawn(
                    async move { client.login(|_| assert!(presented.send(()).is_ok())).await },
                );
            prompt.await.unwrap();
            if cancel {
                login.abort();
                assert!(matches!(login.await, Err(error) if error.is_cancelled()));
            } else if outcome == "expired" {
                assert!(matches!(login.await.unwrap(), Err(CoreError::Timeout)));
            } else {
                assert!(matches!(login.await.unwrap(), Err(CoreError::Unauthorized)));
            }
            assert!(
                AccountClient::local_status(&instance, &secrets)
                    .unwrap()
                    .is_none(),
                "{outcome} issues no usable App session"
            );
            assert!(instance.manifest().unwrap().unwrap().account.is_none());
            assert_eq!(load_state(&secrets).unwrap().attempt.is_some(), cancel);
            assert!(!AccountClient::local_logout_pending(&secrets).unwrap());
        }
    }

    #[tokio::test]
    async fn a_fresh_approved_device_flow_succeeds_after_a_denied_attempt() {
        let polls = Arc::new(AtomicUsize::new(0));
        let counted = polls.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let origin = format!("http://{}", request.headers["host"]);
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/login") => MockResponse::json(
                    200,
                    &json!({
                        "verificationUri": format!("{origin}/device"), "userCode": "ABCD",
                        "expiresAt": unix_now() + 60, "intervalSeconds": 1,
                    }),
                ),
                ("POST", "/api/v1/client/login/poll") => {
                    if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                        MockResponse::empty(401)
                    } else {
                        MockResponse::json(200, &json!({
                            "state": "active", "sessionId": "fresh-session", "expiresAt": unix_now() + 600,
                            "account": {"id":"acct","email":"","issuer":"https://login.whalerein.com/oidc","subject":"subject"},
                            "workspace": {"id":"ws"},
                        }))
                    }
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let directory = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(directory.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        assert!(matches!(
            client.login(|_| {}).await,
            Err(CoreError::Unauthorized)
        ));
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());
        // The same local instance and its device material survive and a fresh
        // authorized attempt succeeds.
        client.login(|_| {}).await.unwrap();
        assert_eq!(
            AccountClient::local_session_id(&instance, &secrets)
                .unwrap()
                .as_deref(),
            Some("fresh-session")
        );
        assert!(instance.manifest().unwrap().unwrap().account.is_some());
        assert!(!AccountClient::local_logout_pending(&secrets).unwrap());
    }

    #[tokio::test]
    async fn account_device_flow_keeps_separate_credentials_and_retries_signout() {
        let starts = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
        let revocations = Arc::new(AtomicUsize::new(0));
        let recorded = starts.clone();
        let counted = revocations.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let origin = format!("http://{}", request.headers["host"]);
            let active = || json!({
                "state": "active", "sessionId": "fixture-session", "expiresAt": unix_now() + 600,
                "account": {"id":"acct","email":"","issuer":"https://login.whalerein.com/oidc","subject":"subject"},
                "workspace": {"id":"ws"},
            });
            let bearer_digest = || token_digest(request.headers["authorization"].strip_prefix("Bearer ").unwrap());
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/login") => {
                    let body = serde_json::from_slice(&request.body).unwrap();
                    *recorded.lock().unwrap() = Some(body);
                    MockResponse::json(200, &json!({
                        "verificationUri": format!("{origin}/device"), "userCode": "ABCD",
                        "expiresAt": unix_now() + 60, "intervalSeconds": 1,
                    }))
                }
                ("POST", "/api/v1/client/login/poll") => {
                    assert_eq!(recorded.lock().unwrap().as_ref().unwrap()["attemptDigest"], bearer_digest());
                    MockResponse::json(200, &active())
                }
                ("GET", "/api/v1/client/me") => {
                    assert_eq!(recorded.lock().unwrap().as_ref().unwrap()["credentialDigest"], bearer_digest());
                    MockResponse::json(200, &active())
                }
                ("GET", "/api/v1/client/hosts") => {
                    assert_eq!(recorded.lock().unwrap().as_ref().unwrap()["credentialDigest"], bearer_digest());
                    MockResponse::json(200, &json!({
                        "workspace": {"id": "ws"},
                        "hosts": [{"hostId":"host-a","displayName":"Computer","enrolled":true,"online":false}],
                    }))
                }
                ("POST", "/api/v1/client/sessions/fixture-session/revoke") => {
                    assert_eq!(recorded.lock().unwrap().as_ref().unwrap()["revocationDigest"], bearer_digest());
                    match counted.fetch_add(1, Ordering::SeqCst) {
                        0 => MockResponse::empty(503),
                        1 => MockResponse::json(202, &json!({"state":"pending"})),
                        _ => MockResponse::json(200, &json!({"state":"revoked"})),
                    }
                }
                _ => MockResponse::empty(404),
            }
        })).await;

        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let account = client
            .login(|prompt| {
                assert_eq!(prompt.verification_uri.path(), "/device");
                assert_eq!(prompt.user_code, "ABCD");
            })
            .await
            .unwrap();
        assert_eq!(account.workspace.id, "ws");
        client
            .login(|_| panic!("existing session should not prompt"))
            .await
            .unwrap();
        assert_eq!(
            client.hosts_for_device(None).await.unwrap()[0].host_id,
            "host-a"
        );
        let state = load_state(&secrets).unwrap();
        let session = state.active.unwrap();
        assert!(session.credential != session.revocation);
        assert!(!client.logout().await.unwrap());
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());
        assert!(AccountClient::local_logout_pending(&secrets).unwrap());
        assert!(
            !String::from_utf8(secrets.load(ACCOUNT_SESSION).unwrap().unwrap())
                .unwrap()
                .contains(&session.credential)
        );
        drop(client);

        let reopened = native_test_client(&instance, &secrets, &server, false).await;
        assert!(!reopened.logout().await.unwrap());
        assert!(reopened.logout().await.unwrap());
        assert!(!AccountClient::local_logout_pending(&secrets).unwrap());
        assert_eq!(revocations.load(Ordering::SeqCst), 3);
    }

    fn seed_session(client: &AccountClient, id: &str) -> Session {
        let account = SignedInAccount {
            account: AccountIdentity {
                id: "account".into(),
                email: String::new(),
                subject: "subject".into(),
                issuer: client
                    .instance
                    .cloud
                    .as_ref()
                    .unwrap()
                    .definition
                    .issuer
                    .clone()
                    .unwrap(),
            },
            workspace: AccountWorkspace {
                id: "workspace".into(),
            },
        };
        client.instance.bind_account(&account.scope()).unwrap();
        Session {
            id: id.into(),
            credential: new_token(),
            revocation: new_token(),
            expires: unix_now() + 600,
            verified: account,
            regional: None,
        }
    }

    #[tokio::test]
    async fn account_regional_permission_proves_device_key_and_keeps_one_token() {
        let device = DeviceIdentity {
            device_id: "fixture-device".into(),
            key: crate::security::StaticIdentity::generate(),
        };
        let device_key = *device.key.public_key();
        let server_secret = StaticSecret::from([41u8; 32]);
        let server_key = URL_SAFE_NO_PAD.encode(PublicKey::from(&server_secret).as_bytes());
        let secret = server_secret.diffie_hellman(&PublicKey::from(device_key));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"whalerein/device-session/v1:fixture-challenge");
        let expected_proof = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let digests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sent = digests.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            match request.path.as_str() {
                "/api/v1/client/session/permission/challenge" => {
                    assert_eq!(body["deviceId"], "fixture-device");
                    assert_eq!(body["deviceNoiseKey"], URL_SAFE_NO_PAD.encode(device_key));
                    sent.lock().unwrap().push(body["credentialDigest"].clone());
                    MockResponse::json(
                        200,
                        &json!({
                            "challengeId":"fixture-challenge",
                            "publicKey":server_key,
                            "expiresAt":unix_now() + 60,
                        }),
                    )
                }
                "/api/v1/client/session/permission" => {
                    assert_eq!(body["challengeId"], "fixture-challenge");
                    assert!(body["proof"] == expected_proof);
                    MockResponse::json(
                        200,
                        &json!({
                            "region":"fixture-region",
                            "accessOrigin":"https://access.fixture.test",
                            "expiresAt":unix_now() + 45,
                        }),
                    )
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        client
            .save(&AccountState {
                active: Some(seed_session(&client, "fixture-session")),
                ..AccountState::default()
            })
            .unwrap();

        let first = client.regional_session(&device).await.unwrap();
        let renewed = client.regional_session(&device).await.unwrap();
        assert_eq!(first.id, renewed.id);
        assert!(first.token == renewed.token);
        {
            let seen = digests.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert!(seen
                .iter()
                .all(|digest| *digest == token_digest(&first.token)));
        }
        assert!(!client.logout().await.unwrap());
        assert!(
            AccountClient::local_regional_session(&secrets, Some("fixture-device"))
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn account_regional_permission_uses_cache_during_outage_and_rejects_stale_authority() {
        let response_kind = Arc::new(AtomicUsize::new(0));
        let server_kind = response_kind.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let kind = server_kind.load(Ordering::SeqCst);
            match request.path.as_str() {
                "/api/v1/client/session/permission/challenge" if kind == 0 => MockResponse::empty(503),
                "/api/v1/client/session/permission/challenge" => {
                    let key = if kind == 3 {
                        [0; 32]
                    } else {
                        PublicKey::from(&StaticSecret::from([62; 32])).to_bytes()
                    };
                    MockResponse::json(200, &json!({
                        "challengeId":"fixture-challenge",
                        "publicKey": URL_SAFE_NO_PAD.encode(key),
                        "expiresAt": unix_now() + 60,
                    }))
                }
                "/api/v1/client/session/permission" => MockResponse::json(200, &json!({
                    "region":"another-region",
                    "accessOrigin": if kind == 2 { "http://access.fixture.test" } else { "https://standby.fixture.test" },
                    "expiresAt":unix_now()+45,
                    "revision":9,
                })),
                _ => MockResponse::empty(404),
            }
        })).await;
        let directory = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(directory.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let identity = DeviceIdentity {
            device_id: "fixture-device".into(),
            key: crate::security::StaticIdentity::generate(),
        };
        let mut active = seed_session(&client, "fixture-session");
        let mut regional = RegionalSession {
            id: active.id.clone(),
            token: new_token(),
            device_id: identity.device_id.clone(),
            permission: Some(RegionalPermission {
                region: "current-region".into(),
                revision: 10,
                access_origin: Url::parse("https://access.fixture.test").unwrap(),
                expires_at: unix_now() + 30,
            }),
        };
        active.regional = Some(regional.clone());
        client
            .save(&AccountState {
                active: Some(active.clone()),
                ..AccountState::default()
            })
            .unwrap();
        let cached = client.regional_session(&identity).await.unwrap();
        assert_eq!(cached.permission.unwrap().region, "current-region");

        response_kind.store(1, Ordering::SeqCst);
        let renewed = client.regional_session(&identity).await.unwrap();
        assert_eq!(renewed.permission.unwrap().revision, 10);

        for kind in [2, 3] {
            response_kind.store(kind, Ordering::SeqCst);
            assert!(matches!(
                client.regional_session(&identity).await,
                Err(CoreError::ProtocolIncompatible)
            ));
        }
        let other = DeviceIdentity {
            device_id: "other-device".into(),
            key: crate::security::StaticIdentity::generate(),
        };
        assert!(matches!(
            client.regional_session(&other).await,
            Err(CoreError::Conflict(_))
        ));

        response_kind.store(0, Ordering::SeqCst);
        regional.permission.as_mut().unwrap().expires_at = unix_now().saturating_sub(1);
        active.regional = Some(regional);
        client
            .save(&AccountState {
                active: Some(active.clone()),
                ..AccountState::default()
            })
            .unwrap();
        assert!(matches!(
            client.regional_session(&identity).await,
            Err(CoreError::Unavailable(_))
        ));
        active.expires = unix_now().saturating_sub(1);
        client
            .save(&AccountState {
                active: Some(active),
                ..AccountState::default()
            })
            .unwrap();
        assert!(matches!(
            client.regional_session(&identity).await,
            Err(CoreError::AuthenticationRequired)
        ));
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn account_signout_status_does_not_clear_a_new_session() {
        let server = MockServer::start(Arc::new(|request| match request.path.as_str() {
            "/api/v1/client/me" => MockResponse::empty(401),
            "/api/v1/client/sessions/previous-session/revoke" => {
                MockResponse::json(200, &json!({"state":"revoked"}))
            }
            _ => MockResponse::empty(404),
        }))
        .await;
        let directory = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(directory.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let old = seed_session(&client, "previous-session");
        client
            .save(&AccountState {
                active: Some(old.clone()),
                ..AccountState::default()
            })
            .unwrap();
        assert!(matches!(
            client.status().await,
            Err(CoreError::AuthenticationRequired)
        ));
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());
        assert!(AccountClient::local_logout_pending(&secrets).unwrap());

        let active = seed_session(&client, "new-session");
        let mut state = load_state(&secrets).unwrap();
        state.active = Some(active);
        client.save(&state).unwrap();
        let response = client
            .http
            .get(server.url().join("api/v1/client/me").unwrap())
            .send()
            .await
            .unwrap();
        assert!(matches!(
            client.check_active(response, &old.credential).await,
            Err(CoreError::AuthenticationRequired)
        ));
        assert!(!client
            .begin_local_logout(Some("previous-session"))
            .await
            .unwrap());
        client.retry_remote_signout().await.unwrap();
        assert!(!AccountClient::local_logout_pending(&secrets).unwrap());
        assert_eq!(
            AccountClient::local_session_id(&instance, &secrets)
                .unwrap()
                .as_deref(),
            Some("new-session")
        );
    }

    #[tokio::test]
    async fn account_bulk_signout_ambiguous_response_reconciles_local_session() {
        let server = MockServer::start(Arc::new(|request| match request.path.as_str() {
            "/api/v1/client/session-revocations" => MockResponse::empty(503),
            "/api/v1/client/sessions/current-session/revoke" => {
                MockResponse::json(200, &json!({"state":"revoked"}))
            }
            _ => MockResponse::empty(404),
        }))
        .await;
        let directory = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(directory.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        client
            .save(&AccountState {
                active: Some(seed_session(&client, "current-session")),
                ..AccountState::default()
            })
            .unwrap();

        assert!(matches!(
            client.revoke_all_sessions().await,
            Err(CoreError::Unavailable(_))
        ));
        assert!(AccountClient::local_session_id(&instance, &secrets)
            .unwrap()
            .is_none());
        assert!(AccountClient::local_logout_pending(&secrets).unwrap());
        client.retry_remote_signout().await.unwrap();
        assert!(!AccountClient::local_logout_pending(&secrets).unwrap());
    }

    #[tokio::test]
    async fn account_sessions_list_and_revoke_exactly_the_intended_app_session() {
        let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorded = calls.clone();
        let revocations = Arc::new(AtomicUsize::new(0));
        let counted = revocations.clone();
        let server = MockServer::start(Arc::new(move |request| {
            recorded
                .lock()
                .unwrap()
                .push(format!("{} {}", request.method, request.path));
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/v1/client/sessions") => MockResponse::json(
                    200,
                    &json!({"sessions": [
                        {"sessionId":"target-session","instanceId":"other-instance","expiresAt":unix_now() + 600,"state":"active"},
                        {"sessionId":"other-session","instanceId":"third-instance","expiresAt":unix_now() + 600,"state":"active"}
                    ]}),
                ),
                ("POST", "/api/v1/client/sessions/target-session/revoke") => {
                    match counted.fetch_add(1, Ordering::SeqCst) {
                        0 => MockResponse::json(202, &json!({"state":"pending"})),
                        _ => MockResponse::json(200, &json!({"state":"revoked"})),
                    }
                }
                ("POST", "/api/v1/client/sessions/current-session/revoke") => {
                    MockResponse::json(200, &json!({"state":"revoked"}))
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let directory = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(directory.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        client
            .save(&AccountState {
                active: Some(seed_session(&client, "current-session")),
                ..AccountState::default()
            })
            .unwrap();

        let sessions = client.sessions().await.unwrap();
        let ids: Vec<&str> = sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["target-session", "other-session"]);
        assert_eq!(sessions[0].state, AccountSessionState::Active);
        assert_eq!(sessions[0].instance_id, "other-instance");

        assert_eq!(
            client.revoke_session("target-session").await.unwrap(),
            AccountSessionState::Pending,
            "a pending regional revocation is reported as pending"
        );
        // The caller's own session is untouched and remains usable.
        assert_eq!(
            AccountClient::local_session_id(&instance, &secrets)
                .unwrap()
                .as_deref(),
            Some("current-session")
        );
        assert_eq!(
            client.revoke_session("target-session").await.unwrap(),
            AccountSessionState::Revoked,
            "the enforced acknowledgement is distinguishable from pending"
        );
        assert_eq!(
            client.revoke_session("current-session").await.unwrap(),
            AccountSessionState::Revoked
        );
        assert!(AccountClient::local_session_id(&instance, &secrets)
            .unwrap()
            .is_none());
        let calls = calls.lock().unwrap().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains("target-session"))
                .count(),
            2,
            "{calls:?}"
        );
        assert!(
            calls.iter().all(|call| !call.contains("other-session")),
            "an unrelated App session is never revoked: {calls:?}"
        );
        assert!(matches!(
            client.revoke_session("bad/id").await,
            Err(CoreError::InvalidInput(_))
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

    // MARK: - Native iOS authorization-code flow

    const IOS_TEST_ISSUER: &str = "https://login.whalerein.com/oidc";

    fn ios_active_body() -> serde_json::Value {
        json!({
            "state": "active",
            "sessionId": "ios-session",
            "expiresAt": unix_now() + 600,
            "account": {
                "id": "acct-ios",
                "email": "user@example.test",
                "issuer": IOS_TEST_ISSUER,
                "subject": "subject-ios",
            },
            "workspace": {"id": "ws-ios"},
        })
    }

    #[derive(Default)]
    struct IOSFixture {
        produced_state: Option<String>,
        challenge: Option<String>,
        begin: Option<serde_json::Value>,
    }

    #[tokio::test]
    async fn native_ios_begin_and_exchange_create_a_session_and_clear_the_attempt() {
        let fixture = Arc::new(std::sync::Mutex::new(IOSFixture::default()));
        let recorded = fixture.clone();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let counted = exchanges.clone();
        let server = MockServer::start(Arc::new(move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/ios/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap().to_owned();
                    assert_eq!(attempt_id.len(), 32);
                    assert!(attempt_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
                    assert_eq!(body["codeChallengeMethod"], "S256");
                    assert_eq!(body["redirectUri"], "whalerein://auth/callback");
                    assert_ne!(body["credentialDigest"], body["revocationDigest"]);
                    let state = format!("state-{attempt_id}");
                    let mut fixture = recorded.lock().unwrap();
                    fixture.challenge = body["codeChallenge"].as_str().map(str::to_owned);
                    fixture.begin = Some(body);
                    fixture.produced_state = Some(state.clone());
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!(
                                "{IOS_TEST_ISSUER}/auth?client_id=ios&state={state}&code_challenge=x"
                            ),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/ios/exchange") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let fixture_state = recorded.lock().unwrap();
                    assert_eq!(body["attemptId"], fixture_state.begin.as_ref().unwrap()["attemptId"]);
                    assert_eq!(body["state"], fixture_state.produced_state.clone().unwrap());
                    assert_eq!(body["code"], "code-ios");
                    assert_eq!(
                        token_digest(body["codeVerifier"].as_str().unwrap()),
                        fixture_state.challenge.clone().unwrap()
                    );
                    drop(fixture_state);
                    MockResponse::json(200, &ios_active_body())
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;

        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let attempt = client.auth_start().await.unwrap();
        assert_eq!(attempt.attempt_id.len(), 32);
        assert!(attempt.url.contains("state="));
        let state = {
            let stored = load_state(&secrets).unwrap();
            let pending = stored.ios.expect("attempt retained");
            assert_eq!(pending.attempt_id, attempt.attempt_id);
            assert!(!pending.verifier.is_empty());
            pending.state
        };
        assert!(secrets.load(ACCOUNT_SESSION).unwrap().is_some());

        let callback = format!("whalerein://auth/callback?code=code-ios&state={state}");
        let account = client.auth_complete(&callback).await.unwrap();
        assert_eq!(account.account.id, "acct-ios");
        assert_eq!(account.workspace.id, "ws-ios");
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
        assert!(load_state(&secrets).unwrap().ios.is_none());
        assert_eq!(
            AccountClient::local_session_id(&instance, &secrets)
                .unwrap()
                .as_deref(),
            Some("ios-session")
        );
        let manifest = instance.manifest().unwrap().unwrap();
        assert_eq!(manifest.account.unwrap().workspace, "ws-ios");
        // The callback carried no token and the returned value has none.
        assert!(!serde_json::to_string(&account)
            .unwrap()
            .contains("credential"));
    }

    #[tokio::test]
    async fn native_ios_state_mismatch_never_exchanges_and_keeps_the_real_attempt() {
        let fixture = Arc::new(std::sync::Mutex::new(IOSFixture::default()));
        let recorded = fixture.clone();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let counted = exchanges.clone();
        let server = MockServer::start(Arc::new(move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/ios/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let state = "expected-state".to_owned();
                    recorded.lock().unwrap().begin = Some(body);
                    recorded.lock().unwrap().produced_state = Some(state.clone());
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!("{IOS_TEST_ISSUER}/auth?state={state}"),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/ios/exchange") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    MockResponse::json(200, &ios_active_body())
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        client.auth_start().await.unwrap();

        for callback in [
            "whalerein://auth/callback?code=code-ios&state=other-state",
            "whalerein://auth/callback?code=code-ios&state=expected-state&state=other",
            "whalerein://auth/callback?state=expected-state",
            "whalerein://other/callback?code=code-ios&state=expected-state",
            "https://auth/callback?code=code-ios&state=expected-state",
        ] {
            assert!(
                matches!(
                    client.auth_complete(callback).await,
                    Err(CoreError::Unauthorized)
                ),
                "{callback}"
            );
        }
        assert_eq!(exchanges.load(Ordering::SeqCst), 0);
        assert!(load_state(&secrets).unwrap().ios.is_some());
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());

        let account = client
            .auth_complete("whalerein://auth/callback?code=code-ios&state=expected-state")
            .await
            .unwrap();
        assert_eq!(account.account.id, "acct-ios");
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn native_ios_expired_replayed_and_cancelled_callbacks_establish_nothing() {
        let fixture = Arc::new(std::sync::Mutex::new(IOSFixture::default()));
        let recorded = fixture.clone();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let counted = exchanges.clone();
        let server = MockServer::start(Arc::new(move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/ios/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap().to_owned();
                    let state = format!("state-{attempt_id}");
                    let mut fixture = recorded.lock().unwrap();
                    fixture.begin = Some(body);
                    fixture.produced_state = Some(state.clone());
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!("{IOS_TEST_ISSUER}/auth?state={state}"),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/ios/exchange") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    MockResponse::json(200, &ios_active_body())
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;

        // Expired attempt: mutate the stored deadline and then present its exact callback.
        let attempt = client.auth_start().await.unwrap();
        let state = load_state(&secrets).unwrap().ios.unwrap().state;
        let mut expired = load_state(&secrets).unwrap();
        let pending = expired.ios.as_mut().unwrap();
        assert_eq!(pending.attempt_id, attempt.attempt_id);
        pending.expires = unix_now() - 1;
        client.save(&expired).unwrap();
        let callback = format!("whalerein://auth/callback?code=code-ios&state={state}");
        assert!(matches!(
            client.auth_complete(&callback).await,
            Err(CoreError::Unauthorized)
        ));
        assert!(load_state(&secrets).unwrap().ios.is_none());

        // Replay: the first exchange succeeds, the second cannot create a session.
        let second = client.auth_start().await.unwrap();
        let state = format!("state-{}", second.attempt_id);
        client
            .auth_complete(&callback_with_state(&state))
            .await
            .unwrap();
        assert!(matches!(
            client.auth_complete(&callback_with_state(&state)).await,
            Err(CoreError::Unauthorized)
        ));
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);

        // Cancellation: a callback that arrives afterwards is rejected.
        let third = client.auth_start().await.unwrap();
        client.auth_cancel().await.unwrap();
        assert!(load_state(&secrets).unwrap().ios.is_none());
        assert!(matches!(
            client
                .auth_complete(&callback_with_state(&format!("state-{}", third.attempt_id)))
                .await,
            Err(CoreError::Unauthorized)
        ));
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn android_redirect_follows_the_cloud_environment() {
        assert_eq!(android_redirect_uri("local"), ANDROID_REDIRECT_URI);
        assert_eq!(
            android_redirect_uri("staging"),
            "https://app.staging.whalerein.com/client/android/callback"
        );
        assert_eq!(
            android_redirect_uri("production"),
            "https://app.whalerein.com/client/android/callback"
        );
        assert_eq!(
            CloudAuthPlatform::Ios.redirect_uri("staging"),
            IOS_REDIRECT_URI
        );

        let staging = Url::parse(android_redirect_uri("staging")).unwrap();
        assert!(CloudAuthPlatform::Android.accepts_callback(&staging, "staging"));
        assert!(!CloudAuthPlatform::Android.accepts_callback(&staging, "production"));
        assert!(!CloudAuthPlatform::Android.accepts_callback(&staging, "local"));
        let insecure =
            Url::parse("http://app.staging.whalerein.com/client/android/callback").unwrap();
        assert!(!CloudAuthPlatform::Android.accepts_callback(&insecure, "staging"));
        let wrong_path = Url::parse("https://app.staging.whalerein.com/other").unwrap();
        assert!(!CloudAuthPlatform::Android.accepts_callback(&wrong_path, "staging"));
    }

    fn callback_with_state(state: &str) -> String {
        format!("whalerein://auth/callback?code=code-ios&state={state}")
    }

    fn android_callback_with_state(state: &str) -> String {
        // The shared test instance resolves to the production environment.
        format!(
            "{}?code=code-android&state={state}",
            android_redirect_uri("production")
        )
    }

    #[tokio::test]
    async fn native_android_uses_android_routes_app_link_and_rejects_other_callbacks() {
        let recorded = Arc::new(std::sync::Mutex::new(IOSFixture::default()));
        let fixture = recorded.clone();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let counted = exchanges.clone();
        let origin = Arc::new(std::sync::Mutex::new(String::new()));
        let server = MockServer::start(Arc::new(move |request| {
            let authority = format!("http://{}", request.headers["host"]);
            *origin.lock().unwrap() = authority.clone();
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/android/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap().to_owned();
                    assert_eq!(body["redirectUri"], android_redirect_uri("production"));
                    assert_eq!(body["codeChallengeMethod"], "S256");
                    assert!(body["instanceId"]
                        .as_str()
                        .is_some_and(|instance| !instance.is_empty()));
                    let state = format!("state-{attempt_id}");
                    let mut fixture = fixture.lock().unwrap();
                    fixture.begin = Some(body);
                    fixture.produced_state = Some(state.clone());
                    fixture.challenge = None;
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!(
                                "{authority}/oidc/auth?client_id=android&state={state}"
                            ),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/android/exchange") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    MockResponse::json(200, &ios_active_body())
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let attempt = client
            .auth_start_platform(CloudAuthPlatform::Android)
            .await
            .unwrap();
        assert_eq!(attempt.attempt_id.len(), 32);
        let stored = load_state(&secrets).unwrap();
        let pending = stored.ios.expect("android attempt retained");
        assert_eq!(pending.platform, CloudAuthPlatform::Android);
        let state = pending.state;

        // iOS, cross-host and fragment callbacks never reach the exchange.
        for callback in [
            format!("whalerein://auth/callback?code=code-android&state={state}"),
            format!("https://evil.example/client/android/callback?code=code-android&state={state}"),
            format!("{ANDROID_REDIRECT_URI}?code=code-android&state={state}&state=other"),
            format!("{ANDROID_REDIRECT_URI}?code=code-android&state=other"),
            format!("{ANDROID_REDIRECT_URI}?code=code-android&state={state}#fragment"),
        ] {
            assert!(
                matches!(
                    client.auth_complete(&callback).await,
                    Err(CoreError::Unauthorized)
                ),
                "{callback}"
            );
        }
        assert_eq!(exchanges.load(Ordering::SeqCst), 0);
        assert!(load_state(&secrets).unwrap().ios.is_some());

        let account = client
            .auth_complete(&android_callback_with_state(&state))
            .await
            .unwrap();
        assert_eq!(account.account.id, "acct-ios");
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
        assert!(load_state(&secrets).unwrap().ios.is_none());
        assert_eq!(
            AccountClient::local_session_id(&instance, &secrets)
                .unwrap()
                .as_deref(),
            Some("ios-session")
        );
        // A replayed code cannot establish a second session.
        assert!(matches!(
            client
                .auth_complete(&android_callback_with_state(&state))
                .await,
            Err(CoreError::Unauthorized)
        ));
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn native_android_rejects_authorization_url_outside_the_issuer_authority() {
        let server = MockServer::start(Arc::new(|request| match request.path.as_str() {
            "/api/v1/client/android/attempts" => {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let attempt_id = body["attemptId"].as_str().unwrap();
                MockResponse::json(
                    201,
                    &json!({
                        "authorizationUrl": format!(
                            "https://untrusted.example/auth?state=state-{attempt_id}"
                        ),
                        "expiresAt": unix_now() + 600,
                    }),
                )
            }
            _ => MockResponse::empty(404),
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        assert!(matches!(
            client.auth_start_platform(CloudAuthPlatform::Android).await,
            Err(CoreError::ProtocolIncompatible)
        ));
        assert!(load_state(&secrets).unwrap().ios.is_none());
    }

    #[tokio::test]
    async fn native_android_lost_exchange_response_recovers_with_the_original_credential() {
        let recorded = Arc::new(std::sync::Mutex::new(IOSFixture::default()));
        let fixture = recorded.clone();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let counted = exchanges.clone();
        let me_calls = Arc::new(AtomicUsize::new(0));
        let counted_me = me_calls.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let authority = format!("http://{}", request.headers["host"]);
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/android/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap().to_owned();
                    let state = format!("state-{attempt_id}");
                    fixture.lock().unwrap().produced_state = Some(state.clone());
                    fixture.lock().unwrap().begin = Some(body);
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!("{authority}/oidc/auth?state={state}"),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                // Cloud committed the App Session but the response is lost.
                ("POST", "/api/v1/client/android/exchange") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    MockResponse::empty(503)
                }
                ("GET", "/api/v1/client/me") => {
                    counted_me.fetch_add(1, Ordering::SeqCst);
                    assert!(request.headers.contains_key("authorization"));
                    MockResponse::json(
                        200,
                        &json!({
                            "sessionId": "android-session",
                            "expiresAt": unix_now() + 600,
                            "account": {
                                "id": "acct-android",
                                "email": "user@example.test",
                                "issuer": "https://login.whalerein.com/oidc",
                                "subject": "subject-android",
                            },
                            "workspace": {"id": "ws-android"},
                        }),
                    )
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let attempt = client
            .auth_start_platform(CloudAuthPlatform::Android)
            .await
            .unwrap();
        let state = load_state(&secrets).unwrap().ios.unwrap().state;
        let error = match client
            .auth_complete(&android_callback_with_state(&state))
            .await
        {
            Ok(_) => panic!("lost exchange response must not report success"),
            Err(error) => error,
        };
        assert!(matches!(error, CoreError::Unavailable(_)));
        // The pending attempt and its original credential survive.
        let pending = load_state(&secrets).unwrap().ios.expect("attempt retained");
        assert_eq!(pending.attempt_id, attempt.attempt_id);
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);

        let account = client.auth_recover().await.unwrap();
        assert_eq!(account.account.id, "acct-android");
        assert_eq!(account.workspace.id, "ws-android");
        assert_eq!(me_calls.load(Ordering::SeqCst), 1);
        assert!(load_state(&secrets).unwrap().ios.is_none());
        // Recovery never replays the code.
        assert_eq!(exchanges.load(Ordering::SeqCst), 1);
        assert_eq!(
            AccountClient::local_session_id(&instance, &secrets)
                .unwrap()
                .as_deref(),
            Some("android-session")
        );
    }

    #[tokio::test]
    async fn native_cross_platform_callback_cannot_consume_the_other_platforms_attempt() {
        let server = MockServer::start(Arc::new(|request| {
            let authority = format!("http://{}", request.headers["host"]);
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/android/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap();
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!(
                                "{authority}/oidc/auth?state=state-{attempt_id}"
                            ),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/ios/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap();
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!(
                                "{authority}/oidc/auth?state=state-{attempt_id}"
                            ),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let android = client
            .auth_start_platform(CloudAuthPlatform::Android)
            .await
            .unwrap();
        let state = load_state(&secrets).unwrap().ios.unwrap().state;
        // An iOS-shaped callback cannot complete the Android attempt even with
        // the exact Android state value.
        assert!(matches!(
            client.auth_complete(&callback_with_state(&state)).await,
            Err(CoreError::Unauthorized)
        ));
        assert!(load_state(&secrets).unwrap().ios.is_some());
        let _ = android;

        let ios = client.auth_start().await.unwrap();
        let state = load_state(&secrets).unwrap().ios.unwrap().state;
        assert_eq!(
            load_state(&secrets).unwrap().ios.unwrap().platform,
            CloudAuthPlatform::Ios
        );
        assert!(matches!(
            client
                .auth_complete(&android_callback_with_state(&state))
                .await,
            Err(CoreError::Unauthorized)
        ));
        assert!(load_state(&secrets).unwrap().ios.is_some());
        let _ = ios;
    }

    #[tokio::test]
    async fn native_ios_wrong_scope_keeps_the_previous_binding_and_never_stores_a_session() {
        let server = MockServer::start(Arc::new(|request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/ios/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap().to_owned();
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!(
                                "{IOS_TEST_ISSUER}/auth?state=state-{attempt_id}"
                            ),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/ios/exchange") => {
                    let mut body = ios_active_body();
                    body["workspace"]["id"] = json!("another-workspace");
                    MockResponse::json(200, &body)
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        instance.prepare_runtime().unwrap();
        instance
            .bind_account(&crate::api::instance::AccountScope {
                issuer: IOS_TEST_ISSUER.into(),
                subject: "subject-ios".into(),
                workspace: "ws-ios".into(),
            })
            .unwrap();
        let client = native_test_client(&instance, &secrets, &server, true).await;
        let attempt = client.auth_start().await.unwrap();
        assert!(matches!(
            client
                .auth_complete(&callback_with_state(&format!(
                    "state-{}",
                    attempt.attempt_id
                )))
                .await,
            Err(CoreError::Conflict(_))
        ));
        assert!(load_state(&secrets).unwrap().ios.is_none());
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());
        assert_eq!(
            instance
                .manifest()
                .unwrap()
                .unwrap()
                .account
                .unwrap()
                .workspace,
            "ws-ios"
        );
    }

    #[tokio::test]
    async fn native_ios_callback_for_another_instance_cannot_complete_this_attempt() {
        let fixture = Arc::new(std::sync::Mutex::new(IOSFixture::default()));
        let recorded = fixture.clone();
        let server = MockServer::start(Arc::new(move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/ios/attempts") => {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    let attempt_id = body["attemptId"].as_str().unwrap().to_owned();
                    recorded.lock().unwrap().begin = Some(body);
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!(
                                "{IOS_TEST_ISSUER}/auth?state=state-{attempt_id}"
                            ),
                            "expiresAt": unix_now() + 600,
                        }),
                    )
                }
                ("POST", "/api/v1/client/ios/exchange") => {
                    MockResponse::json(200, &ios_active_body())
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let (first_instance, first_secrets, _) = native_test_instance(first_root.path());
        let (second_instance, second_secrets, _) = native_test_instance(second_root.path());
        let first = native_test_client(&first_instance, &first_secrets, &server, true).await;
        let second = native_test_client(&second_instance, &second_secrets, &server, true).await;
        let first_attempt = first.auth_start().await.unwrap();
        let second_attempt = second.auth_start().await.unwrap();
        assert_ne!(first_attempt.attempt_id, second_attempt.attempt_id);

        // The first instance's callback (state and attempt id) is presented to
        // the second instance while its own attempt is pending.
        assert!(matches!(
            second
                .auth_complete(&callback_with_state(&format!(
                    "state-{}",
                    first_attempt.attempt_id
                )))
                .await,
            Err(CoreError::Unauthorized)
        ));
        assert!(load_state(&second_secrets).unwrap().ios.is_some());
        assert!(
            AccountClient::local_status(&second_instance, &second_secrets)
                .unwrap()
                .is_none()
        );

        // The originating instance still completes its own callback.
        first
            .auth_complete(&callback_with_state(&format!(
                "state-{}",
                first_attempt.attempt_id
            )))
            .await
            .unwrap();
        second
            .auth_complete(&callback_with_state(&format!(
                "state-{}",
                second_attempt.attempt_id
            )))
            .await
            .unwrap();
        assert!(load_state(&first_secrets).unwrap().ios.is_none());
        assert!(load_state(&second_secrets).unwrap().ios.is_none());
    }

    #[tokio::test]
    async fn native_ios_begin_rejects_a_provider_url_without_state_or_a_bad_deadline() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let server = MockServer::start(Arc::new(move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/client/ios/attempts") => {
                    let expires = if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                        unix_now() + 600
                    } else {
                        // A URL without state or a past deadline is refused
                        // instead of being retained as a usable attempt.
                        unix_now() - 1
                    };
                    MockResponse::json(
                        201,
                        &json!({
                            "authorizationUrl": format!("{IOS_TEST_ISSUER}/auth?client_id=ios"),
                            "expiresAt": expires,
                        }),
                    )
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let root = tempfile::tempdir().unwrap();
        let (instance, secrets, _) = native_test_instance(root.path());
        let client = native_test_client(&instance, &secrets, &server, true).await;
        assert!(matches!(
            client.auth_start().await,
            Err(CoreError::ProtocolIncompatible)
        ));
        assert!(matches!(
            client.auth_start().await,
            Err(CoreError::ProtocolIncompatible)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(load_state(&secrets).unwrap().ios.is_none());
        assert!(AccountClient::local_status(&instance, &secrets)
            .unwrap()
            .is_none());
    }
}
