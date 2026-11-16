//! HTTPS client for the WhaleRein Access HTTP surface.
//!
//! All bodies use the proto3 JSON encoding of `proto/v1`. The client never
//! exposes these DTOs outside Core.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;
use whalerein_proto::{
    ErrorCode, ErrorResponse, HostInfo, HostList, HostRequest, OnlineStatus, PairingClaimRequest,
    PairingClaimResponse, RequestReceipt, RequestState,
};

use crate::util::now_unix_ms;

pub const TRACE_HEADER: &str = "x-whalerein-trace-id";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
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
}

impl AccessError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: AccessErrorKind::Transport,
            status: None,
            code: None,
            message: message.into(),
        }
    }

    fn not_sent() -> Self {
        Self {
            kind: AccessErrorKind::NotSent,
            status: None,
            code: None,
            message: "Access connection failed".into(),
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
        }
    }

    pub fn timeout() -> Self {
        Self {
            kind: AccessErrorKind::Timeout,
            status: None,
            code: None,
            message: "request timed out".into(),
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

/// Latest observed transport state of one in-flight read-only request.
/// A successful HTTP exchange clears an earlier transient error.
#[derive(Default)]
pub(crate) struct TransportObservation(Mutex<Option<AccessError>>);

impl TransportObservation {
    pub(crate) fn recovered(&self) {
        *self.0.lock().expect("transport observation") = None;
    }
    pub(crate) fn failed(&self, error: &AccessError) {
        *self.0.lock().expect("transport observation") = Some(error.clone());
    }
    pub(crate) fn latest_error(&self) -> Option<AccessError> {
        self.0.lock().expect("transport observation").clone()
    }
}

#[derive(Clone)]
pub struct AccessClient {
    http: Client,
    base: Url,
    token: Option<String>,
    request_timeout: Duration,
    poll_interval: Duration,
    trace_id: String,
    transport_observation: Option<Arc<TransportObservation>>,
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
        Self::with_ca(base, token, None)
    }

    /// `ca_pem` adds a private CA on top of the platform trust roots.
    pub fn with_ca(
        base: Url,
        token: Option<String>,
        ca_pem: Option<&[u8]>,
    ) -> Result<Self, AccessError> {
        if !matches!(base.scheme(), "https" | "http")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.fragment().is_some()
        {
            return Err(AccessError::protocol("invalid Access URL"));
        }
        let mut builder = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent(concat!("whalerein-core/", env!("CARGO_PKG_VERSION")));
        if let Some(ca) = ca_pem {
            if !ca
                .windows(27)
                .any(|window| window == b"-----BEGIN CERTIFICATE-----")
            {
                return Err(AccessError::protocol("invalid CA certificate"));
            }
            let parsed: Vec<_> = rustls_pemfile::certs(&mut &ca[..])
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| AccessError::protocol("invalid CA certificate"))?;
            if parsed.is_empty() {
                return Err(AccessError::protocol("invalid CA certificate"));
            }
            let certificate = reqwest::Certificate::from_pem(ca)
                .map_err(|_| AccessError::protocol("invalid CA certificate"))?;
            builder = builder.add_root_certificate(certificate);
        }
        let http = builder
            .build()
            .map_err(|_| AccessError::transport("HTTP client initialization failed"))?;
        Ok(Self {
            http,
            base,
            token,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
            trace_id: crate::util::random_id(16),
            transport_observation: None,
        })
    }

    /// Bind every HTTP call, including polling, to one ephemeral operation.
    pub fn with_trace_id(mut self, trace_id: &str) -> Self {
        self.trace_id = trace_id.to_owned();
        self
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub(crate) fn with_transport_observation(
        mut self,
        observation: Arc<TransportObservation>,
    ) -> Self {
        self.transport_observation = Some(observation);
        self
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
        builder = builder.header(TRACE_HEADER, trace_id.unwrap_or(&self.trace_id));
        builder
    }

    async fn send<T: DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<T, AccessError> {
        let started = std::time::Instant::now();
        let response = builder.send().await.map_err(|error| {
            tracing::warn!(event = "http_transport_failed", trace_id = %self.trace_id, layer = "access", code = "unavailable");
            let error = if error.is_connect() || error.is_builder() {
                AccessError::not_sent()
            } else {
                AccessError::transport("Access request failed")
            };
            if let Some(observation) = &self.transport_observation { observation.failed(&error); }
            error
        })?;
        let status = response.status().as_u16();
        let result = parse_response(response).await;
        if let Some(observation) = &self.transport_observation {
            match &result {
                Err(error) if error.is_retryable_transport() => observation.failed(error),
                _ => observation.recovered(),
            }
        }
        tracing::debug!(event = "http_result", trace_id = %self.trace_id, layer = "access", status, ms = started.elapsed().as_millis() as u64, code = ?result.as_ref().err().and_then(|error| error.code));
        result
    }

    pub async fn list_hosts(&self) -> Result<Vec<HostInfo>, AccessError> {
        let url = self.endpoint(&["v1", "hosts"])?;
        let list: HostList = self
            .send(self.request(reqwest::Method::GET, url, None))
            .await?;
        Ok(list.hosts)
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
        tracing::info!(event = "request_submitted", trace_id = %request.trace_id, request_id = %request.request_id, kind = request.kind, bytes = request.body.len());
        self.send(builder).await
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
        Err(parse_error_response(response).await)
    }

    pub async fn get_request(&self, request_id: &str) -> Result<RequestState, AccessError> {
        let url = self.endpoint(&["v1", "requests", request_id])?;
        self.send(self.request(reqwest::Method::GET, url, None))
            .await
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
        Err(parse_error_response(response).await)
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

    /// Poll one request until it reaches a terminal status (or `host_accepted`
    /// when requested, for control commands).
    pub async fn wait_for_request(
        &self,
        request_id: &str,
        allow_host_accepted: bool,
    ) -> Result<RequestState, AccessError> {
        self.wait_for_request_observed(request_id, allow_host_accepted, None)
            .await
    }

    pub(crate) async fn wait_for_request_observed(
        &self,
        request_id: &str,
        allow_host_accepted: bool,
        observation: Option<&TransportObservation>,
    ) -> Result<RequestState, AccessError> {
        let deadline = now_unix_ms() + self.request_timeout.as_millis() as i64;
        loop {
            match self.get_request(request_id).await {
                Ok(state) => {
                    if let Some(observation) = observation {
                        observation.recovered();
                    }
                    if is_terminal(state.status)
                        || (allow_host_accepted
                            && state.status == OnlineStatus::HostAccepted as i32)
                    {
                        return Ok(state);
                    }
                }
                Err(error) if error.is_retryable_transport() => {
                    if let Some(observation) = observation {
                        observation.failed(&error);
                    }
                }
                Err(error) => return Err(error),
            }
            if now_unix_ms() >= deadline {
                return Err(AccessError::timeout());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

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

async fn parse_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, AccessError> {
    if !response.status().is_success() {
        return Err(parse_error_response(response).await);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| AccessError::transport("Access response could not be read"))?;
    crate::api::decode_json(&bytes)
}

async fn parse_error_response(response: reqwest::Response) -> AccessError {
    let status = response.status();
    let bytes = response.bytes().await.unwrap_or_default();
    let parsed = serde_json::from_slice::<ErrorResponse>(&bytes).ok();
    let (code, message) = match parsed {
        Some(error) => (
            ErrorCode::try_from(error.code).ok(),
            sanitize_message(error.message),
        ),
        None => (None, default_status_message(status)),
    };
    AccessError {
        kind: AccessErrorKind::Http,
        status: Some(status.as_u16()),
        code: code.or_else(|| status_to_error_code(status)),
        message,
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
                        request_id: "r-1".into(),
                        status: OnlineStatus::Received as i32,
                        command_id: String::new(),
                        expires_at_unix_ms: 1,
                    },
                ),
                ("GET", "/v1/requests/r-1") => MockResponse::json(
                    200,
                    &RequestState {
                        request_id: "r-1".into(),
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
        let client = AccessClient::new(server.url(), Some("device-token".into()))
            .unwrap()
            .with_trace_id("trace-1");

        let hosts = client.list_hosts().await.unwrap();
        assert_eq!(hosts[0].host_id, "h-1");
        let receipt = client.submit_request("h-1", &request()).await.unwrap();
        assert_eq!(receipt.status, OnlineStatus::Received as i32);
        let state = client.get_request("r-1").await.unwrap();
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
        assert_eq!(submitted.request_id, "r-1");
        assert_eq!(seen[1].2.get(TRACE_HEADER).unwrap(), "trace-1");
        assert_eq!(seen[2].2.get(TRACE_HEADER).unwrap(), "trace-1");
        assert_eq!(seen[4].1, "/v1/pairing/claims");
        assert!(!seen[4].2.contains_key("authorization"));
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
        let mut client = AccessClient::new(server.url(), None).unwrap();
        client.set_poll_timing(Duration::from_secs(5), Duration::from_millis(1));
        let state = client.wait_for_request("r", false).await.unwrap();
        assert_eq!(state.status, OnlineStatus::Succeeded as i32);
        assert_eq!(*calls.lock().unwrap(), 3);
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
            client.wait_for_request("r", true).await,
            Err(AccessError {
                kind: AccessErrorKind::Timeout,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn wait_transport_observation_clears_only_after_a_successful_poll() {
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
        let mut client = AccessClient::new(server.url(), None).unwrap();
        client.set_poll_timing(Duration::from_secs(5), Duration::from_millis(5));
        let observation = TransportObservation::default();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            client.wait_for_request_observed("r", false, Some(&observation)),
        )
        .await;
        assert!(result.is_err());
        assert!(*calls.lock().unwrap() >= 2);
        assert!(observation.latest_error().is_none());

        let unavailable = MockServer::start(Arc::new(|_| MockResponse {
            status: 0,
            body: Vec::new(),
            delay_ms: 0,
        }))
        .await;
        let mut client = AccessClient::new(unavailable.url(), None).unwrap();
        client.set_poll_timing(Duration::from_secs(5), Duration::from_millis(5));
        let observation = TransportObservation::default();
        assert!(tokio::time::timeout(
            Duration::from_millis(100),
            client.wait_for_request_observed("r", false, Some(&observation))
        )
        .await
        .is_err());
        assert!(observation
            .latest_error()
            .is_some_and(|error| error.is_retryable_transport()));
    }

    #[test]
    fn invalid_ca_is_rejected() {
        let error = AccessClient::with_ca(
            Url::parse("https://example.test").unwrap(),
            None,
            Some(b"not a certificate"),
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
            message: "x".into()
        }
        .is_retryable_transport());
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
