//! HTTPS client for the WhaleRein Access HTTP surface.
//!
//! All bodies use the proto3 JSON encoding of `proto/v1`. The client never
//! exposes these DTOs outside Core.

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use ring::signature::Ed25519KeyPair;
use serde::de::DeserializeOwned;
use url::Url;
use whalerein_proto::{
    ErrorCode, ErrorResponse, HostDirectory, HostInfo, HostList, HostRequest, OnlineStatus,
    PairingClaimRequest, PairingClaimResponse, PayloadReference, RelayObject,
    RelayObjectDeleteResult, RelayObjectFinishRequest, RelayObjectReservation,
    RelayObjectReserveRequest, RequestKind, RequestReceipt, RequestState,
};

use crate::access::transport::{TransportBudget, RELAY_EPOCH_V5};
use crate::api::{error_code_name, CoreError, CoreResult};
use crate::diagnostics::journal::{request_id_for_log, DiagnosticKind, Diagnostics};
use crate::util::now_unix_ms;

const SESSION_PROOF_PREFIX: &str = "whalerein/v1/session-proof";

fn session_proof_input(
    session_id: &str,
    host_id: &str,
    device_id: &str,
    channel_id: &str,
    transcript_hash: &[u8; 32],
    issued: i64,
    expires: i64,
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        SESSION_PROOF_PREFIX,
        session_id,
        host_id,
        device_id,
        channel_id,
        URL_SAFE_NO_PAD.encode(transcript_hash),
        issued,
        expires
    )
}

pub const TRACE_HEADER: &str = "x-whalerein-trace-id";
/// Every raw relay-directory request carries the coordinated relay epoch.
pub const RELAY_EPOCH_HEADER: &str = "WhaleRein-Relay-Epoch";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Time held back from an authorized wait for exactly one final authoritative
/// `GET /v1/requests/{id}`. It is spent only when the wait would otherwise end
/// without a proven outcome.
pub const FINAL_LOOKUP_RESERVE: Duration = Duration::from_secs(2);
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessErrorKind {
    /// Connection setup failed before an HTTP request could be sent.
    NotSent,
    Transport,
    Http,
    Protocol,
    Timeout,
}

#[derive(Debug, Clone)]
pub struct AccessError {
    pub kind: AccessErrorKind,
    pub status: Option<u16>,
    /// Stable ErrorResponse code when Access returned one.
    pub code: Option<ErrorCode>,
    /// Sanitized message. Never contains request bodies.
    pub message: String,
    /// Stable authority reason from Access. A non-empty reason is terminal for
    /// a long-lived observation; an empty reason keeps the ErrorCode behavior.
    pub reason: String,
}

impl AccessError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: AccessErrorKind::Transport,
            status: None,
            code: None,
            message: message.into(),
            reason: String::new(),
        }
    }

    fn not_sent() -> Self {
        Self {
            kind: AccessErrorKind::NotSent,
            status: None,
            code: None,
            message: "Access connection failed".into(),
            reason: String::new(),
        }
    }

    pub(crate) fn definitely_not_sent(&self) -> bool {
        self.kind == AccessErrorKind::NotSent
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: AccessErrorKind::Protocol,
            status: None,
            code: None,
            message: message.into(),
            reason: String::new(),
        }
    }

    pub fn timeout() -> Self {
        Self {
            kind: AccessErrorKind::Timeout,
            status: None,
            code: None,
            message: "request timed out".into(),
            reason: String::new(),
        }
    }

    pub fn is_retryable_transport(&self) -> bool {
        matches!(
            self.kind,
            AccessErrorKind::Transport | AccessErrorKind::NotSent
        ) && self.status.is_none()
    }
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AccessError {}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CloudPairingState {
    Pending,
    Ready,
    Terminal,
}

/// Host-owned display metadata a client may preview before pairing.
///
/// The values are advisory decoration only: they never authorize a pairing,
/// pin a key, decide the prompt or reach persisted configuration. Access has
/// already bounded them, and Core drops any field that is empty, oversized or
/// contains control characters instead of failing the whole preview.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostDirectoryPreview {
    pub display_name: String,
    pub os_family: String,
}

impl HostDirectoryPreview {
    fn bounded(directory: HostDirectory) -> Self {
        Self {
            display_name: bounded_directory_field(
                &directory.display_name,
                MAX_DIRECTORY_DISPLAY_NAME_CHARS,
            ),
            os_family: bounded_directory_field(&directory.os_family, MAX_DIRECTORY_OS_FAMILY_CHARS),
        }
    }
}

const MAX_DIRECTORY_BODY_BYTES: usize = 16 * 1024;
const MAX_DIRECTORY_DISPLAY_NAME_CHARS: usize = 120;
const MAX_DIRECTORY_OS_FAMILY_CHARS: usize = 32;
const MAX_INVITATION_TOKEN_BYTES: usize = 8 * 1024;

/// Drop a field rather than fail the preview when Access sends something the
/// prompt cannot show safely. Trimming is not a truncation: an oversized value
/// is dropped exactly like an empty one.
fn bounded_directory_field(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars || value.chars().any(char::is_control)
    {
        String::new()
    } else {
        value.to_owned()
    }
}

#[derive(Clone)]
pub struct AccessClient {
    http: Client,
    base: Url,
    token: Option<String>,
    request_timeout: Duration,
    /// Bound for the advisory pre-pair directory read. The production value
    /// matches normal pairing calls; tests shorten it.
    directory_timeout: Duration,
    #[allow(dead_code)]
    poll_interval: Duration,
    transport: TransportBudget,
    response_limit: usize,
    pub(crate) diagnostics: Arc<Diagnostics>,
    cloud_session: Option<(String, Arc<dyn crate::security::SecretStore>)>,
}

impl std::fmt::Debug for AccessClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessClient")
            .field("base", &self.base.as_str())
            .field("authenticated", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl AccessClient {
    #[cfg(test)]
    pub fn new(base: Url, token: Option<String>) -> Result<Self, AccessError> {
        Self::with_trust(base, token, None, None, Arc::default())
    }

    /// Open Access with both trust inputs: an explicit custom CA (which
    /// replaces everything) and the platform anchors an app supplies when the
    /// platform store cannot be enumerated by Rust.
    pub fn with_trust(
        base: Url,
        token: Option<String>,
        ca_pem: Option<&[u8]>,
        platform_roots: Option<&[u8]>,
        diagnostics: Arc<Diagnostics>,
    ) -> Result<Self, AccessError> {
        if !matches!(base.scheme(), "https" | "http")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.fragment().is_some()
        {
            return Err(AccessError::protocol("invalid Access URL"));
        }
        // One trust decision for both transports: platform verification by
        // default, or exactly the supplied CA bundle.
        let tls = super::tls::client_config(ca_pem, platform_roots, &[])?;
        let http = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent(concat!("whalerein-core/", env!("CARGO_PKG_VERSION")))
            .use_preconfigured_tls((*tls).clone())
            .build()
            .map_err(|_| AccessError::transport("HTTP client initialization failed"))?;
        let transport = TransportBudget::default();
        Ok(Self {
            http,
            base,
            token,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            directory_timeout: DEFAULT_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
            transport,
            response_limit: https_body_limit(transport.envelope_bytes),
            diagnostics,
            cloud_session: None,
        })
    }

    pub fn trace_id(&self) -> &str {
        self.diagnostics.trace_id()
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub(crate) fn with_transport_budget(mut self, transport: TransportBudget) -> Self {
        self.transport = transport;
        self.response_limit = https_body_limit(transport.envelope_bytes);
        self
    }

    pub(crate) fn transport_budget(&self) -> TransportBudget {
        self.transport
    }

    pub(crate) fn with_cloud_session(
        mut self,
        id: String,
        secrets: Arc<dyn crate::security::SecretStore>,
    ) -> Self {
        self.cloud_session = Some((id, secrets));
        self
    }

    pub(crate) fn cloud_session_id(&self) -> Option<&str> {
        self.cloud_session.as_ref().map(|(id, _)| id.as_str())
    }

    fn matching_cloud_region(
        &self,
        device_id: Option<&str>,
    ) -> crate::api::CoreResult<Option<crate::account::native::RegionalSession>> {
        let Some((id, secrets)) = &self.cloud_session else {
            return Ok(None);
        };
        let current =
            crate::account::native::AccountClient::local_regional_session(secrets, device_id)?;
        Ok(current.filter(|current| current.id == *id && self.has_token(&current.token)))
    }

    pub(crate) fn cloud_session_current(
        &self,
        device_id: Option<&str>,
    ) -> crate::api::CoreResult<Option<bool>> {
        self.cloud_session
            .as_ref()
            .map(|_| {
                self.matching_cloud_region(device_id)
                    .map(|matched| matched.is_some())
            })
            .transpose()
    }

    fn check_cloud_session(&self) -> Result<(), AccessError> {
        if self.cloud_session.is_none() {
            return Ok(());
        }
        let Ok(Some(current)) = self.matching_cloud_region(None) else {
            return Err(AccessError {
                kind: AccessErrorKind::Http,
                status: Some(401),
                code: None,
                message: "Cloud App session ended".into(),
                reason: "session_revoked".into(),
            });
        };
        if current
            .permission
            .is_none_or(|permit| permit.access_origin != self.base)
        {
            return Err(AccessError::transport("the active regional route changed"));
        }
        Ok(())
    }

    pub(crate) fn has_token(&self, token: &str) -> bool {
        self.token.as_deref() == Some(token)
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Override polling behavior for isolated unit tests.
    #[cfg(test)]
    pub fn set_poll_timing(&mut self, request_timeout: Duration, poll_interval: Duration) {
        self.request_timeout = request_timeout;
        self.poll_interval = poll_interval;
    }

    #[cfg(test)]
    pub(crate) fn set_directory_timeout(&mut self, timeout: Duration) {
        self.directory_timeout = timeout;
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, AccessError> {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| AccessError::protocol("Access URL cannot have path segments"))?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    fn request(
        &self,
        method: reqwest::Method,
        url: Url,
        trace_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        self.request_with_auth(method, url, trace_id, true)
    }

    fn request_with_auth(
        &self,
        method: reqwest::Method,
        url: Url,
        trace_id: Option<&str>,
        authenticate: bool,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .request(method, url)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json");
        if authenticate {
            if let Some(token) = &self.token {
                builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
            }
        }
        builder = builder.header(TRACE_HEADER, trace_id.unwrap_or_else(|| self.trace_id()));
        builder
    }

    async fn send<T: DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<T, AccessError> {
        self.send_observing(builder, true, |_| {}).await
    }

    async fn send_observing<T: DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
        log_exchange: bool,
        observe_headers: impl FnOnce(&reqwest::header::HeaderMap),
    ) -> Result<T, AccessError> {
        self.send_observing_limit(builder, log_exchange, observe_headers, self.response_limit)
            .await
    }

    async fn send_observing_limit<T: DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
        log_exchange: bool,
        observe_headers: impl FnOnce(&reqwest::header::HeaderMap),
        limit: usize,
    ) -> Result<T, AccessError> {
        self.check_cloud_session()?;
        let started = std::time::Instant::now();
        let response = builder.send().await.map_err(|error| {
            if log_exchange {
                let code = if error.is_timeout() {
                    "timeout"
                } else if error.is_connect() {
                    "connection_failed"
                } else {
                    "unavailable"
                };
                self.diagnostics
                    .record(DiagnosticKind::HttpTransportFailed {
                        code,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
            }
            if error.is_connect() || error.is_builder() {
                AccessError::not_sent()
            } else {
                AccessError::transport("Access request failed")
            }
        })?;
        let status = response.status().as_u16();
        observe_headers(response.headers());
        let result = parse_response(response, limit).await;
        self.check_cloud_session()?;
        if log_exchange {
            self.diagnostics.record(DiagnosticKind::HttpCompleted {
                status,
                elapsed_ms: started.elapsed().as_millis() as u64,
                code: result
                    .as_ref()
                    .err()
                    .and_then(|error| error.code)
                    .map(error_code_name),
            });
        }
        result
    }

    /// Prove possession of the App-session key, bound to the exact Noise
    /// transcript, and let Access relay it to the Host. A missing proof key
    /// (legacy/iOS/Android sessions) is a no-op until those flows mint one.
    pub async fn submit_session_proof(
        &self,
        host_id: &str,
        device_id: &str,
        channel_id: &str,
        transcript_hash: &[u8; 32],
    ) -> Result<(), AccessError> {
        let Some((session_id, _)) = &self.cloud_session else {
            return Ok(());
        };
        let Some((active_id, proof_key)) =
            crate::account::native::local_session_proof(&self.cloud_session.as_ref().unwrap().1)
                .map_err(|_| AccessError::transport("session proof key is unavailable"))?
        else {
            return Ok(());
        };
        if active_id != *session_id {
            return Ok(());
        }
        let issued = now_unix_ms();
        let expires = issued + 60_000;
        let input = session_proof_input(
            session_id,
            host_id,
            device_id,
            channel_id,
            transcript_hash,
            issued,
            expires,
        );
        let document = URL_SAFE_NO_PAD
            .decode(proof_key)
            .map_err(|_| AccessError::transport("session proof key is invalid"))?;
        let pair = Ed25519KeyPair::from_pkcs8(&document)
            .map_err(|_| AccessError::transport("session proof key is invalid"))?;
        let signature = pair.sign(input.as_bytes());
        let body = serde_json::json!({
            "hostId": host_id,
            "deviceId": device_id,
            "channelId": channel_id,
            "transcriptHash": URL_SAFE_NO_PAD.encode(transcript_hash),
            "issuedAtUnixMs": issued,
            "expiresAtUnixMs": expires,
            "signature": URL_SAFE_NO_PAD.encode(signature.as_ref()),
        });
        let url = self.endpoint(&["v1", "sessions", session_id, "proof"])?;
        let response = self
            .request(reqwest::Method::POST, url, None)
            .json(&body)
            .send()
            .await
            .map_err(|_| AccessError::transport("session proof request failed"))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            // Older Access builds do not expose the proof endpoint yet. The
            // channel still works under Access admission until the coordinated
            // release turns the Host gate on.
            return Ok(());
        }
        if !status.is_success() {
            return Err(AccessError::transport("session proof was rejected"));
        }
        Ok(())
    }

    /// Renew one observation lease. 204 means the Device's grant, session and
    /// Host enrollment are still current; a typed reason means authority ended.
    #[allow(dead_code)]
    pub async fn host_authority(&self, host_id: &str) -> Result<(), AccessError> {
        let url = self.endpoint(&["v1", "hosts", host_id, "authority"])?;
        let response = self
            .request(reqwest::Method::GET, url, None)
            .send()
            .await
            .map_err(|_| AccessError::transport("Access authority check failed"))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(parse_error_response(response, self.response_limit).await)
    }

    pub async fn list_hosts(&self) -> Result<Vec<HostInfo>, AccessError> {
        let url = self.endpoint(&["v1", "hosts"])?;
        let list: HostList = self
            .send(self.request(reqwest::Method::GET, url, None))
            .await?;
        Ok(list.hosts)
    }

    pub(crate) async fn inspect_hosts(&self) -> Result<(Vec<HostInfo>, bool), AccessError> {
        let url = self.endpoint(&["v1", "hosts"])?;
        let mut complete = true;
        let list: HostList = self
            .send_observing(
                self.request(reqwest::Method::GET, url, None),
                true,
                |headers| {
                    if let Some(truncated) = headers.get("X-Whalerein-Hosts-Truncated") {
                        complete = truncated == "false";
                    }
                },
            )
            .await?;
        Ok((list.hosts, complete))
    }

    pub async fn submit_request(
        &self,
        host_id: &str,
        request: &HostRequest,
    ) -> Result<RequestReceipt, AccessError> {
        let url = self.endpoint(&["v1", "hosts", host_id, "requests"])?;
        let builder = self
            .request(reqwest::Method::POST, url, Some(&request.trace_id))
            .json(request);
        let request_id = request_id_for_log(&request.request_id);
        self.diagnostics
            .record(DiagnosticKind::AccessRequestSubmitting {
                request_id: request_id.clone(),
                request_kind: RequestKind::try_from(request.kind)
                    .map_or("unknown", |kind| kind.as_str_name()),
            });
        let receipt: RequestReceipt = self.send(builder).await?;
        self.diagnostics
            .record(if receipt_matches_request(&receipt, request) {
                DiagnosticKind::AccessRequestAccepted {
                    request_id,
                    status: status_name(receipt.status),
                }
            } else {
                DiagnosticKind::RequestReceiptMismatch { request_id }
            });
        Ok(receipt)
    }

    /// Renew the Access client-poll lease without reading the result.
    pub async fn heartbeat_request(&self, request_id: &str) -> Result<(), AccessError> {
        let url = self.endpoint(&["v1", "requests", request_id, "heartbeat"])?;
        let response = self
            .request(reqwest::Method::POST, url, None)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|_| AccessError::transport("Access heartbeat failed"))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(parse_error_response(response, MAX_ERROR_BODY_BYTES).await)
    }

    pub async fn get_request(&self, request_id: &str) -> Result<RequestState, AccessError> {
        self.read_request(request_id, true, self.response_limit)
            .await
    }

    /// Read one stored result under an exact admitted bound. The JSON envelope
    /// expands the encrypted result body, so the bound includes encoding
    /// overhead while remaining finite before any allocation.
    pub async fn get_request_with_limit(
        &self,
        request_id: &str,
        result_max_bytes: u64,
    ) -> Result<RequestState, AccessError> {
        let limit =
            https_body_limit(result_max_bytes).min(https_body_limit(self.transport.envelope_bytes));
        self.read_request(request_id, true, limit).await
    }

    async fn read_request(
        &self,
        request_id: &str,
        log_exchange: bool,
        limit: usize,
    ) -> Result<RequestState, AccessError> {
        let url = self.endpoint(&["v1", "requests", request_id])?;
        self.send_observing_limit(
            self.request(reqwest::Method::GET, url, None),
            log_exchange,
            |_| {},
            limit,
        )
        .await
    }

    /// Reserve exact bytes and scope in the Access temporary ciphertext
    /// directory. The reservation is the only object the caller may upload.
    pub async fn reserve_relay_object(
        &self,
        request: &RelayObjectReserveRequest,
    ) -> Result<RelayObjectReservation, AccessError> {
        let url = self.endpoint(&["v1", "relay", "objects"])?;
        let reservation: RelayObjectReservation = self
            .send(self.request(reqwest::Method::POST, url, None).json(request))
            .await?;
        if reservation.object_id.is_empty()
            || reservation.object_id.len() != 32
            || !reservation
                .object_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || reservation.upload_path.is_empty()
        {
            return Err(AccessError::protocol("invalid relay object reservation"));
        }
        Ok(reservation)
    }

    /// Upload exactly the reserved ciphertext length. The body is never
    /// buffered, inspected or re-encoded by Access.
    pub async fn upload_relay_object(
        &self,
        reservation: &RelayObjectReservation,
        bytes: &[u8],
    ) -> Result<(), AccessError> {
        if bytes.is_empty() || bytes.len() as u64 > reservation.max_bytes {
            return Err(AccessError::protocol(
                "ciphertext does not match the reserved length",
            ));
        }
        let url = self.object_url(&reservation.upload_path)?;
        let mut request = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header(RELAY_EPOCH_HEADER, RELAY_EPOCH_V5.to_string())
            .header(reqwest::header::CONTENT_LENGTH, bytes.len().to_string())
            .header(TRACE_HEADER, self.trace_id())
            .timeout(self.transfer_timeout());
        // The directory authenticates the Device bearer on every route.
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| AccessError::transport("relay ciphertext upload failed"))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(parse_error_response(response, MAX_ERROR_BODY_BYTES).await)
    }

    /// Publish the reserved object only after the upload validated. Until this
    /// call succeeds, no reference may be submitted or published.
    pub async fn finish_relay_object(
        &self,
        object_id: &str,
        finish: &RelayObjectFinishRequest,
    ) -> Result<RelayObject, AccessError> {
        let url = self.endpoint(&["v1", "relay", "objects", object_id, "finish"])?;
        let object: RelayObject = self
            .send(self.request(reqwest::Method::POST, url, None).json(finish))
            .await?;
        if object.object_id != object_id {
            return Err(AccessError::protocol("relay object identity mismatch"));
        }
        Ok(object)
    }

    /// Fetch one complete referenced ciphertext. A missing, short, long or
    /// corrupt stream is an error and never a completed message.
    pub async fn fetch_relay_object(
        &self,
        reference: &PayloadReference,
    ) -> Result<Vec<u8>, AccessError> {
        let url = self.endpoint(&["v1", "relay", "objects", &reference.object_id])?;
        let limit = usize::try_from(reference.ciphertext_bytes).unwrap_or(usize::MAX);
        let mut request = self
            .http
            .get(url)
            .header(RELAY_EPOCH_HEADER, RELAY_EPOCH_V5.to_string())
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .header(TRACE_HEADER, self.trace_id())
            .timeout(self.transfer_timeout());
        // The directory rechecks the authenticated Device principal on every
        // ciphertext release, just like every other request route.
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|_| AccessError::transport("relay ciphertext fetch failed"))?;
        if !response.status().is_success() {
            return Err(parse_error_response(response, MAX_ERROR_BODY_BYTES).await);
        }
        read_bounded(response, limit).await
    }

    /// Bounded, idempotent release of a reserved or ready object. Best effort:
    /// a failed delete stays charged until Access expires it.
    pub async fn delete_relay_object(
        &self,
        object_id: &str,
    ) -> Result<RelayObjectDeleteResult, AccessError> {
        let url = self.endpoint(&["v1", "relay", "objects", object_id])?;
        self.send(
            self.request(reqwest::Method::DELETE, url, None)
                .header(RELAY_EPOCH_HEADER, RELAY_EPOCH_V5.to_string()),
        )
        .await
    }

    fn transfer_timeout(&self) -> Duration {
        Duration::from_millis(u64::from(self.transport.transfer_timeout_ms))
    }

    /// Resolve an Access-supplied node-scoped path under the configured Access
    /// origin while preserving a deployment ingress prefix.
    fn object_url(&self, path: &str) -> Result<Url, AccessError> {
        let mut url = self.base.clone();
        let prefix = url.path().trim_end_matches('/').to_owned();
        let relative = path.trim_start_matches('/');
        url.set_path(&format!("{prefix}/{relative}"));
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    pub async fn delete_device(&self, device_id: &str) -> Result<(), AccessError> {
        let url = self.endpoint(&["v1", "devices", device_id])?;
        let response = self
            .request(reqwest::Method::DELETE, url, None)
            .send()
            .await
            .map_err(|_| AccessError::transport("Access request failed"))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(parse_error_response(response, MAX_ERROR_BODY_BYTES).await)
    }

    /// Fetch Host-owned display metadata with the one-time invitation token.
    ///
    /// The token is sent only to the Access origin this client was built for,
    /// is never persisted or logged, and does not consume the invitation. Any
    /// failure is reported to the caller so pairing can treat the preview as
    /// advisory and continue with empty fields. Response fields are bounded and
    /// sanitized individually; one bad field never fails the preview.
    pub async fn host_directory(
        &self,
        access_url: &str,
        host_id: &str,
        token: &str,
    ) -> Result<HostDirectoryPreview, AccessError> {
        if host_id.is_empty() || token.is_empty() || token.len() > MAX_INVITATION_TOKEN_BYTES {
            return Err(AccessError::protocol("invalid directory request"));
        }
        let url = self.directory_endpoint(access_url, host_id)?;
        for attempt in 0..3 {
            let builder = self
                .request_with_auth(reqwest::Method::GET, url.clone(), None, false)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .timeout(self.directory_timeout);
            match self
                .send_observing_limit(builder, true, |_| {}, MAX_DIRECTORY_BODY_BYTES)
                .await
            {
                Err(error) if error.is_retryable_transport() && attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok(directory) => return Ok(HostDirectoryPreview::bounded(directory)),
                Err(error) => return Err(error),
            }
        }
        Err(AccessError::timeout())
    }

    /// Refuse to send the invitation token to any origin or prefix other than
    /// the one this client was opened for.
    fn directory_endpoint(&self, access_url: &str, host_id: &str) -> Result<Url, AccessError> {
        let requested =
            Url::parse(access_url).map_err(|_| AccessError::protocol("invalid Access URL"))?;
        if requested.scheme() != self.base.scheme()
            || requested.host_str() != self.base.host_str()
            || requested.port_or_known_default() != self.base.port_or_known_default()
            || requested.path().trim_end_matches('/') != self.base.path().trim_end_matches('/')
        {
            return Err(AccessError::protocol(
                "Access URL does not match the client",
            ));
        }
        self.endpoint(&["v1", "hosts", host_id, "directory"])
    }

    pub async fn pair_claim(
        &self,
        claim: &PairingClaimRequest,
    ) -> Result<PairingClaimResponse, AccessError> {
        let url = self.endpoint(&["v1", "pairing", "claims"])?;
        // First-registration claims authenticate with the one-time token in the
        // body; the device transport token does not exist yet.
        for attempt in 0..3 {
            let builder = self
                .request_with_auth(reqwest::Method::POST, url.clone(), None, false)
                .json(claim);
            match self.send(builder).await {
                Err(error) if error.is_retryable_transport() && attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                result => return result,
            }
        }
        Err(AccessError::timeout())
    }

    pub(crate) async fn cloud_pairing_outcome(
        &self,
        host: &str,
        operation: &str,
    ) -> Result<CloudPairingState, AccessError> {
        #[derive(serde::Deserialize)]
        struct Outcome {
            state: CloudPairingState,
        }
        let url = self.endpoint(&["v1", "external", "hosts", host, "pairings", operation])?;
        let outcome: Outcome = self
            .send(self.request(reqwest::Method::GET, url, None))
            .await?;
        Ok(outcome.state)
    }

    pub async fn pair_cloud_claim(
        &self,
        authorization: &whalerein_proto::ExternalAuthorization,
        claim: &PairingClaimRequest,
    ) -> Result<PairingClaimResponse, AccessError> {
        #[derive(serde::Serialize)]
        struct Request<'a> {
            authorization: &'a whalerein_proto::ExternalAuthorization,
            claim: &'a PairingClaimRequest,
        }
        let url = self.endpoint(&["v1", "enrollments", "devices", "pairing"])?;
        let request = Request {
            authorization,
            claim,
        };
        for attempt in 0..3 {
            let builder = self
                .request(reqwest::Method::POST, url.clone(), None)
                .json(&request);
            match self.send(builder).await {
                Err(error) if error.is_retryable_transport() && attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                result => return result,
            }
        }
        Err(AccessError::timeout())
    }

    /// Poll one request until it reaches a terminal status (or `host_accepted`
    /// when requested, for control commands).
    #[allow(dead_code)]
    pub async fn wait_for_request(
        &self,
        request: &HostRequest,
        allow_host_accepted: bool,
    ) -> CoreResult<RequestState> {
        let deadline = now_unix_ms() + self.request_timeout.as_millis() as i64;
        let started = std::time::Instant::now();
        let logged_request_id = request_id_for_log(&request.request_id);
        let mut polls = 0u32;
        let mut transport_failed = false;
        let result = loop {
            polls = polls.saturating_add(1);
            let limit = request
                .transport
                .as_ref()
                .map_or(self.response_limit, |transport| {
                    https_body_limit(transport.result_max_bytes)
                        .min(https_body_limit(self.transport.envelope_bytes))
                });
            match self.read_request(&request.request_id, false, limit).await {
                Ok(state) => {
                    self.validate_request_state(request, &state)?;
                    if transport_failed {
                        transport_failed = false;
                        self.diagnostics
                            .record(DiagnosticKind::AccessRequestPollRecovered {
                                request_id: logged_request_id.clone(),
                                polls,
                            });
                    }
                    let status = OnlineStatus::try_from(state.status).unwrap_or_default();
                    let proven = is_terminal(state.status)
                        || (allow_host_accepted && status == OnlineStatus::HostAccepted);
                    if proven {
                        break Ok(state);
                    }
                }
                Err(error) if error.is_retryable_transport() => {
                    if !transport_failed {
                        transport_failed = true;
                        self.diagnostics
                            .record(DiagnosticKind::AccessRequestPollFailed {
                                request_id: logged_request_id.clone(),
                                code: CoreError::from(error.clone()).code(),
                                polls,
                            });
                    }
                }
                Err(error) => break Err(error.into()),
            }
            let remaining_ms = deadline - now_unix_ms();
            if remaining_ms <= 0 {
                break Err(CoreError::Timeout);
            }
            // A wait observes an authoritative status with the short fixed
            // cadence. Growing backoff could miss a result returned just
            // before the overall deadline; foreground commands use scoped
            // notifications instead of this loop.
            tokio::time::sleep(self.poll_interval).await;
        };

        let request_id = logged_request_id;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        self.diagnostics.record(match &result {
            Ok(state) if state.status != OnlineStatus::Unknown as i32 => {
                DiagnosticKind::AccessRequestObserved {
                    request_id,
                    status: status_name(state.status),
                    polls,
                    elapsed_ms,
                }
            }
            _ => DiagnosticKind::AccessRequestUnconfirmed {
                request_id,
                code: result.as_ref().err().map_or("unknown", CoreError::code),
                polls,
                elapsed_ms,
            },
        });
        result
    }

    /// One authoritative status read for a foreground wait. The caller decides
    /// whether the returned status is a proven outcome.
    pub(crate) async fn read_request_authoritative(
        &self,
        request: &HostRequest,
    ) -> Result<RequestState, AccessError> {
        let limit = request
            .transport
            .as_ref()
            .map_or(self.response_limit, |transport| {
                https_body_limit(transport.result_max_bytes)
                    .min(https_body_limit(self.transport.envelope_bytes))
            });
        self.read_request(&request.request_id, true, limit).await
    }

    pub(crate) fn validate_request_state(
        &self,
        request: &HostRequest,
        state: &RequestState,
    ) -> CoreResult<()> {
        if state.request_id == request.request_id
            && state.host_id == request.host_id
            && state.device_id == request.device_id
            && state.command_id == request.command_id
        {
            return Ok(());
        }
        self.diagnostics
            .record(DiagnosticKind::RequestStateMismatch {
                request_id: request_id_for_log(&request.request_id),
            });
        Err(CoreError::Security(
            "request status identity mismatch".into(),
        ))
    }
}

fn status_name(status: i32) -> &'static str {
    OnlineStatus::try_from(status).map_or("unknown", |status| status.as_str_name())
}

pub(crate) fn receipt_matches_request(receipt: &RequestReceipt, request: &HostRequest) -> bool {
    receipt.request_id == request.request_id && receipt.command_id == request.command_id
}

#[allow(dead_code)]
pub fn is_terminal(status: i32) -> bool {
    matches!(
        OnlineStatus::try_from(status).unwrap_or(OnlineStatus::Unspecified),
        OnlineStatus::HostOffline
            | OnlineStatus::Rejected
            | OnlineStatus::Succeeded
            | OnlineStatus::Failed
            | OnlineStatus::Unknown
    )
}

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const HTTP_OVERHEAD_BYTES: u64 = 1024 * 1024;

pub(crate) fn https_body_limit(result_max_bytes: u64) -> usize {
    let encoded = result_max_bytes
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(HTTP_OVERHEAD_BYTES);
    usize::try_from(encoded).unwrap_or(usize::MAX)
}

fn response_too_large() -> AccessError {
    AccessError {
        kind: AccessErrorKind::Protocol,
        status: None,
        code: Some(ErrorCode::PayloadTooLarge),
        message: "Access response exceeds the configured transport budget".into(),
        reason: String::new(),
    }
}

async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, AccessError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(response_too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AccessError::transport("Access response could not be read"))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(response_too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn parse_response<T: DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<T, AccessError> {
    if !response.status().is_success() {
        return Err(parse_error_response(response, MAX_ERROR_BODY_BYTES).await);
    }
    let bytes = read_bounded(response, limit).await?;
    crate::api::decode_json(&bytes)
}

async fn parse_error_response(response: reqwest::Response, limit: usize) -> AccessError {
    let status = response.status();
    let bytes = read_bounded(response, limit).await.unwrap_or_default();
    let parsed = serde_json::from_slice::<ErrorResponse>(&bytes).ok();
    let (code, message, reason) = match parsed {
        Some(error) => (
            ErrorCode::try_from(error.code).ok(),
            sanitize_message(error.message),
            error.reason,
        ),
        None => (None, default_status_message(status), String::new()),
    };
    AccessError {
        kind: AccessErrorKind::Http,
        status: Some(status.as_u16()),
        code: code.or_else(|| status_to_error_code(status)),
        message,
        reason,
    }
}

fn sanitize_message(message: String) -> String {
    // Access error messages are diagnostic only. Cap the length so an
    // unexpectedly large body cannot flood CLI output.
    let mut message = message.replace(['\n', '\r'], " ");
    crate::util::truncate_utf8(&mut message, 200);
    if message.is_empty() {
        "request failed".into()
    } else {
        message
    }
}

fn default_status_message(status: StatusCode) -> String {
    format!("Access returned HTTP {}", status.as_u16())
}

pub fn status_to_error_code(status: StatusCode) -> Option<ErrorCode> {
    Some(match status.as_u16() {
        400 | 422 => ErrorCode::InvalidArgument,
        401 => ErrorCode::Unauthorized,
        403 => ErrorCode::Forbidden,
        404 => ErrorCode::NotFound,
        409 => ErrorCode::Conflict,
        410 => ErrorCode::Expired,
        429 => ErrorCode::RateLimited,
        500..=599 => ErrorCode::Unavailable,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn regional_host_list_never_treats_explicit_truncation_as_complete() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for (header, expected) in [("true", false), ("false", true), ("invalid", false)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                assert_ne!(stream.read(&mut request).await.unwrap(), 0);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\nX-Whalerein-Hosts-Truncated: {header}\r\n\r\n{{}}"
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let address = Url::parse(&format!("http://{address}")).unwrap();
            let client = AccessClient::new(address, None).unwrap();
            let (_, complete) = client.inspect_hosts().await.unwrap();
            assert_eq!(complete, expected);
            server.await.unwrap();
        }
    }

    #[test]
    fn rejects_invalid_base_urls() {
        for value in [
            "not-a-url",
            "ftp://example.test",
            "https://user:pw@example.test",
        ] {
            let parsed = Url::parse(value);
            assert!(
                parsed.is_err() || AccessClient::new(parsed.unwrap(), None).is_err(),
                "expected reject: {value}"
            );
        }
    }

    #[test]
    fn preserves_base_path_when_building_endpoints() {
        let client = AccessClient::new(
            Url::parse("https://example.test/prefix").unwrap(),
            Some("token".into()),
        )
        .unwrap();
        assert_eq!(
            client.endpoint(&["v1", "hosts"]).unwrap().as_str(),
            "https://example.test/prefix/v1/hosts"
        );
        assert_eq!(
            client
                .endpoint(&["v1", "hosts", "h-1", "requests"])
                .unwrap()
                .as_str(),
            "https://example.test/prefix/v1/hosts/h-1/requests"
        );
        assert_eq!(
            client
                .endpoint(&["v1", "requests", "r 1"])
                .unwrap()
                .as_str(),
            "https://example.test/prefix/v1/requests/r%201"
        );
    }

    #[test]
    fn maps_http_statuses_to_stable_codes() {
        assert_eq!(
            status_to_error_code(StatusCode::UNAUTHORIZED),
            Some(ErrorCode::Unauthorized)
        );
        assert_eq!(
            status_to_error_code(StatusCode::CONFLICT),
            Some(ErrorCode::Conflict)
        );
        assert_eq!(
            status_to_error_code(StatusCode::SERVICE_UNAVAILABLE),
            Some(ErrorCode::Unavailable)
        );
        assert_eq!(status_to_error_code(StatusCode::IM_A_TEAPOT), None);
    }

    #[test]
    fn classifies_terminal_and_nonterminal_statuses() {
        assert!(is_terminal(OnlineStatus::Succeeded as i32));
        assert!(is_terminal(OnlineStatus::HostOffline as i32));
        assert!(!is_terminal(OnlineStatus::Received as i32));
        assert!(!is_terminal(OnlineStatus::HostAccepted as i32));
        assert!(!is_terminal(OnlineStatus::Unspecified as i32));
        assert!(is_terminal(OnlineStatus::Rejected as i32));
        assert!(is_terminal(OnlineStatus::Failed as i32));
        // Unknown stops polling this request but can later be recovered by command ID.
        assert!(is_terminal(OnlineStatus::Unknown as i32));
        assert!(!is_terminal(999));
    }

    #[test]
    fn sanitizes_error_messages() {
        assert_eq!(sanitize_message("a\nb".into()), "a b");
        assert_eq!(sanitize_message(String::new()), "request failed");
        assert!(sanitize_message("x".repeat(1000)).len() <= 200);
    }

    #[test]
    fn error_message_byte_limit_preserves_utf8_without_panicking() {
        let sanitized = sanitize_message(format!("x{}", "🦀".repeat(60)));
        assert!(sanitized.len() <= 200);
        assert!(sanitized.starts_with('x'));
        assert!(!sanitized.contains('\u{fffd}'));
    }

    #[test]
    fn debug_never_contains_token() {
        let client = AccessClient::new(
            Url::parse("https://example.test").unwrap(),
            Some("secret-token".into()),
        )
        .unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("authenticated: true"));
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use crate::api::test_support::{MockResponse, MockServer};
    use std::sync::{Arc, Mutex};
    use whalerein_proto::{
        ErrorCode, HostList, HostRequest, OnlineStatus, PairingClaimRequest, PairingClaimResponse,
        RequestState,
    };

    fn request() -> HostRequest {
        HostRequest {
            request_id: "r-1".into(),
            host_id: "h-1".into(),
            device_id: "d-1".into(),
            kind: whalerein_proto::RequestKind::Query as i32,
            trace_id: "trace-1".into(),
            protocol_version: 1,
            channel_id: "chan".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn sends_expected_wire_paths_headers_and_bodies() {
        const REQUEST_ID: &str = "00000000000000000000000000000001";
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = seen.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            handler_seen.lock().unwrap().push((
                mock.method.clone(),
                mock.path.clone(),
                mock.headers.clone(),
                mock.body.clone(),
            ));
            match (mock.method.as_str(), mock.path.as_str()) {
                ("GET", "/v1/hosts") => MockResponse::json(
                    200,
                    &HostList {
                        hosts: vec![whalerein_proto::HostInfo {
                            host_id: "h-1".into(),
                            online: true,
                            protocol_version: 1,
                            fingerprint: "f".into(),
                        }],
                    },
                ),
                ("POST", "/v1/hosts/h-1/requests") => MockResponse::json(
                    200,
                    &whalerein_proto::RequestReceipt {
                        request_id: REQUEST_ID.into(),
                        status: OnlineStatus::Received as i32,
                        command_id: String::new(),
                        expires_at_unix_ms: 1,
                    },
                ),
                ("GET", "/v1/requests/00000000000000000000000000000001") => MockResponse::json(
                    200,
                    &RequestState {
                        request_id: REQUEST_ID.into(),
                        status: OnlineStatus::Succeeded as i32,
                        ..Default::default()
                    },
                ),
                ("DELETE", "/v1/devices/d-1") => MockResponse::empty(204),
                ("POST", "/v1/pairing/claims") => MockResponse::json(
                    200,
                    &PairingClaimResponse {
                        claim_id: "claim".into(),
                        host_id: "h-1".into(),
                        device_token: "token".into(),
                        ..Default::default()
                    },
                ),
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let sink = Arc::new(crate::diagnostics::journal::CapturingSink::default());
        let client = AccessClient::with_trust(
            server.url(),
            Some("device-token".into()),
            None,
            None,
            Arc::new(Diagnostics::new(Some(sink.clone()))),
        )
        .unwrap();

        let hosts = client.list_hosts().await.unwrap();
        assert_eq!(hosts[0].host_id, "h-1");
        let outgoing = HostRequest {
            request_id: REQUEST_ID.into(),
            trace_id: client.trace_id().into(),
            ..request()
        };
        let receipt = client.submit_request("h-1", &outgoing).await.unwrap();
        assert_eq!(receipt.status, OnlineStatus::Received as i32);
        let state = client.get_request(REQUEST_ID).await.unwrap();
        assert_eq!(state.status, OnlineStatus::Succeeded as i32);
        client.delete_device("d-1").await.unwrap();
        let pairing_client = AccessClient::new(server.url(), None).unwrap();
        let claim = pairing_client
            .pair_claim(&PairingClaimRequest {
                claim_id: "claim".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(claim.device_token, "token");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 5);
        assert_eq!(seen[0].0, "GET");
        assert_eq!(seen[0].1, "/v1/hosts");
        assert_eq!(
            seen[0].2.get("authorization").unwrap(),
            "Bearer device-token"
        );
        let submitted: HostRequest = serde_json::from_slice(&seen[1].3).unwrap();
        assert_eq!(submitted.request_id, REQUEST_ID);
        assert_eq!(seen[1].2.get(TRACE_HEADER).unwrap(), client.trace_id());
        assert_eq!(seen[2].2.get(TRACE_HEADER).unwrap(), client.trace_id());
        assert_eq!(seen[4].1, "/v1/pairing/claims");
        assert!(!seen[4].2.contains_key("authorization"));

        let events = sink.0.lock().unwrap();
        let request_events: Vec<_> = events
            .iter()
            .filter(|event| event["request_id"] == REQUEST_ID)
            .collect();
        assert_eq!(request_events.len(), 2);
        assert_eq!(request_events[0]["event"], "access_request_submitting");
        assert_eq!(request_events[0]["level"], "debug");
        assert_eq!(request_events[1]["event"], "access_request_accepted");
        assert_eq!(request_events[1]["level"], "info");
        assert_eq!(
            request_events[1]["status"],
            OnlineStatus::Received.as_str_name()
        );
        assert!(request_events
            .iter()
            .all(|event| event["trace_id"] == client.trace_id()));
        assert!(!serde_json::to_string(&*events)
            .unwrap()
            .contains("device-token"));
    }

    #[tokio::test]
    async fn preserves_error_code_from_access_response() {
        let server = MockServer::start(Arc::new(|_| {
            MockResponse::json(
                409,
                &ErrorResponse {
                    code: ErrorCode::PayloadDigestMismatch as i32,
                    message: "digest".into(),
                    ..Default::default()
                },
            )
        }))
        .await;
        let client = AccessClient::new(server.url(), None).unwrap();
        let error = client.get_request("r").await.unwrap_err();
        assert_eq!(error.status, Some(409));
        assert_eq!(error.code, Some(ErrorCode::PayloadDigestMismatch));
        assert_eq!(error.message, "digest");
        assert!(!error.is_retryable_transport());
        assert_eq!(error.to_string(), "digest");
    }

    fn poll_request() -> HostRequest {
        HostRequest {
            request_id: "r".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn request_diagnostics_keep_offline_receipts_and_exclude_mismatched_states() {
        const REQUEST_ID: &str = "00000000000000000000000000000001";
        let request = HostRequest {
            request_id: REQUEST_ID.into(),
            ..request()
        };
        let server = MockServer::start(Arc::new(|incoming| {
            if incoming.method == "POST" {
                return MockResponse::json(
                    200,
                    &RequestReceipt {
                        request_id: REQUEST_ID.into(),
                        status: OnlineStatus::HostOffline as i32,
                        ..Default::default()
                    },
                );
            }
            MockResponse::json(
                200,
                &RequestState {
                    request_id: REQUEST_ID.into(),
                    host_id: "unexpected-host".into(),
                    device_id: "d-1".into(),
                    status: OnlineStatus::Succeeded as i32,
                    ..Default::default()
                },
            )
        }))
        .await;
        let sink = Arc::new(crate::diagnostics::journal::CapturingSink::default());
        let client = AccessClient::with_trust(
            server.url(),
            None,
            None,
            None,
            Arc::new(Diagnostics::new(Some(sink.clone()))),
        )
        .unwrap();
        let receipt = client.submit_request("h-1", &request).await.unwrap();
        assert_eq!(receipt.status, OnlineStatus::HostOffline as i32);
        let error = client.wait_for_request(&request, false).await.unwrap_err();
        assert!(matches!(error, CoreError::Security(_)));
        let events = sink.0.lock().unwrap();
        let receipt_event = events
            .iter()
            .find(|event| event["event"] == "access_request_accepted")
            .unwrap();
        assert_eq!(
            receipt_event["status"],
            OnlineStatus::HostOffline.as_str_name()
        );
        assert!(!events
            .iter()
            .any(|event| event["event"] == "access_request_observed"));
        let mismatch = events.last().unwrap();
        assert_eq!(mismatch["event"], "request_state_mismatch");
        assert_eq!(mismatch["request_id"], REQUEST_ID);
        assert!(!serde_json::to_string(&*events)
            .unwrap()
            .contains("unexpected-host"));
    }

    #[tokio::test]
    async fn polls_request_until_access_returns_terminal_status() {
        let calls = Arc::new(Mutex::new(0u32));
        let handler_calls = calls.clone();
        let server = MockServer::start(Arc::new(move |_| {
            let mut calls = handler_calls.lock().unwrap();
            *calls += 1;
            let status = if *calls < 3 {
                OnlineStatus::Received as i32
            } else {
                OnlineStatus::Succeeded as i32
            };
            MockResponse::json(
                200,
                &RequestState {
                    request_id: "r".into(),
                    status,
                    ..Default::default()
                },
            )
        }))
        .await;
        let sink = Arc::new(crate::diagnostics::journal::CapturingSink::default());
        let mut client = AccessClient::with_trust(
            server.url(),
            None,
            None,
            None,
            Arc::new(Diagnostics::new(Some(sink.clone()))),
        )
        .unwrap();
        client.set_poll_timing(Duration::from_secs(5), Duration::from_millis(1));
        let state = client
            .wait_for_request(&poll_request(), false)
            .await
            .unwrap();
        assert_eq!(state.status, OnlineStatus::Succeeded as i32);
        assert_eq!(*calls.lock().unwrap(), 3);
        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "access_request_observed");
        assert_eq!(events[0]["polls"], 3);
        assert!(events[0].get("request_id").is_none());
    }

    #[tokio::test]
    async fn reports_timeout_when_request_status_never_becomes_terminal() {
        let server = MockServer::start(Arc::new(|_| {
            MockResponse::json(
                200,
                &RequestState {
                    request_id: "r".into(),
                    status: OnlineStatus::Received as i32,
                    ..Default::default()
                },
            )
        }))
        .await;
        let mut client = AccessClient::new(server.url(), None).unwrap();
        client.set_poll_timing(Duration::from_millis(20), Duration::from_millis(1));
        assert!(matches!(
            client.wait_for_request(&poll_request(), true).await,
            Err(CoreError::Timeout)
        ));
    }

    #[tokio::test]
    async fn poll_diagnostics_report_recovery_only_after_a_successful_response() {
        let calls = Arc::new(Mutex::new(0u32));
        let seen = calls.clone();
        let server = MockServer::start(Arc::new(move |_| {
            let mut count = seen.lock().unwrap();
            *count += 1;
            if *count == 1 {
                return MockResponse {
                    status: 0,
                    body: Vec::new(),
                    delay_ms: 0,
                };
            }
            MockResponse::json(
                200,
                &RequestState {
                    request_id: "r".into(),
                    status: OnlineStatus::Received as i32,
                    ..Default::default()
                },
            )
        }))
        .await;
        let sink = Arc::new(crate::diagnostics::journal::CapturingSink::default());
        let mut client = AccessClient::with_trust(
            server.url(),
            None,
            None,
            None,
            Arc::new(Diagnostics::new(Some(sink.clone()))),
        )
        .unwrap();
        client.set_poll_timing(Duration::from_secs(5), Duration::from_millis(5));
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            client.wait_for_request(&poll_request(), false),
        )
        .await;
        assert!(result.is_err());
        assert!(*calls.lock().unwrap() >= 2);
        {
            let mut events = sink.0.lock().unwrap();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0]["event"], "access_request_poll_failed");
            assert_eq!(events[0]["polls"], 1);
            assert_eq!(events[1]["event"], "access_request_poll_recovered");
            assert_eq!(events[1]["polls"], 2);
            events.clear();
        }

        let unavailable = MockServer::start(Arc::new(|_| MockResponse {
            status: 0,
            body: Vec::new(),
            delay_ms: 0,
        }))
        .await;
        let mut client = AccessClient::with_trust(
            unavailable.url(),
            None,
            None,
            None,
            Arc::new(Diagnostics::new(Some(sink.clone()))),
        )
        .unwrap();
        client.set_poll_timing(Duration::from_secs(5), Duration::from_millis(5));
        assert!(tokio::time::timeout(
            Duration::from_millis(100),
            client.wait_for_request(&poll_request(), false)
        )
        .await
        .is_err());
        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "access_request_poll_failed");
        assert_eq!(events[0]["level"], "warn");
    }

    #[test]
    fn invalid_ca_is_rejected() {
        let error = AccessClient::with_trust(
            Url::parse("https://example.test").unwrap(),
            None,
            Some(b"not a certificate"),
            None,
            Arc::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind, AccessErrorKind::Protocol);
    }

    #[test]
    fn error_kinds_are_retryable_only_for_transport() {
        assert!(AccessError::transport("x").is_retryable_transport());
        assert!(!AccessError::timeout().is_retryable_transport());
        assert!(!AccessError::protocol("invalid wire data").is_retryable_transport());
        assert!(!AccessError {
            kind: AccessErrorKind::Http,
            status: Some(500),
            code: None,
            message: "x".into(),
            reason: String::new()
        }
        .is_retryable_transport());
    }

    #[tokio::test]
    async fn https_reads_are_bounded_before_decoding() {
        use crate::access::transport::{TransportBudget, TransportProfile, TransportSettings};
        let server = MockServer::start(Arc::new(|_| MockResponse {
            status: 200,
            // Larger than the compact profile's encoded envelope plus its
            // bounded JSON overhead.
            body: vec![b' '; 13 * 1024 * 1024],
            delay_ms: 0,
        }))
        .await;
        let compact = TransportBudget::resolve(&TransportSettings {
            profile: TransportProfile::Compact,
            overrides: None,
        })
        .unwrap();
        let client = AccessClient::new(server.url(), None)
            .unwrap()
            .with_transport_budget(compact);
        let error = client.list_hosts().await.unwrap_err();
        assert_eq!(error.code, Some(ErrorCode::PayloadTooLarge));
        assert!(!error.message.contains("hosts"));
    }

    #[tokio::test]
    async fn additive_response_fields_are_ignored_and_unknown_enums_are_actionable() {
        // A newer peer adding a field must not break an older reader.
        let additive = MockServer::start(Arc::new(|_| MockResponse {
            status: 200,
            body: br#"{"hosts":[],"futureField":true}"#.to_vec(),
            delay_ms: 0,
        }))
        .await;
        let client = AccessClient::new(additive.url(), None).unwrap();
        assert!(client.list_hosts().await.unwrap().is_empty());

        // An enum value this reader does not know is a version mismatch, not a
        // silently parsed zero value.
        let unknown = MockServer::start(Arc::new(|_| MockResponse {
            status: 200,
            body: br#"{"requestId":"r-1","status":"FUTURE_STATUS"}"#.to_vec(),
            delay_ms: 0,
        }))
        .await;
        let client = AccessClient::new(unknown.url(), None).unwrap();
        let error = client.get_request("r-1").await.unwrap_err();
        assert_eq!(error.code, Some(ErrorCode::ProtocolVersionIncompatible));
        assert!(error.message.contains("upgrade"), "{}", error.message);
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use crate::access::transport::resolve_reference;
    use crate::api::test_support::{MockResponse, MockServer};
    use std::sync::{Arc, Mutex};
    use whalerein_proto::{PayloadPurpose, RelayObject, RelayObjectReservation};

    fn reference(bytes: &[u8]) -> PayloadReference {
        PayloadReference {
            object_id: "b".repeat(32),
            owner_node_id: "node-1".into(),
            ciphertext_bytes: bytes.len() as u64,
            sha256: crate::util::sha256(bytes).to_vec(),
            expires_at_unix_ms: 1,
            purpose: PayloadPurpose::Result as i32,
        }
    }

    #[tokio::test]
    async fn reserves_uploads_and_finishes_exact_ciphertext_in_order() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = calls.clone();
        let object_id = "c".repeat(32);
        let served_object_id = object_id.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            observed
                .lock()
                .unwrap()
                .push(format!("{} {}", mock.method, mock.path));
            match (mock.method.as_str(), mock.path.as_str()) {
                ("POST", "/v1/relay/objects") => MockResponse::json(
                    200,
                    &RelayObjectReservation {
                        object_id: served_object_id.clone(),
                        owner_node_id: "node-1".into(),
                        upload_path: format!("/v1/relay/objects/{served_object_id}"),
                        max_bytes: 8,
                        expires_at_unix_ms: 9,
                        upload_timeout_ms: 5_000,
                    },
                ),
                ("PUT", path) if path == format!("/v1/relay/objects/{served_object_id}") => {
                    assert_eq!(mock.headers.get("whalerein-relay-epoch").unwrap(), "5");
                    assert_eq!(mock.headers.get("content-length").unwrap(), "8");
                    assert_eq!(mock.body, b"cipher-!");
                    MockResponse::empty(204)
                }
                ("POST", path)
                    if path == format!("/v1/relay/objects/{served_object_id}/finish") =>
                {
                    MockResponse::json(
                        200,
                        &RelayObject {
                            object_id: served_object_id.clone(),
                            owner_node_id: "node-1".into(),
                            purpose: PayloadPurpose::Result as i32,
                            ciphertext_bytes: 8,
                            sha256: crate::util::sha256(b"cipher-!").to_vec(),
                            expires_at_unix_ms: 9,
                        },
                    )
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let client = AccessClient::new(server.url(), Some("device-token".into())).unwrap();
        let reservation = client
            .reserve_relay_object(&RelayObjectReserveRequest {
                relay_version: RELAY_EPOCH_V5,
                host_id: "h".into(),
                device_id: "d".into(),
                session_id: String::new(),
                channel_id: "c".into(),
                purpose: PayloadPurpose::Result as i32,
                part_id: String::new(),
                ciphertext_bytes: 8,
                sha256: crate::util::sha256(b"cipher-!").to_vec(),
                expires_at_unix_ms: 9,
                signed_at_unix_ms: 0,
                signature: Vec::new(),
                request_id: String::new(),
                enrollment_generation: 0,
                connection_ref: String::new(),
            })
            .await
            .unwrap();
        client
            .upload_relay_object(&reservation, b"cipher-!")
            .await
            .unwrap();
        let object = client
            .finish_relay_object(
                &object_id,
                &RelayObjectFinishRequest {
                    relay_version: RELAY_EPOCH_V5,
                    ciphertext_bytes: 8,
                    sha256: crate::util::sha256(b"cipher-!").to_vec(),
                    signed_at_unix_ms: 0,
                    signature: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(object.object_id, object_id);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "POST /v1/relay/objects".to_owned(),
                format!("PUT /v1/relay/objects/{object_id}"),
                format!("POST /v1/relay/objects/{object_id}/finish"),
            ]
        );
    }

    #[tokio::test]
    async fn resolves_an_exact_reference_and_rejects_truncation_and_corruption() {
        for (body, expected) in [
            (b"payload".to_vec(), true),
            (b"payloa".to_vec(), false),
            (b"PAYLOAD".to_vec(), false),
        ] {
            let served = body.clone();
            let server = MockServer::start(Arc::new(move |_| MockResponse {
                status: 200,
                body: served.clone(),
                delay_ms: 0,
            }))
            .await;
            let client = AccessClient::new(server.url(), Some("device-token".into())).unwrap();
            let resolved = resolve_reference(&client, &reference(b"payload"), 1024).await;
            assert_eq!(resolved.is_ok(), expected, "{body:?}");
        }

        // A stream longer than the reserved length is rejected before it can
        // become a complete object.
        let server = MockServer::start(Arc::new(|_| MockResponse {
            status: 200,
            body: b"payload-plus".to_vec(),
            delay_ms: 0,
        }))
        .await;
        let client = AccessClient::new(server.url(), Some("device-token".into())).unwrap();
        assert!(resolve_reference(&client, &reference(b"payload"), 1024)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_failed_fetch_is_terminal_and_never_a_completed_message() {
        let server = MockServer::start(Arc::new(|_| MockResponse {
            status: 410,
            body: br#"{"code":"expired","message":"expired"}"#.to_vec(),
            delay_ms: 0,
        }))
        .await;
        let client = AccessClient::new(server.url(), Some("device-token".into())).unwrap();
        let error = resolve_reference(&client, &reference(b"payload"), 1024)
            .await
            .unwrap_err();
        assert_eq!(error.code, Some(ErrorCode::Expired));
    }
}

#[cfg(test)]
mod session_proof_tests {
    use super::*;

    #[test]
    fn proof_canonical_matches_the_host_contract() {
        let hash = [0u8; 32];
        assert_eq!(
            session_proof_input(
                "session-1",
                "host-1",
                "device-1",
                "channel-1",
                &hash,
                1_700_000_000_000,
                1_700_000_030_000,
            ),
            "whalerein/v1/session-proof|session-1|host-1|device-1|channel-1|AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA|1700000000000|1700000030000"
        );
    }

    #[test]
    fn proof_signature_matches_the_host_fixture() {
        let key = URL_SAFE_NO_PAD
            .decode("MFECAQEwBQYDK2VwBCIEIEBVX8_htq2WRPytz3dGchBV_l5P71a1KpuAcmnIP6ZygSEAXWqO2VXVBSivN9KHeV_rF-5fFt_FgKfvsPPpM7FJKcs")
            .unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(&key).unwrap();
        let hash = [0u8; 32];
        let input = session_proof_input(
            "session-1",
            "host-1",
            "device-1",
            "channel-1",
            &hash,
            1_700_000_000_000,
            1_700_000_030_000,
        );
        assert_eq!(
            URL_SAFE_NO_PAD.encode(pair.sign(input.as_bytes()).as_ref()),
            "3OjP2K7k06XZdOnaCjcz9aXeHNw9Dqea41uUOiei_5U5JNvDIf_Biii4leS6l8sef92yEXlam0IOuGMAFzXeBw"
        );
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    use crate::api::test_support::{MockResponse, MockServer};
    use std::sync::{Arc, Mutex};
    use whalerein_proto::HostDirectory;

    #[tokio::test]
    async fn host_directory_uses_only_the_invitation_bearer() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let server = MockServer::start(Arc::new(move |request| {
            captured.lock().unwrap().push(request);
            MockResponse::json(
                200,
                &HostDirectory {
                    host_id: "host-1".into(),
                    display_name: "Studio Mac".into(),
                    os_family: "macos".into(),
                },
            )
        }))
        .await;
        let sink = Arc::new(crate::diagnostics::journal::CapturingSink::default());
        let mut client = AccessClient::with_trust(
            server.url(),
            Some("device-token".into()),
            None,
            None,
            Arc::new(Diagnostics::new(Some(sink.clone()))),
        )
        .unwrap();
        client.set_directory_timeout(Duration::from_millis(500));
        let preview = client
            .host_directory(server.url().as_str(), "host-1", "invitation-token")
            .await
            .unwrap();
        assert_eq!(preview.display_name, "Studio Mac");
        assert_eq!(preview.os_family, "macos");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].path, "/v1/hosts/host-1/directory");
        assert_eq!(
            seen[0].headers.get("authorization").unwrap(),
            "Bearer invitation-token"
        );
        let events = sink.0.lock().unwrap();
        let logged = serde_json::to_string(&*events).unwrap();
        assert!(!logged.contains("invitation-token"));
        assert!(!logged.contains("device-token"));
        assert!(!logged.contains("Studio Mac"));
        assert!(!logged.contains("macos"));
    }

    #[tokio::test]
    async fn host_directory_drops_invalid_fields_without_failing() {
        let server = MockServer::start(Arc::new(|_| {
            MockResponse::json(
                200,
                &HostDirectory {
                    host_id: "host-1".into(),
                    display_name: "x".repeat(MAX_DIRECTORY_DISPLAY_NAME_CHARS + 1),
                    os_family: "mac\u{0}os".into(),
                },
            )
        }))
        .await;
        let client = AccessClient::new(server.url(), None).unwrap();
        let preview = client
            .host_directory(server.url().as_str(), "host-1", "invitation-token")
            .await
            .unwrap();
        assert_eq!(preview.display_name, "");
        assert_eq!(preview.os_family, "");
    }

    #[tokio::test]
    async fn host_directory_failures_are_errors_not_panics() {
        let unavailable = MockServer::start(Arc::new(|_| {
            MockResponse::json(
                404,
                &ErrorResponse {
                    code: ErrorCode::NotFound as i32,
                    message: "not found".into(),
                    ..Default::default()
                },
            )
        }))
        .await;
        let client = AccessClient::new(unavailable.url(), None).unwrap();
        let error = client
            .host_directory(unavailable.url().as_str(), "host-1", "invitation-token")
            .await
            .unwrap_err();
        assert_eq!(error.status, Some(404));

        let malformed = MockServer::start(Arc::new(|_| MockResponse {
            status: 200,
            body: b"{not-protojson".to_vec(),
            delay_ms: 0,
        }))
        .await;
        let client = AccessClient::new(malformed.url(), None).unwrap();
        let error = client
            .host_directory(malformed.url().as_str(), "host-1", "invitation-token")
            .await
            .unwrap_err();
        assert_eq!(error.kind, AccessErrorKind::Protocol);
    }

    #[tokio::test]
    async fn host_directory_timeout_is_bounded() {
        let server = MockServer::start(Arc::new(|_| MockResponse {
            status: 200,
            body: serde_json::to_vec(&HostDirectory {
                host_id: "host-1".into(),
                display_name: "late".into(),
                os_family: "macos".into(),
            })
            .unwrap(),
            delay_ms: 2_000,
        }))
        .await;
        let mut client = AccessClient::new(server.url(), None).unwrap();
        client.set_directory_timeout(Duration::from_millis(50));
        let started = std::time::Instant::now();
        let error = client
            .host_directory(server.url().as_str(), "host-1", "invitation-token")
            .await
            .unwrap_err();
        assert!(error.is_retryable_transport());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn host_directory_refuses_a_foreign_origin() {
        let server = MockServer::start(Arc::new(|_| MockResponse::empty(200))).await;
        let client = AccessClient::new(server.url(), None).unwrap();
        let error = client
            .host_directory("http://127.0.0.1:1", "host-1", "invitation-token")
            .await
            .unwrap_err();
        assert_eq!(error.kind, AccessErrorKind::Protocol);
    }
}
