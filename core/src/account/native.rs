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

#[derive(Default, Serialize, Deserialize)]
struct AccountState {
    active: Option<Session>,
    attempt: Option<LoginAttempt>,
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
            secrets,
            instance,
        })
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
        let mut client = AccountClient::for_instance(instance.clone(), secrets.clone(), create)
            .await
            .unwrap();
        client.api = server.url();
        client.issuer = server.url().join("oidc").unwrap();
        client
    }

    #[tokio::test]
    async fn account_device_flow_denial_and_cancellation_leave_no_active_session() {
        for cancel in [false, true] {
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
                    ("POST", "/api/v1/client/login/poll") => MockResponse::empty(401),
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
            } else {
                assert!(matches!(login.await.unwrap(), Err(CoreError::Unauthorized)));
            }
            assert!(AccountClient::local_status(&instance, &secrets)
                .unwrap()
                .is_none());
            assert!(instance.manifest().unwrap().unwrap().account.is_none());
            assert_eq!(load_state(&secrets).unwrap().attempt.is_some(), cancel);
        }
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
