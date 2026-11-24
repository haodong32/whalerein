//! Command submission, result polling and the command projection.
//!
//! Only this module writes the command journal. Intent is durable before I/O;
//! only proven-unsent failures remove it. Network retries reuse the same
//! request identity and encrypted payload. Cancellation keeps recovery metadata.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use prost::Message;
use whalerein_proto::{
    CommandAccepted, CommandResult, ErrorCode, GetCommandResult, HostRequest, PayloadPurpose,
    PayloadReference, RelayObjectFinishRequest, RelayObjectReserveRequest, RequestKind,
    RequestPayload, RequestReceipt, RequestState, RequestStateNotice, RequestTransportMetadata,
    ResponsePayload,
};

use crate::access::{
    http::{receipt_matches_request, TransportObservation, FINAL_LOOKUP_RESERVE},
    transport::TransportBudget,
    AccessClient,
};
use crate::api::{
    error_code_name, CommandHandle, CoreError, CoreResult, LifecycleReceipt,
    OnlineStatus as DomainStatus, QuestionReceiptInfo,
};
use crate::cache::{Cache, CommandIntent};
use crate::connection::{HostConnection, HostConnections, NoticeEvent, NoticeGuard};
use crate::pairing::PROTOCOL_VERSION;
use crate::security::{decode_frame, encode_frame, RequestAad, SecureChannel};
use crate::util::{now_unix_ms, random_id, sha256};

pub const REQUEST_TTL_MS: i64 = 30_000;
pub const MAX_SUBMIT_ATTEMPTS: u32 = 3;
/// How long a resumed foreground command waits for a terminal result notice
/// before spending its reserved authoritative lookup.
pub const FOREGROUND_WAIT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

struct SubmitFailure {
    error: CoreError,
    definitely_unsent: bool,
}

struct Admission {
    config: crate::api::config::ConfigStore,
    profile: std::path::PathBuf,
    secrets: std::sync::Arc<dyn crate::security::SecretStore>,
}

/// Why a heartbeat stopped renewing. A terminal response (404/expired/closed)
/// is observable so renewal never loops after the exact request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// The exact request is no longer renewable; renewal stopped.
    Closed,
    /// Renewal ended with a transient transport failure.
    TransportFailed,
}

/// Bounded, non-blocking renewal of one command's original request identity.
///
/// The task renewal is independent of result polling, so a slow heartbeat can
/// never stall terminal-result or deadline progress. Dropping the guard stops
/// renewal; there is no detached heartbeat after completion or cancellation.
/// A 404/expired/terminal response stops renewal instead of looping.
struct CommandHeartbeat {
    stop: Arc<AtomicBool>,
    /// Latest closure/failure fact. Renewal stops on a terminal response and
    /// the guard stops on completion, so no detached heartbeat survives.
    outcome: Arc<std::sync::Mutex<Option<HeartbeatOutcome>>>,
    task: Option<JoinHandle<()>>,
}

impl CommandHeartbeat {
    fn start(access: Arc<AccessClient>, request_id: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let outcome = Arc::new(std::sync::Mutex::new(None));
        let task_stop = stop.clone();
        let task_outcome = outcome.clone();
        let task = tokio::spawn(async move {
            loop {
                if task_stop.load(Ordering::SeqCst) {
                    break;
                }
                match access.heartbeat_request(&request_id).await {
                    Ok(()) => {}
                    Err(error) if error.is_retryable_transport() => {
                        *task_outcome.lock().expect("heartbeat outcome") =
                            Some(HeartbeatOutcome::TransportFailed);
                    }
                    Err(_) => {
                        // The exact request is expired, cancelled or terminal.
                        // Renewing again cannot help and could mask closure.
                        *task_outcome.lock().expect("heartbeat outcome") =
                            Some(HeartbeatOutcome::Closed);
                        break;
                    }
                }
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            }
        });
        Self {
            stop,
            outcome,
            task: Some(task),
        }
    }

    /// Latest observable heartbeat outcome, if any renewal failed.
    #[allow(dead_code)]
    fn outcome(&self) -> Option<HeartbeatOutcome> {
        *self.outcome.lock().expect("heartbeat outcome")
    }
}

impl Drop for CommandHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Attached before submission. Signals are scoped to this command identity
/// and arrive through the shared Host connection owner; dropping the guard
/// removes only this waiter.
struct ForegroundNotices {
    signals: mpsc::Receiver<NoticeEvent>,
    _guard: NoticeGuard,
    /// The shared connection was already subscribed when this waiter attached.
    receiver_ready: bool,
    last_seen: Option<(String, i64)>,
}

impl ForegroundNotices {
    /// Return true when this notice is new and matches the invocation scope.
    /// Foreign, stale and duplicate notices are ignored and never fetched.
    fn accept(&mut self, notice: &RequestStateNotice, request: &HostRequest) -> bool {
        if notice.request_id != request.request_id
            || notice.host_id != request.host_id
            || notice.device_id != request.device_id
            || notice.command_id != request.command_id
        {
            return false;
        }
        let seen = self.last_seen.as_ref().is_some_and(|(id, updated)| {
            id == &notice.request_id && *updated >= notice.updated_at_unix_ms
        });
        if seen {
            return false;
        }
        self.last_seen = Some((notice.request_id.clone(), notice.updated_at_unix_ms));
        true
    }
}

pub struct CommandService {
    access: Arc<AccessClient>,
    cache: Arc<Cache>,
    host_fingerprint: Option<String>,
    admission: Option<Admission>,
    transport: TransportBudget,
    outbound_envelope_bytes: u64,
    /// Largest complete inline request this device sends. A larger sealed body
    /// is uploaded and submitted as a reference before any submit attempt.
    outbound_inline_bytes: u64,
    /// The shared per-Host connection owner. The query lane and command result
    /// notices both travel through it; a Core instance never opens a second
    /// foreground connection.
    connections: Option<Arc<HostConnections>>,
    connection: Option<Arc<HostConnection>>,
    /// Test-only legacy HTTP query lane. The production query path is MQTT;
    /// this keeps the low-level commands fixtures that exercise the shared
    /// representation/decryption code without a broker meaningful. It is never
    /// reachable from a `Core` (which injects its connection registry).
    #[cfg(test)]
    legacy_http_query: bool,
}

impl CommandService {
    pub fn new(access: Arc<AccessClient>, cache: Arc<Cache>) -> Self {
        let transport = TransportBudget::default();
        Self {
            access,
            cache,
            host_fingerprint: None,
            admission: None,
            transport,
            outbound_envelope_bytes: transport.envelope_bytes,
            outbound_inline_bytes: transport.inline_request_bytes(),
            connections: None,
            connection: None,
            #[cfg(test)]
            legacy_http_query: true,
        }
    }

    /// Inject the shared Host connection registry owned by the Core instance.
    /// Queries and command notices never open an auxiliary connection.
    pub(crate) fn with_connections(mut self, connections: Arc<HostConnections>) -> Self {
        self.connections = Some(connections);
        #[cfg(test)]
        {
            self.legacy_http_query = false;
        }
        self
    }

    /// Inject one exact shared connection for a caller that already resolved
    /// and prepared it.
    #[cfg(test)]
    pub(crate) fn with_host_connection(mut self, connection: Arc<HostConnection>) -> Self {
        self.connection = Some(connection);
        #[cfg(test)]
        {
            self.legacy_http_query = false;
        }
        self
    }

    fn connection_for(&self, host_id: &str) -> CoreResult<Arc<HostConnection>> {
        if let Some(connection) = &self.connection {
            if connection.host_id() == host_id {
                return Ok(connection.clone());
            }
        }
        if let Some(connections) = &self.connections {
            if let Some(connection) = connections.running(host_id) {
                return Ok(connection);
            }
        }
        Err(CoreError::Unavailable(
            "host connection is not ready for the query lane".into(),
        ))
    }

    pub(crate) fn with_transport_budget(mut self, transport: TransportBudget) -> Self {
        self.transport = transport;
        // Locally valid overrides take effect; the peer minimum is applied by
        // `with_outbound_envelope` once the channel advertisement is known.
        self.outbound_envelope_bytes = transport.envelope_bytes;
        self.outbound_inline_bytes = transport.inline_request_bytes();
        self
    }

    pub(crate) fn with_outbound_envelope(mut self, envelope_bytes: u64) -> Self {
        self.outbound_envelope_bytes = envelope_bytes.min(self.transport.envelope_bytes);
        self
    }

    pub(crate) fn with_admission(
        mut self,
        config: crate::api::config::ConfigStore,
        profile: std::path::PathBuf,
        secrets: std::sync::Arc<dyn crate::security::SecretStore>,
    ) -> Self {
        self.admission = Some(Admission {
            config,
            profile,
            secrets,
        });
        self
    }

    pub(crate) fn with_host_fingerprint(mut self, fingerprint: Option<String>) -> Self {
        self.host_fingerprint = fingerprint;
        self
    }

    /// Submit an encrypted read-only query and wait for its terminal result.
    pub async fn query(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        payload: RequestPayload,
    ) -> CoreResult<ResponsePayload> {
        self.query_inner(host_id, device_id, channel, payload, None, None)
            .await
    }

    pub(crate) async fn query_wait(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        payload: RequestPayload,
        deadline: Instant,
        observation: &TransportObservation,
    ) -> CoreResult<ResponsePayload> {
        self.query_inner(
            host_id,
            device_id,
            channel,
            payload,
            Some(deadline),
            Some(observation),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn query_inner(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        mut payload: RequestPayload,
        wait_deadline: Option<Instant>,
        transport: Option<&TransportObservation>,
    ) -> CoreResult<ResponsePayload> {
        #[cfg(test)]
        {
            if self.legacy_http_query && self.connection_for(host_id).is_err() {
                return self
                    .query_inner_http(
                        host_id,
                        device_id,
                        channel,
                        payload,
                        wait_deadline,
                        transport,
                    )
                    .await;
            }
        }
        let _ = transport;
        let source_sensitive = matches!(
            payload.payload.as_ref(),
            Some(whalerein_proto::request_payload::Payload::WaitSession(_))
                | Some(whalerein_proto::request_payload::Payload::ReadWaitState(_))
        );
        let safe_to_retry = safe_query_to_retry(&payload);
        let request_id = random_id(16);
        let expires_at = now_unix_ms() + REQUEST_TTL_MS;
        if let Some(deadline) = wait_deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // The caller owns the overall deadline and distinguishes it
                // from a transport error; let its timeout resolve this case.
                return std::future::pending().await;
            }
            let remaining_ms = remaining.as_nanos().div_ceil(1_000_000) as u64;
            let Some(whalerein_proto::request_payload::Payload::WaitSession(wait)) =
                payload.payload.as_mut()
            else {
                return Err(CoreError::InvalidInput("wait payload missing".into()));
            };
            // One MQTT request/reply carries the slice; the caller keeps the
            // overall deadline and resends with whatever time remains.
            wait.timeout_ms = remaining_ms.min(u64::from(u32::MAX)) as u32;
        }
        let aad = RequestAad {
            request_id: &request_id,
            host_id,
            device_id,
            kind: RequestKind::Query,
            command_id: "",
            expires_at_unix_ms: expires_at,
        };
        let frame = channel.seal_request(&aad, &payload.encode_to_vec())?;
        let mut request = build_request(
            &request_id,
            host_id,
            device_id,
            RequestKind::Query,
            "",
            Vec::new(),
            expires_at,
            channel.channel_id(),
            encode_frame(&frame),
        );
        request.trace_id = self.access.trace_id().to_owned();
        // This field is the encoded HTTPS result bound, not the raw logical
        // envelope; Access converts it back to a raw ciphertext allowance.
        request.transport = Some(RequestTransportMetadata {
            result_max_bytes: crate::access::http::https_body_limit(self.transport.envelope_bytes)
                as u64,
            grant_generation: 0,
            revocation_generation: 0,
            enrollment_generation: 0,
        });
        self.ensure_forwardable(&request)?;
        let connection = self.connection_for(host_id)?;
        let result = self
            .query_lane(&connection, &request, safe_to_retry)
            .await?;
        let bound = self.admitted_result_bound(&request);
        let state = self.resolve_query_reply(result, bound).await?;
        let status = DomainStatus::from_proto(state.status).unwrap_or(DomainStatus::Unknown);
        let result = decrypt_response_raw(channel, &aad, &state).map_err(|error| {
            if source_sensitive
                && !state.result_body.is_empty()
                && matches!(error, CoreError::ChannelMismatch)
            {
                CoreError::Security("wait response channel could not be verified".into())
            } else {
                error
            }
        });
        match status {
            DomainStatus::Succeeded => result.and_then(|payload| {
                if payload.error_code == 0 {
                    Ok(payload)
                } else if source_sensitive {
                    Err(source_response_error(&payload))
                } else {
                    Err(response_error(&payload))
                }
            }),
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                // A failed result may still carry an encrypted domain error.
                match result {
                    Ok(payload) if source_sensitive && payload.error_code != 0 => {
                        Err(source_response_error(&payload))
                    }
                    Ok(payload) => Err(response_error(&payload)),
                    // Any non-empty encrypted error body claims authentication;
                    // never mask corruption with its cleartext status copy.
                    Err(error) if !state.result_body.is_empty() => Err(error),
                    Err(_) => Err(status_error(status, error_code_of(&state))),
                }
            }
            DomainStatus::Unknown => Err(CoreError::UnknownResult),
            other => Err(CoreError::Internal(format!(
                "query ended in {}",
                other.as_str()
            ))),
        }
    }

    /// Publish one query on the shared MQTT lane and wait on its correlated
    /// waiter. A connection replacement or broker loss is retryable only for a
    /// safe read, and retries keep the original application deadline.
    async fn query_lane(
        &self,
        connection: &Arc<HostConnection>,
        request: &HostRequest,
        safe_to_retry: bool,
    ) -> CoreResult<whalerein_proto::HostResult> {
        let cancellation = crate::api::Cancellation::new();
        let mut attempts = 0u32;
        loop {
            match connection.query(request, &cancellation).await {
                Ok(result) => return Ok(result),
                Err(error)
                    if safe_to_retry
                        && error.is_retryable_transport()
                        && now_unix_ms() < request.expires_at_unix_ms
                        && attempts < 8 =>
                {
                    attempts = attempts.saturating_add(1);
                    // Wait for a usable generation before reissuing. The
                    // request identity and deadline are unchanged.
                    if let Err(error) = connection.wait_ready(&cancellation).await {
                        if error.is_retryable_transport() {
                            tokio::time::sleep(Duration::from_millis(100 * u64::from(attempts)))
                                .await;
                            continue;
                        }
                        return Err(error);
                    }
                    if !connection.is_connected() {
                        tokio::time::sleep(Duration::from_millis(100 * u64::from(attempts))).await;
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Convert one MQTT reply into the same stored-status shape the rest of the
    /// query path decrypts, resolving a result reference through its existing
    /// HTTPS object binding and bound.
    async fn resolve_query_reply(
        &self,
        result: whalerein_proto::HostResult,
        max_ciphertext_bytes: u64,
    ) -> CoreResult<RequestState> {
        let body = crate::access::transport::resolve_query_reply_body(
            &self.access,
            &result,
            max_ciphertext_bytes,
        )
        .await
        .map_err(CoreError::from)?;
        Ok(RequestState {
            request_id: result.request_id,
            host_id: result.host_id,
            device_id: result.device_id,
            status: result.status,
            command_id: result.command_id,
            payload_digest: result.payload_digest,
            result_body: body.unwrap_or_default(),
            error_code: result.error_code,
            error_message: result.error_message,
            channel_id: result.channel_id,
            ..Default::default()
        })
    }

    #[cfg(test)]
    async fn query_inner_http(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        mut payload: RequestPayload,
        wait_deadline: Option<Instant>,
        transport: Option<&TransportObservation>,
    ) -> CoreResult<ResponsePayload> {
        let source_sensitive = matches!(
            payload.payload.as_ref(),
            Some(whalerein_proto::request_payload::Payload::WaitSession(_))
                | Some(whalerein_proto::request_payload::Payload::ReadWaitState(_))
        );
        let request_id = random_id(16);
        let expires_at = now_unix_ms() + REQUEST_TTL_MS;
        if let Some(deadline) = wait_deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // The caller owns the overall deadline and distinguishes it
                // from a transport error; let its timeout resolve this case.
                return std::future::pending().await;
            }
            let remaining_ms = remaining.as_nanos().div_ceil(1_000_000) as u64;
            let transport_ms = (expires_at - now_unix_ms()).max(0) as u64;
            let poll_ms = self.access.request_timeout().as_millis() as u64;
            let available_ms = transport_ms.min(poll_ms).saturating_sub(2_000);
            if available_ms == 0 {
                return Err(CoreError::Timeout);
            }
            let response_allowance = (remaining_ms / 5).min(500);
            let slice_ms = remaining_ms
                .saturating_sub(response_allowance)
                .min(available_ms)
                .max(1);
            let Some(whalerein_proto::request_payload::Payload::WaitSession(wait)) =
                payload.payload.as_mut()
            else {
                return Err(CoreError::InvalidInput("wait payload missing".into()));
            };
            wait.timeout_ms = slice_ms as u32;
        }
        let aad = RequestAad {
            request_id: &request_id,
            host_id,
            device_id,
            kind: RequestKind::Query,
            command_id: "",
            expires_at_unix_ms: expires_at,
        };
        let frame = channel.seal_request(&aad, &payload.encode_to_vec())?;
        let mut request = build_request(
            &request_id,
            host_id,
            device_id,
            RequestKind::Query,
            "",
            Vec::new(),
            expires_at,
            channel.channel_id(),
            encode_frame(&frame),
        );
        request.trace_id = self.access.trace_id().to_owned();
        // This field is the encoded HTTPS result bound, not the raw logical
        // envelope; Access converts it back to a raw ciphertext allowance.
        request.transport = Some(RequestTransportMetadata {
            result_max_bytes: crate::access::http::https_body_limit(self.transport.envelope_bytes)
                as u64,
            grant_generation: 0,
            revocation_generation: 0,
            enrollment_generation: 0,
        });
        self.ensure_forwardable(&request)?;
        let receipt = self
            .submit_observed(&request, transport)
            .await
            .map_err(|failure| failure.error)?;
        if let Some(status) = DomainStatus::from_proto(receipt.status) {
            if status.is_terminal() && status != DomainStatus::Succeeded {
                return Err(status_error(status, None));
            }
        }
        let state = self
            .access
            .wait_for_request_observed(&request, false, transport)
            .await?;
        let bound = self.admitted_result_bound(&request);
        let state = self.resolve_state(state, bound).await?;
        let status = DomainStatus::from_proto(state.status).unwrap_or(DomainStatus::Unknown);
        let result = decrypt_response_raw(channel, &aad, &state).map_err(|error| {
            if source_sensitive
                && !state.result_body.is_empty()
                && matches!(error, CoreError::ChannelMismatch)
            {
                CoreError::Security("wait response channel could not be verified".into())
            } else {
                error
            }
        });
        match status {
            DomainStatus::Succeeded => result.and_then(|payload| {
                if payload.error_code == 0 {
                    Ok(payload)
                } else if source_sensitive {
                    Err(source_response_error(&payload))
                } else {
                    Err(response_error(&payload))
                }
            }),
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                // A failed result may still carry an encrypted domain error.
                match result {
                    Ok(payload) if source_sensitive && payload.error_code != 0 => {
                        Err(source_response_error(&payload))
                    }
                    Ok(payload) => Err(response_error(&payload)),
                    // Any non-empty encrypted error body claims authentication;
                    // never mask corruption with its cleartext status copy.
                    Err(error) if !state.result_body.is_empty() => Err(error),
                    Err(_) => Err(status_error(status, error_code_of(&state))),
                }
            }
            DomainStatus::Unknown => Err(CoreError::UnknownResult),
            other => Err(CoreError::Internal(format!(
                "query ended in {}",
                other.as_str()
            ))),
        }
    }

    /// Submit an encrypted control command. Returns as soon as the host has
    /// accepted it, or at its terminal state.
    pub async fn control(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        payload: RequestPayload,
    ) -> CoreResult<CommandHandle> {
        self.control_with_id(host_id, device_id, channel, payload, None)
            .await
    }

    pub async fn control_with_id(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        payload: RequestPayload,
        supplied_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        self.control_with_identity(host_id, device_id, channel, payload, supplied_id, None)
            .await
    }

    pub async fn control_with_identity(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        payload: RequestPayload,
        supplied_id: Option<&str>,
        supplied_request_id: Option<&str>,
    ) -> CoreResult<CommandHandle> {
        let (command_kind, command_parent_session_id) = match &payload.payload {
            Some(whalerein_proto::request_payload::Payload::CreateSession(_)) => ("create", ""),
            Some(whalerein_proto::request_payload::Payload::ForkSession(request)) => {
                ("fork", request.session_id.as_str())
            }
            _ => ("", ""),
        };
        let command_id = match supplied_id {
            Some("") => {
                return Err(CoreError::InvalidInput(
                    "command identity is required".into(),
                ));
            }
            Some(value) => value.to_owned(),
            None => random_id(16),
        };
        let request_id = supplied_request_id.map_or_else(|| random_id(16), str::to_owned);
        let expires_at = now_unix_ms() + REQUEST_TTL_MS;
        let aad = RequestAad {
            request_id: &request_id,
            host_id,
            device_id,
            kind: RequestKind::Command,
            command_id: &command_id,
            expires_at_unix_ms: expires_at,
        };
        let frame = channel.seal_request(&aad, &payload.encode_to_vec())?;
        let body = encode_frame(&frame);
        let digest = sha256(&body).to_vec();
        let mut request = build_request(
            &request_id,
            host_id,
            device_id,
            RequestKind::Command,
            &command_id,
            digest,
            expires_at,
            channel.channel_id(),
            body,
        );
        request.trace_id = self.access.trace_id().to_owned();
        // This field is the encoded HTTPS result bound, not the raw logical
        // envelope; Access converts it back to a raw ciphertext allowance.
        request.transport = Some(RequestTransportMetadata {
            result_max_bytes: crate::access::http::https_body_limit(self.transport.envelope_bytes)
                as u64,
            grant_generation: 0,
            revocation_generation: 0,
            enrollment_generation: 0,
        });
        self.ensure_forwardable(&request)?;
        let admission_guard = if let Some(admission) = &self.admission {
            #[cfg(feature = "protocol-testing")]
            if let Some(path) = std::env::var_os("WHALEREIN_TEST_ADMISSION_READY") {
                std::fs::write(path, b"waiting")
                    .map_err(|_| CoreError::Internal("test barrier failed".into()))?;
            }
            let guard = crate::security::device::ProfileLock::acquire_admission_async(
                &admission.profile,
                std::time::Instant::now()
                    + std::time::Duration::from_millis(
                        expires_at.saturating_sub(now_unix_ms()).max(0) as u64,
                    ),
            )
            .await
            .map_err(|_| CoreError::Unavailable("profile admission lock unavailable".into()))?;
            if now_unix_ms() >= expires_at {
                return Err(CoreError::Unavailable(
                    "request expired before admission".into(),
                ));
            }
            let config = admission.config.load()?.ok_or(CoreError::NotPaired)?;
            let current = match self.access.cloud_session_current(Some(device_id))? {
                Some(true) => true,
                Some(false) => return Err(CoreError::AuthenticationRequired),
                None => {
                    let token = crate::security::load_device_token(&admission.secrets, device_id)?
                        .ok_or(CoreError::NotPaired)?;
                    self.access.has_token(&token)
                }
            };
            let key = admission
                .secrets
                .load(&crate::security::keys::cache_key(device_id))?
                .ok_or(CoreError::NotPaired)?;
            if let Some(state) = crate::api::core::revoked_commands::marker_in(guard.directory())? {
                return Err(if state.starts_with("confirmed") {
                    #[cfg(windows)]
                    self.cache.release_for_revocation()?;
                    CoreError::Revoked
                } else {
                    CoreError::Unavailable("authorization verification is pending; inspect the pairing or retry revocation".into())
                });
            }
            if config.device_id != device_id
                || url::Url::parse(&config.access_url).ok().as_ref() != Some(self.access.base_url())
                || !current
                || !self.cache.has_key(&key)
            {
                return Err(CoreError::Revoked);
            }
            Some(guard)
        } else {
            None
        };
        self.cache.record_typed_command_intent(CommandIntent {
            request_id: &request_id,
            command_id: &command_id,
            host_id,
            device_id,
            access_url: self.access.base_url().as_str(),
            host_fingerprint: self.host_fingerprint.as_deref().unwrap_or(""),
            command_kind,
            command_parent_session_id,
        })?;
        drop(admission_guard);
        // Attach the scoped MQTT receiver before the request can be submitted,
        // so a result published before the wait is installed is still routed
        // to this invocation.
        let mut notices = self.attach_notices(&request);
        // A sealed body above the effective encoded request budget is uploaded
        // and finished first. Only then is the reference submitted. The
        // original request id, command id and payload digest are unchanged, so
        // an upload retry can never create a second execution.
        if let Err(error) = self.ensure_representation(&mut request).await {
            drop(notices.take());
            self.cache.remove_unsent_command(&request_id)?;
            return Err(error);
        }
        let receipt = match self.submit(&request).await {
            Ok(receipt) => receipt,
            Err(failure) => {
                if failure.definitely_unsent {
                    self.cache.remove_unsent_command(&request_id)?;
                }
                return Err(failure.error);
            }
        };
        // The original R is registered now; renew exactly it while the Host
        // acceptance receipt and the terminal result are awaited.
        let _heartbeat = CommandHeartbeat::start(self.access.clone(), request_id.clone());
        let receipt_status =
            DomainStatus::from_proto(receipt.status).unwrap_or(DomainStatus::Unknown);
        // A terminal copy can describe a previously attempted command. In
        // particular, an MQTT completion ACK may be lost after delivery, so
        // host_offline is not by itself proof that the command was unsent.
        match receipt_status {
            DomainStatus::Unknown => {
                self.project(&request_id, &command_id, receipt_status, None)?;
                return Err(CoreError::UnknownResult);
            }
            DomainStatus::Received | DomainStatus::HostAccepted => {
                self.project(&request_id, &command_id, receipt_status, None)?;
            }
            DomainStatus::Succeeded
            | DomainStatus::Failed
            | DomainStatus::Rejected
            | DomainStatus::HostOffline => {}
        }

        let state = self
            .wait_foreground(
                notices.as_mut(),
                &request,
                true,
                Some(receipt.expires_at_unix_ms),
            )
            .await?;
        let bound = self.admitted_result_bound(&request);
        let state = self.resolve_state(state, bound).await?;
        let status = DomainStatus::from_proto(state.status).unwrap_or(DomainStatus::Unknown);
        if matches!(
            status,
            DomainStatus::Received | DomainStatus::HostAccepted | DomainStatus::Unknown
        ) {
            self.project(&request_id, &command_id, status, error_code_of(&state))?;
        }

        let decrypted = check_result_bound(&state, self.transport.envelope_bytes)
            .and_then(|()| decrypt_response(channel, &aad, &state));
        match status {
            DomainStatus::HostAccepted | DomainStatus::Succeeded => {
                let (message_id, delivery_id, model_selection, lifecycle_receipt, question_receipt) =
                    match decrypted {
                        Ok(payload) => (
                            message_id_of(&payload),
                            delivery_id_of(&payload),
                            model_selection_of(&payload),
                            lifecycle_receipt_of(&payload)?,
                            question_receipt_of(&payload),
                        ),
                        Err(CoreError::ChannelMismatch) => {
                            return Err(CoreError::StaleCommand {
                                request_id: request_id.clone(),
                                command_id: command_id.clone(),
                            });
                        }
                        Err(CoreError::UnknownResult)
                            if status == DomainStatus::HostAccepted
                                && state.result_body.is_empty() =>
                        {
                            (None, None, None, None, None)
                        }
                        Err(error) => return Err(error),
                    };
                if status == DomainStatus::Succeeded
                    && !command_kind.is_empty()
                    && !lifecycle_receipt_matches(
                        lifecycle_receipt.as_ref(),
                        command_kind,
                        command_parent_session_id,
                    )
                {
                    return Err(CoreError::UnknownResult);
                }
                if status == DomainStatus::Succeeded {
                    self.project(&request_id, &command_id, status, error_code_of(&state))?;
                }
                Ok(CommandHandle {
                    request_id,
                    command_id,
                    status,
                    message_id,
                    delivery_id,
                    model_selection,
                    lifecycle_receipt,
                    question_receipt,
                })
            }
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                match decrypted {
                    // An authenticated success payload contradicts the outer
                    // negative status; recover from the Host journal.
                    Ok(_) | Err(CoreError::ChannelMismatch) => Err(CoreError::StaleCommand {
                        request_id: request_id.clone(),
                        command_id: command_id.clone(),
                    }),
                    Err(error @ CoreError::Host { .. }) => {
                        self.project(&request_id, &command_id, status, error_code_of(&state))?;
                        Err(error)
                    }
                    // A bodyless negative status is not source proof. Ask the
                    // Host journal about this same identity without replaying it.
                    Err(CoreError::UnknownResult) if state.result_body.is_empty() => {
                        Err(CoreError::StaleCommand {
                            request_id: request_id.clone(),
                            command_id: command_id.clone(),
                        })
                    }
                    Err(error) => Err(error),
                }
            }
            DomainStatus::Unknown => Err(CoreError::UnknownResult),
            other => Err(CoreError::Internal(format!(
                "command ended in {}",
                other.as_str()
            ))),
        }
    }

    /// Wait for the host to produce a terminal payload for an already accepted
    /// command. The wait is notification-driven: it attaches the scoped MQTT
    /// receiver, reconciles the original command once, reads the authoritative
    /// result only on a matching notice or reconnect, and reserves exactly one
    /// final read before reporting an unconfirmed outcome. A fresh secure
    /// channel still recovers the original outcome through `GetCommandResult`.
    pub async fn await_command_result(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        request_id: &str,
        command_id: &str,
    ) -> CoreResult<CommandHandle> {
        // Terminal-result queries use their own request ids. Renewal must
        // remain bound to the original accepted command R.
        let _heartbeat = CommandHeartbeat::start(self.access.clone(), request_id.to_owned());
        // Attach before resuming the wait so an already-published terminal
        // notice is not lost.
        let mut notices = self.attach_command_notices(host_id, device_id, request_id, command_id);
        let budget = FOREGROUND_WAIT + FINAL_LOOKUP_RESERVE;
        let final_reserve = FINAL_LOOKUP_RESERVE.min(budget);
        let deadline = Instant::now() + budget;

        // Reconcile once on attach/resume by the original command identity.
        let probe = self
            .get_command_result(host_id, device_id, channel, command_id)
            .await;
        if let Some(outcome) = self.command_probe_outcome(request_id, command_id, probe) {
            return outcome;
        }
        let mut recovery_attempt = 0u32;
        let mut receiver_ready = notices.as_ref().is_some_and(|watch| watch.receiver_ready);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            if remaining <= final_reserve {
                break;
            }
            let slice = (remaining - final_reserve).min(Duration::from_secs(2));
            let mut read_now = false;
            if receiver_ready {
                match notices.as_mut() {
                    Some(watch) => match tokio::time::timeout(slice, watch.signals.recv()).await {
                        Ok(Some(NoticeEvent::Notice(notice))) => {
                            // The notice carries the original request/command
                            // identity; it is a hint, never the result itself.
                            let scope = HostRequest {
                                request_id: request_id.to_owned(),
                                host_id: host_id.to_owned(),
                                device_id: device_id.to_owned(),
                                command_id: command_id.to_owned(),
                                ..Default::default()
                            };
                            read_now = watch.accept(&notice, &scope);
                        }
                        Ok(Some(NoticeEvent::Reconnected)) => read_now = true,
                        Ok(None) => break,
                        // A connected receiver with no notice performs no
                        // status fetch: the final lookup covers a lost notice.
                        Err(_) => {}
                    },
                    None => break,
                }
            } else {
                // Until the first successful subscription the receiver cannot
                // deliver notices. Bounded recovery reads keep an already
                // accepted command recoverable without a fixed poll loop.
                match notices.as_mut() {
                    Some(watch) => match tokio::time::timeout(slice, watch.signals.recv()).await {
                        Ok(Some(NoticeEvent::Reconnected)) => {
                            receiver_ready = true;
                            read_now = true;
                        }
                        Ok(Some(NoticeEvent::Notice(notice))) => {
                            receiver_ready = true;
                            let scope = HostRequest {
                                request_id: request_id.to_owned(),
                                host_id: host_id.to_owned(),
                                device_id: device_id.to_owned(),
                                command_id: command_id.to_owned(),
                                ..Default::default()
                            };
                            read_now = watch.accept(&notice, &scope);
                        }
                        Ok(None) => break,
                        Err(_) => {
                            if recovery_attempt < 4 {
                                let backoff = Duration::from_millis(100 * (1 << recovery_attempt));
                                tokio::time::sleep(backoff.min(remaining - final_reserve)).await;
                                recovery_attempt = recovery_attempt.saturating_add(1);
                                read_now = true;
                            }
                        }
                    },
                    None => {
                        if recovery_attempt < 4 {
                            let backoff = Duration::from_millis(100 * (1 << recovery_attempt));
                            tokio::time::sleep(backoff.min(remaining - final_reserve)).await;
                            recovery_attempt = recovery_attempt.saturating_add(1);
                            read_now = true;
                        }
                    }
                }
            }
            if !read_now {
                continue;
            }
            let probe = self
                .get_command_result(host_id, device_id, channel, command_id)
                .await;
            match self.command_probe_outcome(request_id, command_id, probe) {
                Some(outcome) => return outcome,
                None => continue,
            }
        }

        // The one reserved final authoritative read.
        let probe = self
            .get_command_result(host_id, device_id, channel, command_id)
            .await;
        match self.command_probe_outcome(request_id, command_id, probe) {
            Some(outcome) => outcome,
            None => Err(CoreError::Timeout),
        }
    }

    /// Classify one authoritative `GetCommandResult` probe. `None` means the
    /// original command is still pending; `Some` is the final handle or error.
    fn command_probe_outcome(
        &self,
        request_id: &str,
        command_id: &str,
        probe: CoreResult<CommandResult>,
    ) -> Option<CoreResult<CommandHandle>> {
        match probe {
            Ok(result)
                if matches!(
                    DomainStatus::from_proto(result.status),
                    Some(DomainStatus::Received | DomainStatus::HostAccepted)
                ) =>
            {
                None
            }
            Ok(result) => Some(self.handle_from_command_result(request_id, command_id, &result)),
            Err(CoreError::UnknownResult) => None,
            Err(error)
                if matches!(
                    error,
                    CoreError::ChannelMismatch
                        | CoreError::FingerprintMismatch { .. }
                        | CoreError::Security(_)
                ) || error.device_authorization_lost() =>
            {
                Some(Err(error))
            }
            // A transient transport failure is not a rejection of the
            // already-admitted original control; bounded recovery keeps its
            // identity pending.
            Err(CoreError::Unavailable(_) | CoreError::Timeout) => None,
            // Any other authoritative query failure cannot produce a proven
            // outcome for this probe; report the original identity as unknown.
            Err(_) => Some(Err(CoreError::UnknownResult)),
        }
    }

    /// Recover a terminal command payload over the current channel after a
    /// reconnect. The host is the execution dedupe point, keyed by command id.
    pub async fn get_command_result(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        command_id: &str,
    ) -> CoreResult<CommandResult> {
        let response = self
            .query(
                host_id,
                device_id,
                channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::GetCommandResult(
                        GetCommandResult {
                            command_id: command_id.to_owned(),
                        },
                    )),
                },
            )
            .await?;
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::CommandResult(result)) => {
                if !result.found {
                    return Err(CoreError::UnknownResult);
                }
                Ok(result)
            }
            _ => Err(CoreError::UnknownResult),
        }
    }

    /// Decode the terminal payload carried by a `CommandResult`.
    ///
    /// A non-empty result payload must carry exactly one known
    /// `ResponsePayload` variant; an unknown oneof is a protocol violation. A
    /// bodyless result is legitimate for control commands without a typed
    /// response (queue edit, interrupt, approval decision), and typed lifecycle
    /// receipts are validated by the caller that knows the expected kind.
    pub fn decode_command_result(result: &CommandResult) -> CoreResult<Option<ResponsePayload>> {
        if result.result_payload.is_empty() {
            // Bodyless results are legitimate: queue edits, interrupt and
            // approval decisions succeed without a response variant. Typed
            // lifecycle receipts are validated by the caller that knows the
            // expected command kind.
            return Ok(None);
        }
        let payload = ResponsePayload::decode(result.result_payload.as_slice())
            .map_err(|_| CoreError::Internal("invalid command result payload".into()))?;
        if payload.error_code != 0 {
            return Err(response_error(&payload));
        }
        // A known status with no known payload variant means a newer peer sent
        // a variant this build cannot render; reject it instead of reporting an
        // empty success.
        whalerein_proto::required_oneof(payload.payload.as_ref(), "ResponsePayload")
            .map_err(crate::api::wire_error)?;
        Ok(Some(payload))
    }

    /// Build a handle from a recovered `GetCommandResult` response and update
    /// the projection for the original request.
    pub fn handle_from_command_result(
        &self,
        request_id: &str,
        command_id: &str,
        result: &CommandResult,
    ) -> CoreResult<CommandHandle> {
        let status = DomainStatus::from_proto(result.status).unwrap_or(DomainStatus::Unknown);
        let code = ErrorCode::try_from(result.error_code)
            .ok()
            .filter(|code| *code != ErrorCode::Unspecified);
        // A definitive failure settles even if its payload returns a domain error.
        // Successful outcomes stay recoverable until their result validates.
        if matches!(
            status,
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline
        ) {
            self.project_status(request_id, command_id, status, code)?;
        }
        let payload = Self::decode_command_result(result)?;
        let message_id = payload.as_ref().and_then(message_id_of);
        let delivery_id = payload.as_ref().and_then(delivery_id_of);
        let model_selection = payload.as_ref().and_then(model_selection_of);
        let lifecycle_receipt = payload
            .as_ref()
            .map(lifecycle_receipt_of)
            .transpose()?
            .flatten();
        let question_receipt = payload.as_ref().and_then(question_receipt_of);
        let (expected_kind, expected_parent) =
            self.cache.command_identity(request_id)?.unwrap_or_default();
        if status == DomainStatus::Succeeded
            && !expected_kind.is_empty()
            && !lifecycle_receipt_matches(
                lifecycle_receipt.as_ref(),
                &expected_kind,
                &expected_parent,
            )
        {
            return Err(CoreError::UnknownResult);
        }
        self.project_status(request_id, command_id, status, code)?;
        match status {
            DomainStatus::Succeeded | DomainStatus::HostAccepted => Ok(CommandHandle {
                request_id: request_id.to_owned(),
                command_id: command_id.to_owned(),
                status,
                message_id,
                delivery_id,
                model_selection,
                lifecycle_receipt,
                question_receipt,
            }),
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                Err(status_error(status, code))
            }
            DomainStatus::Unknown
                if lifecycle_receipt.as_ref().is_some_and(|receipt| {
                    matches!(receipt.disposition.as_str(), "FAILED" | "UNKNOWN")
                }) =>
            {
                Ok(CommandHandle {
                    request_id: request_id.to_owned(),
                    command_id: command_id.to_owned(),
                    status,
                    message_id,
                    delivery_id,
                    model_selection,
                    lifecycle_receipt,
                    question_receipt,
                })
            }
            DomainStatus::Unknown => Err(CoreError::UnknownResult),
            other => Err(CoreError::Internal(format!(
                "command ended in {}",
                other.as_str()
            ))),
        }
    }

    /// Update the command projection. Only `commands` writes this table.
    pub fn project_status(
        &self,
        request_id: &str,
        command_id: &str,
        status: DomainStatus,
        error_code: Option<ErrorCode>,
    ) -> CoreResult<()> {
        self.project(request_id, command_id, status, error_code)
    }

    fn project(
        &self,
        request_id: &str,
        command_id: &str,
        status: DomainStatus,
        error_code: Option<ErrorCode>,
    ) -> CoreResult<()> {
        let error_name = error_code.map(error_code_name);
        self.cache
            .put_command(request_id, command_id, status, error_name)?;
        Ok(())
    }

    /// Upload and finish the exact sealed body when the inline complete request
    /// would exceed the effective encoded request budget. The reference is
    /// submitted only after finish succeeds; small bodies stay inline.
    async fn ensure_representation(&self, request: &mut HostRequest) -> CoreResult<()> {
        if request.body.is_empty() || request.encoded_len() as u64 <= self.outbound_inline_bytes {
            return Ok(());
        }
        let body = std::mem::take(&mut request.body);
        let digest = sha256(&body).to_vec();
        if body.len() as u64 > self.outbound_envelope_bytes {
            return Err(CoreError::Api {
                code: Some(ErrorCode::PayloadTooLarge),
                message: "logical request exceeds transport budget".into(),
            });
        }
        let reference = self.upload_reference(request, &body, &digest).await?;
        request.payload_digest = digest;
        request.body_reference = Some(reference);
        // The reference request itself must fit before it is ever submitted.
        ensure_forwardable_with(request, self.outbound_envelope_bytes).map_err(|_| CoreError::Api {
            code: Some(ErrorCode::PayloadTooLarge),
            message: "reference request exceeds transport budget".into(),
        })
    }

    async fn upload_reference(
        &self,
        request: &HostRequest,
        body: &[u8],
        digest: &[u8],
    ) -> CoreResult<PayloadReference> {
        let reserve = RelayObjectReserveRequest {
            relay_version: crate::access::transport::RELAY_EPOCH_V5,
            host_id: request.host_id.clone(),
            device_id: request.device_id.clone(),
            session_id: self
                .access
                .cloud_session_id()
                .unwrap_or_default()
                .to_owned(),
            channel_id: request.channel_id.clone(),
            purpose: PayloadPurpose::Request as i32,
            part_id: String::new(),
            ciphertext_bytes: body.len() as u64,
            sha256: digest.to_vec(),
            expires_at_unix_ms: request.expires_at_unix_ms,
            signed_at_unix_ms: 0,
            signature: Vec::new(),
            request_id: request.request_id.clone(),
            enrollment_generation: request.enrollment_generation,
            connection_ref: request.connection_ref.clone(),
        };
        let reservation = self.access.reserve_relay_object(&reserve).await?;
        if let Err(error) = self.access.upload_relay_object(&reservation, body).await {
            let _ = self
                .access
                .delete_relay_object(&reservation.object_id)
                .await;
            return Err(error.into());
        }
        let finish = RelayObjectFinishRequest {
            relay_version: crate::access::transport::RELAY_EPOCH_V5,
            ciphertext_bytes: body.len() as u64,
            sha256: digest.to_vec(),
            signed_at_unix_ms: 0,
            signature: Vec::new(),
        };
        match self
            .access
            .finish_relay_object(&reservation.object_id, &finish)
            .await
        {
            Ok(object) => Ok(PayloadReference {
                object_id: object.object_id,
                owner_node_id: object.owner_node_id,
                ciphertext_bytes: object.ciphertext_bytes,
                sha256: object.sha256,
                expires_at_unix_ms: object.expires_at_unix_ms,
                purpose: object.purpose,
            }),
            Err(error) => {
                let _ = self
                    .access
                    .delete_relay_object(&reservation.object_id)
                    .await;
                Err(error.into())
            }
        }
    }

    /// Resolve a stored status copy to its complete verified result body so the
    /// rest of the command path applies identical AEAD and business checks for
    /// the inline and referenced representations. `max_ciphertext_bytes` is
    /// the result bound the request admitted to Access.
    async fn resolve_state(
        &self,
        state: RequestState,
        max_ciphertext_bytes: u64,
    ) -> CoreResult<RequestState> {
        let body = crate::access::transport::resolve_request_state_body(
            &self.access,
            &state,
            max_ciphertext_bytes,
        )
        .await
        .map_err(CoreError::from)?;
        let mut resolved = state;
        resolved.result_body = body.unwrap_or_default();
        resolved.result_reference = None;
        Ok(resolved)
    }

    /// The result bound this request admitted to Access. A referenced result
    /// that declares more is refused before it is downloaded.
    fn admitted_result_bound(&self, request: &HostRequest) -> u64 {
        request
            .transport
            .as_ref()
            .map_or(self.transport.envelope_bytes, |transport| {
                transport.result_max_bytes
            })
    }

    /// Register this exact command identity on the shared Host connection.
    ///
    /// The waiter is installed before submission, so a notice published while
    /// the request is still being admitted is still routed here. Dropping the
    /// guard removes only this waiter; a sibling command or watch is unaffected.
    fn attach_notices(&self, request: &HostRequest) -> Option<ForegroundNotices> {
        let connection = self.connection_for(&request.host_id).ok()?;
        if connection.device_id() != request.device_id {
            return None;
        }
        let (signals, guard, receiver_ready) =
            connection.register_notice(&request.request_id, &request.command_id);
        Some(ForegroundNotices {
            signals,
            _guard: guard,
            receiver_ready,
            last_seen: None,
        })
    }

    /// Register a notice waiter for an already-accepted command identity.
    fn attach_command_notices(
        &self,
        host_id: &str,
        device_id: &str,
        request_id: &str,
        command_id: &str,
    ) -> Option<ForegroundNotices> {
        let connection = self.connection_for(host_id).ok()?;
        if connection.device_id() != device_id {
            return None;
        }
        let (signals, guard, receiver_ready) = connection.register_notice(request_id, command_id);
        Some(ForegroundNotices {
            signals,
            _guard: guard,
            receiver_ready,
            last_seen: None,
        })
    }

    /// Wait for a proven state change. Notices wake the wait; each wake performs
    /// one authoritative read of the original request identity. On an attach or
    /// reconnect the original request is reconciled once. No status polling
    /// loop runs while the receiver stays connected: the one reserved final
    /// authoritative lookup covers a lost terminal notice.
    async fn wait_foreground(
        &self,
        mut notices: Option<&mut ForegroundNotices>,
        request: &HostRequest,
        allow_host_accepted: bool,
        expires_at_unix_ms: Option<i64>,
    ) -> CoreResult<RequestState> {
        let mut budget = self.access.request_timeout();
        if let Some(expires) = expires_at_unix_ms {
            let remaining = expires.saturating_sub(now_unix_ms()).max(0) as u64;
            budget = budget.min(Duration::from_millis(remaining));
        }
        let final_reserve = FINAL_LOOKUP_RESERVE.min(budget);
        let deadline = Instant::now() + budget;

        // Reconcile once on attach, before waiting on notifications. An
        // identity mismatch is a security failure, never another request.
        let mut state = self.access.read_request_authoritative(request).await?;
        self.access.validate_request_state(request, &state)?;
        if state_is_proven(&state, allow_host_accepted) {
            return Ok(state);
        }
        let mut recovery_attempt = 0u32;
        let mut receiver_ready = notices.as_ref().is_some_and(|watch| watch.receiver_ready);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline.saturating_duration_since(now);
            if remaining <= final_reserve {
                break;
            }
            let slice = (remaining - final_reserve).min(Duration::from_secs(2));
            let mut read_now = false;
            if receiver_ready {
                match notices.as_mut() {
                    Some(watch) => {
                        match tokio::time::timeout(slice, watch.signals.recv()).await {
                            Ok(Some(NoticeEvent::Notice(notice))) => {
                                read_now = watch.accept(&notice, request);
                            }
                            // A reconnect binds a fresh advertisement;
                            // reconcile the original identity once more.
                            Ok(Some(NoticeEvent::Reconnected)) => read_now = true,
                            Ok(None) => break,
                            // A connected receiver with no notice performs no
                            // status fetch; the final lookup covers a lost one.
                            Err(_) => {}
                        }
                    }
                    None => break,
                }
            } else {
                // Before the first successful subscription there is no
                // notification path. Bounded recovery uses widening backoff,
                // never a fixed poll loop.
                match notices.as_mut() {
                    Some(watch) => match tokio::time::timeout(slice, watch.signals.recv()).await {
                        Ok(Some(NoticeEvent::Reconnected)) => {
                            receiver_ready = true;
                            read_now = true;
                        }
                        Ok(Some(NoticeEvent::Notice(notice))) => {
                            receiver_ready = true;
                            read_now = watch.accept(&notice, request);
                        }
                        Ok(None) => break,
                        Err(_) => {
                            if recovery_attempt < 4 {
                                let backoff = Duration::from_millis(100 * (1 << recovery_attempt));
                                tokio::time::sleep(backoff.min(remaining - final_reserve)).await;
                                recovery_attempt = recovery_attempt.saturating_add(1);
                                read_now = true;
                            }
                        }
                    },
                    None => {
                        if recovery_attempt < 4 {
                            let backoff = Duration::from_millis(100 * (1 << recovery_attempt));
                            tokio::time::sleep(backoff.min(remaining - final_reserve)).await;
                            recovery_attempt = recovery_attempt.saturating_add(1);
                            read_now = true;
                        } else {
                            tokio::time::sleep(slice).await;
                        }
                    }
                }
            }
            if !read_now {
                continue;
            }
            match self.access.read_request_authoritative(request).await {
                Ok(next) => {
                    self.access.validate_request_state(request, &next)?;
                    state = next;
                    if state_is_proven(&state, allow_host_accepted) {
                        return Ok(state);
                    }
                }
                Err(error) if error.is_retryable_transport() => {}
                Err(error) => return Err(error.into()),
            }
        }

        // Spend exactly one final authoritative lookup before reporting an
        // unconfirmed wait, even while MQTT stayed connected.
        match self.access.read_request_authoritative(request).await {
            Ok(next) => {
                self.access.validate_request_state(request, &next)?;
                if state_is_proven(&next, allow_host_accepted) {
                    Ok(next)
                } else {
                    Err(CoreError::Timeout)
                }
            }
            Err(error) if error.is_retryable_transport() => Err(CoreError::Timeout),
            Err(error) => Err(error.into()),
        }
    }

    async fn submit(&self, request: &HostRequest) -> Result<RequestReceipt, SubmitFailure> {
        self.submit_observed(request, None).await
    }

    async fn submit_observed(
        &self,
        request: &HostRequest,
        observation: Option<&TransportObservation>,
    ) -> Result<RequestReceipt, SubmitFailure> {
        let mut last_error = None;
        let mut definitely_unsent = true;
        for attempt in 0..MAX_SUBMIT_ATTEMPTS {
            match self.access.submit_request(&request.host_id, request).await {
                Ok(receipt) => {
                    if !receipt_matches_request(&receipt, request) {
                        return Err(SubmitFailure {
                            error: CoreError::Security("request receipt identity mismatch".into()),
                            definitely_unsent: false,
                        });
                    }
                    if let Some(observation) = observation {
                        observation.recovered();
                    }
                    return Ok(receipt);
                }
                Err(error) if error.is_retryable_transport() => {
                    if let Some(observation) = observation {
                        observation.failed(&error);
                    }
                    definitely_unsent &= error.definitely_not_sent();
                    last_error = Some(error);
                    if attempt + 1 < MAX_SUBMIT_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
                    }
                }
                Err(error) if error.status == Some(409) => {
                    definitely_unsent = false;
                    // The previous attempt may have reached Access before the
                    // connection failed. Read the same request id back.
                    let result_limit = request
                        .transport
                        .as_ref()
                        .map_or(self.transport.envelope_bytes, |value| {
                            value.result_max_bytes
                        });
                    if let Ok(state) = self
                        .access
                        .get_request_with_limit(&request.request_id, result_limit)
                        .await
                    {
                        if let Err(error) = self.access.validate_request_state(request, &state) {
                            return Err(SubmitFailure {
                                error,
                                definitely_unsent: false,
                            });
                        }
                        if let Some(observation) = observation {
                            observation.recovered();
                        }
                        return Ok(RequestReceipt {
                            request_id: state.request_id,
                            status: state.status,
                            command_id: state.command_id,
                            expires_at_unix_ms: state.expires_at_unix_ms,
                        });
                    }
                    last_error = Some(error);
                    break;
                }
                Err(error) => {
                    return Err(SubmitFailure {
                        error: error.into(),
                        definitely_unsent: false,
                    });
                }
            }
        }
        Err(SubmitFailure {
            error: last_error
                .map(CoreError::from)
                .unwrap_or_else(|| CoreError::Unavailable("Access request failed".into())),
            definitely_unsent,
        })
    }
}

/// A query may be reissued after a connection replacement only when replaying
/// it is safe. Writes and answer submissions keep their single attempt.
fn safe_query_to_retry(payload: &RequestPayload) -> bool {
    use whalerein_proto::request_payload::Payload;
    matches!(
        payload.payload.as_ref(),
        Some(
            Payload::ListSessions(_)
                | Payload::GetSession(_)
                | Payload::GetApproval(_)
                | Payload::GetCommandResult(_)
                | Payload::Ping(_)
                | Payload::ListApprovals(_)
                | Payload::ReadHistory(_)
                | Payload::WatchSession(_)
                | Payload::CheckVersions(_)
                | Payload::UnwatchSession(_)
                | Payload::ReadMessageReply(_)
                | Payload::GetHostCapabilities(_)
                | Payload::ListModels(_)
                | Payload::ReadSessionModel(_)
                | Payload::WaitSession(_)
                | Payload::CancelSessionWait(_)
                | Payload::ReadWaitState(_)
                | Payload::ReadPendingQueue(_)
                | Payload::ListQuestions(_)
                | Payload::GetQuestion(_)
                | Payload::PullAttachment(_)
                | Payload::CloseAttachment(_)
        )
    )
}

#[allow(clippy::too_many_arguments)]
fn build_request(
    request_id: &str,
    host_id: &str,
    device_id: &str,
    kind: RequestKind,
    command_id: &str,
    payload_digest: Vec<u8>,
    expires_at_unix_ms: i64,
    channel_id: &str,
    body: Vec<u8>,
) -> HostRequest {
    HostRequest {
        request_id: request_id.to_owned(),
        host_id: host_id.to_owned(),
        device_id: device_id.to_owned(),
        kind: kind as i32,
        command_id: command_id.to_owned(),
        payload_digest,
        expires_at_unix_ms,
        relay_version: crate::access::transport::RELAY_EPOCH_V5,
        trace_id: random_id(16),
        protocol_version: PROTOCOL_VERSION,
        channel_id: channel_id.to_owned(),
        body,
        body_reference: None,
        transport: None,
        connection_ref: String::new(),
        grant_generation: 0,
        enrollment_generation: 0,
    }
}

// Access adds its own hop metadata before forwarding the complete authenticated
// envelope. Reject the actual encoded size before journaling or I/O.
fn ensure_forwardable_with(request: &HostRequest, limit: u64) -> CoreResult<()> {
    crate::access::transport::validate_request_representation(request).map_err(CoreError::from)?;
    if request.encoded_len() as u64 > limit {
        return Err(CoreError::Api {
            code: Some(ErrorCode::PayloadTooLarge),
            message: "logical request exceeds transport budget".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
fn ensure_forwardable(request: &HostRequest) -> CoreResult<()> {
    ensure_forwardable_with(
        request,
        crate::access::transport::TransportBudget::default().envelope_bytes,
    )
}

impl CommandService {
    fn ensure_forwardable(&self, request: &HostRequest) -> CoreResult<()> {
        ensure_forwardable_with(request, self.outbound_envelope_bytes)
    }
}

fn check_result_bound(state: &RequestState, result_max_bytes: u64) -> CoreResult<()> {
    if state.result_body.len() as u64 > result_max_bytes {
        return Err(CoreError::Api {
            code: Some(ErrorCode::PayloadTooLarge),
            message: "logical result exceeds transport budget".into(),
        });
    }
    Ok(())
}

fn decrypt_response(
    channel: &SecureChannel,
    aad: &RequestAad<'_>,
    state: &RequestState,
) -> CoreResult<ResponsePayload> {
    let payload = decrypt_response_raw(channel, aad, state)?;
    if payload.error_code != 0 {
        return Err(response_error(&payload));
    }
    Ok(payload)
}

fn decrypt_response_raw(
    channel: &SecureChannel,
    aad: &RequestAad<'_>,
    state: &RequestState,
) -> CoreResult<ResponsePayload> {
    if state.result_body.is_empty() {
        return Err(CoreError::UnknownResult);
    }
    let frame = decode_frame(&state.result_body)?;
    let plaintext = channel.open_request(aad, &frame)?;
    let payload = ResponsePayload::decode(plaintext.as_slice())
        .map_err(|_| CoreError::Internal("invalid response payload".into()))?;
    Ok(payload)
}

/// A successful `CommandResult` with no payload is a protocol violation, with
/// the shared actionable `invalid_argument` text.
fn response_error(payload: &ResponsePayload) -> CoreError {
    let code = ErrorCode::try_from(payload.error_code).ok();
    let mut message = payload.error_detail.replace(['\n', '\r'], " ");
    crate::util::truncate_utf8(&mut message, 200);
    CoreError::Host {
        code,
        message: if message.is_empty() {
            "host rejected the request".into()
        } else {
            message
        },
    }
}

fn source_response_error(payload: &ResponsePayload) -> CoreError {
    let CoreError::Host { code, message } = response_error(payload) else {
        unreachable!()
    };
    CoreError::Source { code, message }
}

fn delivery_id_of(payload: &ResponsePayload) -> Option<String> {
    match &payload.payload {
        Some(whalerein_proto::response_payload::Payload::CommandAccepted(accepted))
            if !accepted.delivery_id.is_empty() =>
        {
            Some(accepted.delivery_id.clone())
        }
        _ => None,
    }
}

fn lifecycle_receipt_matches(receipt: Option<&LifecycleReceipt>, kind: &str, parent: &str) -> bool {
    receipt.is_some_and(|receipt| {
        if receipt.operation != kind {
            return false;
        }
        if kind != "fork" {
            return true;
        }
        if parent.is_empty() {
            return false;
        }
        if receipt.parent_session_id.as_deref() == Some(parent) {
            return true;
        }
        // FAILED/UNKNOWN fork receipts have no claimed child identity. The
        // requested parent is carried only as the reconcile key.
        matches!(receipt.disposition.as_str(), "FAILED" | "UNKNOWN")
            && receipt.reconcile_key.as_deref() == Some(parent)
    })
}

fn lifecycle_receipt_of(payload: &ResponsePayload) -> CoreResult<Option<LifecycleReceipt>> {
    let Some(whalerein_proto::response_payload::Payload::LifecycleReceipt(receipt)) =
        &payload.payload
    else {
        return Ok(None);
    };
    let disposition = match whalerein_proto::LifecycleDisposition::try_from(receipt.disposition) {
        Ok(whalerein_proto::LifecycleDisposition::Ready) if receipt.warning_code.is_empty() => {
            "READY"
        }
        Ok(whalerein_proto::LifecycleDisposition::ReadyWithWarning)
            if receipt.warning_code == "workspace_attach_failed" =>
        {
            "READY_WITH_WARNING"
        }
        Ok(whalerein_proto::LifecycleDisposition::Failed) => "FAILED",
        Ok(whalerein_proto::LifecycleDisposition::Unknown) => "UNKNOWN",
        _ => return Err(CoreError::UnknownResult),
    };
    if !matches!(receipt.operation.as_str(), "create" | "fork") {
        return Err(CoreError::UnknownResult);
    }
    let has_identity = !receipt.session_id.trim().is_empty();
    match disposition {
        "READY" | "READY_WITH_WARNING" if !has_identity => return Err(CoreError::UnknownResult),
        "FAILED" | "UNKNOWN" if has_identity => return Err(CoreError::UnknownResult),
        _ => {}
    }
    if has_identity
        && (receipt.operation == "fork"
            && (receipt.parent_session_id.trim().is_empty()
                || receipt.parent_session_id == receipt.session_id))
    {
        return Err(CoreError::UnknownResult);
    }
    if has_identity && receipt.operation == "create" && !receipt.parent_session_id.is_empty() {
        return Err(CoreError::UnknownResult);
    }
    if disposition == "UNKNOWN"
        && !matches!(
            receipt.reconcile_mode.as_str(),
            "OPERATION_ID" | "PREFERRED_SESSION_ID" | "PARENT_SESSION_ID" | "MANUAL"
        )
    {
        return Err(CoreError::UnknownResult);
    }
    Ok(Some(LifecycleReceipt {
        session_id: receipt.session_id.clone(),
        parent_session_id: (!receipt.parent_session_id.is_empty())
            .then(|| receipt.parent_session_id.clone()),
        operation: receipt.operation.clone(),
        disposition: disposition.into(),
        warning_code: (!receipt.warning_code.is_empty()).then(|| receipt.warning_code.clone()),
        operation_id: (!receipt.operation_id.is_empty()).then(|| receipt.operation_id.clone()),
        phase: (!receipt.phase.is_empty()).then(|| receipt.phase.clone()),
        reason_code: (!receipt.reason_code.is_empty()).then(|| receipt.reason_code.clone()),
        retryable: receipt.retryable,
        reconcile_mode: (!receipt.reconcile_mode.is_empty())
            .then(|| receipt.reconcile_mode.clone()),
        reconcile_key: (!receipt.reconcile_key.is_empty()).then(|| receipt.reconcile_key.clone()),
        warning_detail: receipt
            .warning_detail
            .clone()
            .filter(|value| !value.is_empty()),
    }))
}

fn message_id_of(payload: &ResponsePayload) -> Option<String> {
    match &payload.payload {
        Some(whalerein_proto::response_payload::Payload::CommandAccepted(CommandAccepted {
            message_id,
            ..
        })) => Some(message_id.clone()),
        _ => None,
    }
}

fn question_receipt_of(payload: &ResponsePayload) -> Option<QuestionReceiptInfo> {
    let Some(whalerein_proto::response_payload::Payload::QuestionReceipt(receipt)) =
        &payload.payload
    else {
        return None;
    };
    let phase = whalerein_proto::QuestionPhase::try_from(receipt.phase).ok()?;
    if !matches!(
        phase,
        whalerein_proto::QuestionPhase::QuestionSettled
            | whalerein_proto::QuestionPhase::QuestionReplyQueued
    ) || receipt.batch_id.is_empty()
    {
        return None;
    }
    Some(QuestionReceiptInfo {
        batch_id: receipt.batch_id.clone(),
        phase: phase.as_str_name().to_owned(),
    })
}

fn error_code_of(state: &RequestState) -> Option<ErrorCode> {
    ErrorCode::try_from(state.error_code).ok()
}

/// A proven state is a terminal outcome or, for control commands, an explicit
/// host acceptance. `received`/`unknown` are never proof of execution.
fn state_is_proven(state: &RequestState, allow_host_accepted: bool) -> bool {
    let status = DomainStatus::from_proto(state.status).unwrap_or(DomainStatus::Unknown);
    status.is_terminal() || (allow_host_accepted && status == DomainStatus::HostAccepted)
}

fn status_error(status: DomainStatus, code: Option<ErrorCode>) -> CoreError {
    if let Some(code) = code {
        return CoreError::Host {
            code: Some(code),
            message: status.as_str().into(),
        };
    }
    match status {
        DomainStatus::HostOffline => CoreError::HostOffline,
        DomainStatus::Rejected => CoreError::HostRejected,
        DomainStatus::Unknown => CoreError::UnknownResult,
        DomainStatus::Failed => CoreError::Host {
            code: None,
            message: "command failed".into(),
        },
        _ => CoreError::Internal(format!("unexpected terminal status {}", status.as_str())),
    }
}

/// Encode an `EncryptedFrame` body, exposed for tests and tooling.
#[cfg(test)]
pub fn encode_encrypted_frame(frame: &whalerein_proto::EncryptedFrame) -> Vec<u8> {
    encode_frame(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecureChannel;
    use whalerein_proto::OnlineStatus;

    fn channel() -> SecureChannel {
        SecureChannel::from_handshake(&[5u8; 32], "p", "chan", "h-1", "d-1").unwrap()
    }

    fn response(payload: whalerein_proto::response_payload::Payload) -> ResponsePayload {
        ResponsePayload {
            payload: Some(payload),
            error_code: 0,
            error_detail: String::new(),
        }
    }

    fn state_with(
        channel: &SecureChannel,
        request: &HostRequest,
        payload: ResponsePayload,
        status: DomainStatus,
        request_id: &str,
        expires_at: i64,
        command_id: &str,
    ) -> RequestState {
        let aad = RequestAad {
            request_id,
            host_id: "h-1",
            device_id: "d-1",
            kind: RequestKind::try_from(request.kind).unwrap(),
            command_id,
            expires_at_unix_ms: expires_at,
        };
        let frame = channel
            .peer_view()
            .seal_request(&aad, &payload.encode_to_vec())
            .unwrap();
        RequestState {
            request_id: request_id.into(),
            host_id: "h-1".into(),
            device_id: "d-1".into(),
            status: match status {
                DomainStatus::Succeeded => OnlineStatus::Succeeded as i32,
                DomainStatus::HostAccepted => OnlineStatus::HostAccepted as i32,
                DomainStatus::Failed => OnlineStatus::Failed as i32,
                DomainStatus::Rejected => OnlineStatus::Rejected as i32,
                DomainStatus::HostOffline => OnlineStatus::HostOffline as i32,
                DomainStatus::Unknown => OnlineStatus::Unknown as i32,
                DomainStatus::Received => OnlineStatus::Received as i32,
            },
            command_id: command_id.into(),
            payload_digest: request.payload_digest.clone(),
            result_body: encode_frame(&frame),
            error_code: 0,
            error_message: String::new(),
            expires_at_unix_ms: expires_at,
            updated_at_unix_ms: 0,
            channel_id: channel.channel_id().into(),
            result_reference: None,
        }
    }

    #[test]
    fn encrypted_send_envelope_fits_its_actual_wire_budget_at_the_boundary() {
        use crate::access::transport::MESSAGE_BYTES;
        use whalerein_proto::{request_payload::Payload, DeliveryMode, SendMessage};

        let channel = channel();
        let aad = RequestAad {
            request_id: "r",
            host_id: "h-1",
            device_id: "d-1",
            kind: RequestKind::Command,
            command_id: "c",
            expires_at_unix_ms: 5,
        };
        let encoded_size = |text_len| {
            let payload = RequestPayload {
                payload: Some(Payload::SendMessage(SendMessage {
                    session_id: "s".into(),
                    text: "x".repeat(text_len),
                    delivery_mode: DeliveryMode::EnqueueAfterRun as i32,
                })),
            };
            let frame = channel
                .seal_request(&aad, &payload.encode_to_vec())
                .unwrap();
            let body = encode_frame(&frame);
            let request = build_request(
                "r",
                "h-1",
                "d-1",
                RequestKind::Command,
                "c",
                sha256(&body).to_vec(),
                5,
                channel.channel_id(),
                body,
            );
            request.encoded_len()
        };
        let sample = MESSAGE_BYTES - 4096;
        let overhead = encoded_size(sample) - sample;
        let text_at_limit = MESSAGE_BYTES - overhead;
        assert_eq!(encoded_size(text_at_limit - 1), MESSAGE_BYTES - 1);
        assert_eq!(encoded_size(text_at_limit), MESSAGE_BYTES);
        assert_eq!(encoded_size(text_at_limit + 1), MESSAGE_BYTES + 1);
    }

    #[test]
    fn lifecycle_receipts_preserve_identity_and_reject_ambiguous_data() {
        use whalerein_proto::{LifecycleDisposition, LifecycleReceipt as WireReceipt};
        let make = |session_id: &str, operation: &str, disposition: i32, warning_code: &str| {
            response(
                whalerein_proto::response_payload::Payload::LifecycleReceipt(WireReceipt {
                    parent_session_id: String::new(),
                    session_id: session_id.into(),
                    operation: operation.into(),
                    disposition,
                    warning_code: warning_code.into(),
                    operation_id: String::new(),
                    phase: String::new(),
                    reason_code: String::new(),
                    retryable: false,
                    reconcile_mode: String::new(),
                    reconcile_key: String::new(),
                    warning_detail: None,
                }),
            )
        };
        let ready = make("session", "create", LifecycleDisposition::Ready as i32, "");
        assert_eq!(
            lifecycle_receipt_of(&ready).unwrap().unwrap().session_id,
            "session"
        );
        let warning = make(
            "session",
            "create",
            LifecycleDisposition::ReadyWithWarning as i32,
            "workspace_attach_failed",
        );
        assert_eq!(
            lifecycle_receipt_of(&warning).unwrap().unwrap().disposition,
            "READY_WITH_WARNING"
        );
        for bad in [
            make("", "create", 1, ""),
            make("session", "fork", 1, ""),
            make("session", "create", 2, "private"),
            make("session", "create", 0, ""),
        ] {
            assert!(matches!(
                lifecycle_receipt_of(&bad),
                Err(CoreError::UnknownResult)
            ));
        }
        assert!(
            lifecycle_receipt_of(&response(whalerein_proto::response_payload::Payload::Pong(
                Default::default()
            )))
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn fork_receipt_requires_distinct_parent_and_child() {
        use whalerein_proto::{LifecycleDisposition, LifecycleReceipt as WireReceipt};
        let receipt = |parent: &str, child: &str| {
            response(
                whalerein_proto::response_payload::Payload::LifecycleReceipt(WireReceipt {
                    session_id: child.into(),
                    parent_session_id: parent.into(),
                    operation: "fork".into(),
                    disposition: LifecycleDisposition::Ready as i32,
                    warning_code: String::new(),
                    operation_id: String::new(),
                    phase: String::new(),
                    reason_code: String::new(),
                    retryable: false,
                    reconcile_mode: String::new(),
                    reconcile_key: String::new(),
                    warning_detail: None,
                }),
            )
        };
        let valid = lifecycle_receipt_of(&receipt("parent", "child"))
            .unwrap()
            .unwrap();
        assert_eq!(valid.parent_session_id.as_deref(), Some("parent"));
        assert!(matches!(
            lifecycle_receipt_of(&receipt("", "child")),
            Err(CoreError::UnknownResult)
        ));
        assert!(matches!(
            lifecycle_receipt_of(&receipt("same", "same")),
            Err(CoreError::UnknownResult)
        ));
    }

    #[test]
    fn session_create_uses_complete_encrypted_forwarded_envelope_budget() {
        use whalerein_proto::{request_payload::Payload, CreateSession};
        let channel = channel();
        let aad = RequestAad {
            request_id: "r",
            host_id: "h-1",
            device_id: "d-1",
            kind: RequestKind::Command,
            command_id: "c",
            expires_at_unix_ms: 5,
        };
        let request_for = |length| {
            let payload = RequestPayload {
                payload: Some(Payload::CreateSession(CreateSession {
                    workspace_id: Some("x".repeat(length)),
                    ..Default::default()
                })),
            };
            let frame = channel
                .seal_request(&aad, &payload.encode_to_vec())
                .unwrap();
            let body = encode_frame(&frame);
            build_request(
                "r",
                "h-1",
                "d-1",
                RequestKind::Command,
                "c",
                sha256(&body).to_vec(),
                5,
                channel.channel_id(),
                body,
            )
        };
        let target = crate::access::transport::MESSAGE_BYTES;
        let sample = target - 4096;
        let probe = request_for(sample);
        let at = target - (probe.encoded_len() - sample);
        for delta in [-1isize, 0, 1] {
            let request = request_for(at.checked_add_signed(delta).unwrap());
            assert_eq!(
                request.encoded_len(),
                target.checked_add_signed(delta).unwrap()
            );
            assert_eq!(ensure_forwardable(&request).is_ok(), delta <= 0);
        }
    }

    #[tokio::test]
    async fn oversized_session_create_is_rejected_before_journal_or_io() {
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        let fake = crate::api::test_support::FakeAccess::start().await;
        let access = Arc::new(AccessClient::new(fake.url().clone(), None).unwrap());
        let service = CommandService::new(access, cache.clone());
        let payload = RequestPayload {
            payload: Some(whalerein_proto::request_payload::Payload::CreateSession(
                whalerein_proto::CreateSession {
                    workspace_id: Some("x".repeat(crate::access::transport::MESSAGE_BYTES)),
                    ..Default::default()
                },
            )),
        };
        let error = service
            .control("h-1", "d-1", &channel(), payload)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CoreError::Api {
                code: Some(ErrorCode::PayloadTooLarge),
                ..
            }
        ));
        assert!(cache.pending_commands().unwrap().is_empty());
        assert!(fake.host.lock().unwrap().request_traces.is_empty());
        assert_eq!(fake.host.lock().unwrap().commands_seen, 0);
    }

    #[test]
    fn build_request_sets_protocol_and_channel() {
        let request = build_request(
            "r",
            "h",
            "d",
            RequestKind::Command,
            "c",
            vec![1, 2],
            5,
            "chan",
            vec![3],
        );
        assert_eq!(request.protocol_version, PROTOCOL_VERSION);
        assert_eq!(request.channel_id, "chan");
        assert_eq!(request.command_id, "c");
        assert_eq!(request.kind, RequestKind::Command as i32);
    }

    #[test]
    fn decrypts_response_and_surfaces_domain_errors() {
        let channel = channel();
        let request = build_request(
            "r",
            "h-1",
            "d-1",
            RequestKind::Query,
            "",
            vec![],
            10,
            channel.channel_id(),
            vec![],
        );
        let state = state_with(
            &channel,
            &request,
            response(whalerein_proto::response_payload::Payload::Pong(
                Default::default(),
            )),
            DomainStatus::Succeeded,
            "r",
            10,
            "",
        );
        let aad = RequestAad {
            request_id: "r",
            host_id: "h-1",
            device_id: "d-1",
            kind: RequestKind::Query,
            command_id: "",
            expires_at_unix_ms: 10,
        };
        assert!(decrypt_response(&channel, &aad, &state).is_ok());

        let mut error_payload = ResponsePayload {
            payload: None,
            error_code: ErrorCode::PayloadDigestMismatch as i32,
            error_detail: "bad digest".into(),
        };
        error_payload.error_detail.push('\n');
        let state = state_with(
            &channel,
            &request,
            error_payload,
            DomainStatus::Failed,
            "r",
            10,
            "",
        );
        assert!(matches!(
            decrypt_response(&channel, &aad, &state),
            Err(CoreError::Host {
                code: Some(ErrorCode::PayloadDigestMismatch),
                ..
            })
        ));
    }

    #[test]
    fn old_channel_is_reported_as_channel_mismatch() {
        let channel = channel();
        let new = SecureChannel::from_handshake(&[6u8; 32], "p", "chan-2", "h-1", "d-1").unwrap();
        let request = build_request(
            "r",
            "h-1",
            "d-1",
            RequestKind::Query,
            "",
            vec![],
            10,
            channel.channel_id(),
            vec![],
        );
        let state = state_with(
            &channel,
            &request,
            response(whalerein_proto::response_payload::Payload::Pong(
                Default::default(),
            )),
            DomainStatus::Succeeded,
            "r",
            10,
            "",
        );
        let aad = RequestAad {
            request_id: "r",
            host_id: "h-1",
            device_id: "d-1",
            kind: RequestKind::Query,
            command_id: "",
            expires_at_unix_ms: 10,
        };
        assert!(matches!(
            decrypt_response(&new, &aad, &state),
            Err(CoreError::ChannelMismatch)
        ));
    }

    #[test]
    fn maps_each_request_status_to_its_stable_error() {
        assert!(matches!(
            status_error(DomainStatus::HostOffline, None),
            CoreError::HostOffline
        ));
        assert!(matches!(
            status_error(DomainStatus::Rejected, None),
            CoreError::HostRejected
        ));
        assert!(matches!(
            status_error(DomainStatus::Unknown, None),
            CoreError::UnknownResult
        ));
        assert_eq!(
            status_error(DomainStatus::Failed, Some(ErrorCode::ApprovalExpired)).exit_code(),
            8
        );
        assert!(matches!(
            status_error(DomainStatus::Received, None),
            CoreError::Internal(_)
        ));
    }

    #[test]
    fn response_error_sanitizes_message() {
        let payload = ResponsePayload {
            payload: None,
            error_code: ErrorCode::Conflict as i32,
            error_detail: "a\nb".into(),
        };
        let error = response_error(&payload);
        assert_eq!(error.exit_code(), 8);
        assert!(!error.to_string().contains('\n'));
    }

    async fn control_with_result_fixture(
        mode: &'static str,
    ) -> (CoreResult<CommandHandle>, Arc<Cache>) {
        use crate::api::test_support::{MockResponse, MockServer};
        use std::sync::Mutex;
        let state = Arc::new(Mutex::new(None::<HostRequest>));
        let saved = state.clone();
        let channel = channel();
        let remote = channel.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            if mock.method == "POST" && mock.path.ends_with("/heartbeat") {
                return MockResponse::empty(204);
            }
            if mock.method == "POST" {
                let request: HostRequest = serde_json::from_slice(&mock.body).unwrap();
                let receipt = RequestReceipt {
                    request_id: if mode == "wrong-receipt-request" {
                        "wrong-request".into()
                    } else {
                        request.request_id.clone()
                    },
                    command_id: if mode == "wrong-receipt-command" {
                        "wrong-command".into()
                    } else {
                        request.command_id.clone()
                    },
                    status: match mode {
                        "lost-result" => OnlineStatus::Succeeded as i32,
                        "rejected-receipt"
                        | "wrong-receipt-request"
                        | "wrong-receipt-command"
                        | "receipt-negative-state-success" => OnlineStatus::Rejected as i32,
                        "unknown-receipt" => OnlineStatus::Unknown as i32,
                        "offline-receipt" => OnlineStatus::HostOffline as i32,
                        _ => OnlineStatus::Received as i32,
                    },
                    expires_at_unix_ms: request.expires_at_unix_ms,
                };
                *saved.lock().unwrap() = Some(request);
                if mode == "conflict-wrong-state-host" {
                    return MockResponse::empty(409);
                }
                return MockResponse::json(200, &receipt);
            }
            if mode == "lost-result" {
                return MockResponse::empty(503);
            }
            let request = saved.lock().unwrap().clone().unwrap();
            let payload = if matches!(mode, "domain-error" | "failed-domain") {
                ResponsePayload {
                    error_code: ErrorCode::Conflict as i32,
                    ..Default::default()
                }
            } else {
                response(whalerein_proto::response_payload::Payload::CommandAccepted(
                    CommandAccepted {
                        message_id: "fixture-message".into(),
                        ..Default::default()
                    },
                ))
            };
            let status = match mode {
                "accepted-empty" => DomainStatus::HostAccepted,
                "rejected-receipt" => DomainStatus::Rejected,
                "offline-receipt" => DomainStatus::HostOffline,
                "failed-domain"
                | "failed-empty"
                | "failed-frame"
                | "state-negative-authenticated-success"
                | "wrong-state-request"
                | "wrong-state-host"
                | "wrong-state-device"
                | "wrong-state-command" => DomainStatus::Failed,
                _ => DomainStatus::Succeeded,
            };
            let mut result = state_with(
                &remote,
                &request,
                payload,
                status,
                &request.request_id,
                request.expires_at_unix_ms,
                &request.command_id,
            );
            match mode {
                "wrong-state-request" => {
                    result.request_id = "wrong-request".into();
                    result.result_body.clear();
                }
                "wrong-state-host" | "conflict-wrong-state-host" => {
                    result.host_id = "wrong-host".into();
                    result.result_body.clear();
                }
                "wrong-state-device" => {
                    result.device_id = "wrong-device".into();
                    result.result_body.clear();
                }
                "wrong-state-command" => {
                    result.command_id = "wrong-command".into();
                    result.result_body.clear();
                }
                "accepted-empty" | "succeeded-empty" | "rejected-receipt" | "offline-receipt" => {
                    result.result_body.clear()
                }
                "failed-empty" => {
                    result.result_body.clear();
                    result.error_code = ErrorCode::Conflict as i32;
                }
                "malformed-frame" | "failed-frame" => result.result_body = vec![255],
                "old-channel" => {
                    let mut frame = decode_frame(&result.result_body).unwrap();
                    frame.channel_id = "another-channel".into();
                    result.result_body = encode_frame(&frame);
                }
                "tampered-frame" => {
                    let mut frame = decode_frame(&result.result_body).unwrap();
                    frame.ciphertext[0] ^= 1;
                    result.result_body = encode_frame(&frame);
                }
                "malformed-payload" => {
                    let aad = RequestAad {
                        request_id: &request.request_id,
                        host_id: &request.host_id,
                        device_id: &request.device_id,
                        kind: RequestKind::Command,
                        command_id: &request.command_id,
                        expires_at_unix_ms: request.expires_at_unix_ms,
                    };
                    result.result_body =
                        encode_frame(&remote.peer_view().seal_request(&aad, &[255]).unwrap());
                }
                _ => {}
            }
            MockResponse::json(200, &result)
        }))
        .await;
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        let service = CommandService::new(
            Arc::new(AccessClient::new(server.url(), None).unwrap()),
            cache.clone(),
        );
        let result = service
            .control(
                "h-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                        whalerein_proto::Interrupt {
                            session_id: "s".into(),
                        },
                    )),
                },
            )
            .await;
        (result, cache)
    }

    #[tokio::test]
    async fn nonempty_corrupt_query_failure_cannot_be_masked_by_cleartext_status() {
        use crate::api::test_support::{MockResponse, MockServer};
        use std::sync::Mutex;
        for nonempty in [true, false] {
            let saved = Arc::new(Mutex::new(None::<HostRequest>));
            let shared = saved.clone();
            let channel = channel();
            let remote = channel.clone();
            let server = MockServer::start(Arc::new(move |mock| {
                if mock.method == "POST" {
                    let request: HostRequest = serde_json::from_slice(&mock.body).unwrap();
                    let receipt = RequestReceipt {
                        request_id: request.request_id.clone(),
                        command_id: request.command_id.clone(),
                        status: OnlineStatus::Received as i32,
                        expires_at_unix_ms: request.expires_at_unix_ms,
                    };
                    *shared.lock().unwrap() = Some(request);
                    return MockResponse::json(200, &receipt);
                }
                let request = shared.lock().unwrap().clone().unwrap();
                let mut result = state_with(
                    &remote,
                    &request,
                    response(whalerein_proto::response_payload::Payload::Pong(
                        Default::default(),
                    )),
                    DomainStatus::Failed,
                    &request.request_id,
                    request.expires_at_unix_ms,
                    &request.command_id,
                );
                result.result_body = if nonempty { vec![255] } else { vec![] };
                result.error_code = ErrorCode::Unavailable as i32;
                MockResponse::json(200, &result)
            }))
            .await;
            let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
            let service = CommandService::new(
                Arc::new(AccessClient::new(server.url(), None).unwrap()),
                cache,
            );
            let payload = RequestPayload {
                payload: Some(whalerein_proto::request_payload::Payload::GetSession(
                    whalerein_proto::GetSession {
                        session_id: "fixture".into(),
                    },
                )),
            };
            let result = service.query("h-1", "d-1", &channel, payload).await;
            if nonempty {
                assert!(matches!(result, Err(CoreError::Security(_))));
            } else {
                assert!(matches!(
                    result,
                    Err(CoreError::Host {
                        code: Some(ErrorCode::Unavailable),
                        ..
                    })
                ));
            }
        }
    }

    #[tokio::test]
    async fn an_authenticated_initial_success_settles_with_the_exact_message_identity() {
        let (result, cache) = control_with_result_fixture("valid-success").await;
        let handle = result.unwrap();
        assert_eq!(handle.status, DomainStatus::Succeeded);
        assert_eq!(handle.message_id.as_deref(), Some("fixture-message"));
        assert!(cache.pending_commands().unwrap().is_empty());
        assert_eq!(
            cache.get_command(&handle.request_id).unwrap().unwrap().1,
            "succeeded"
        );
    }

    #[tokio::test]
    async fn authenticated_failures_settle_but_unproven_or_invalid_results_remain_pending() {
        for mode in ["failed-domain", "failed-empty", "failed-frame"] {
            let (result, cache) = control_with_result_fixture(mode).await;
            match mode {
                "failed-domain" => {
                    assert!(matches!(
                        result,
                        Err(CoreError::Host {
                            code: Some(ErrorCode::Conflict),
                            ..
                        })
                    ));
                    assert!(cache.pending_commands().unwrap().is_empty());
                }
                "failed-empty" => {
                    assert!(matches!(result, Err(CoreError::StaleCommand { .. })));
                    assert_eq!(cache.pending_commands().unwrap().len(), 1);
                }
                _ => {
                    assert!(matches!(result, Err(CoreError::Security(_))));
                    assert_eq!(cache.pending_commands().unwrap().len(), 1);
                }
            }
        }
    }

    #[tokio::test]
    async fn initial_control_rejects_mismatched_receipt_and_state_identity_without_settlement() {
        for mode in [
            "wrong-receipt-request",
            "wrong-receipt-command",
            "wrong-state-request",
            "wrong-state-host",
            "wrong-state-device",
            "wrong-state-command",
            "conflict-wrong-state-host",
        ] {
            let (result, cache) = control_with_result_fixture(mode).await;
            assert!(matches!(result, Err(CoreError::Security(_))), "{mode}");
            assert_eq!(cache.pending_commands().unwrap().len(), 1, "{mode}");
        }
    }

    #[tokio::test]
    async fn authenticated_success_is_not_discarded_by_negative_receipt_status() {
        let (result, cache) = control_with_result_fixture("receipt-negative-state-success").await;
        let handle = result.unwrap();
        assert_eq!(handle.status, DomainStatus::Succeeded);
        assert_eq!(handle.message_id.as_deref(), Some("fixture-message"));
        assert!(cache.pending_commands().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authenticated_success_with_negative_status_requires_host_recovery() {
        let (result, cache) =
            control_with_result_fixture("state-negative-authenticated-success").await;
        assert!(matches!(result, Err(CoreError::StaleCommand { .. })));
        assert_eq!(cache.pending_commands().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unauthenticated_negative_terminal_result_preserves_recovery_intent() {
        let (result, cache) = control_with_result_fixture("failed-frame").await;
        assert!(matches!(result, Err(CoreError::Security(_))));
        assert_eq!(cache.pending_commands().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_old_channel_control_result_preserves_recovery_identifiers() {
        let (result, cache) = control_with_result_fixture("old-channel").await;
        let pending = cache.pending_commands().unwrap();
        assert_eq!(pending.len(), 1);
        match result {
            Err(CoreError::StaleCommand {
                request_id,
                command_id,
            }) => {
                assert_eq!(request_id, pending[0].request_id);
                assert_eq!(command_id, pending[0].command_id);
            }
            _ => panic!("old channel must require result recovery"),
        }
    }

    #[tokio::test]
    async fn unverified_negative_receipts_preserve_intent_and_require_recovery() {
        for mode in ["rejected-receipt", "offline-receipt", "unknown-receipt"] {
            let (result, cache) = control_with_result_fixture(mode).await;
            if mode == "unknown-receipt" {
                assert!(matches!(result, Err(CoreError::UnknownResult)));
            } else {
                assert!(matches!(result, Err(CoreError::StaleCommand { .. })));
            }
            assert_eq!(cache.pending_commands().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn terminal_success_receipt_does_not_settle_before_its_result_is_available() {
        let (result, cache) = control_with_result_fixture("lost-result").await;
        assert!(result.is_err());
        assert_eq!(cache.pending_commands().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn malformed_tampered_or_domain_error_success_results_cannot_report_success_or_erase_intent(
    ) {
        for mode in [
            "malformed-frame",
            "tampered-frame",
            "malformed-payload",
            "domain-error",
            "succeeded-empty",
        ] {
            let (result, cache) = control_with_result_fixture(mode).await;
            assert!(result.is_err(), "an invalid success response must fail");
            assert_eq!(cache.pending_commands().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn host_acceptance_without_a_terminal_payload_remains_a_recoverable_handle() {
        let (result, cache) = control_with_result_fixture("accepted-empty").await;
        let handle = result.unwrap();
        assert_eq!(handle.status, DomainStatus::HostAccepted);
        assert!(handle.message_id.is_none());
        assert_eq!(cache.pending_commands().unwrap().len(), 1);
    }

    #[test]
    fn an_invalid_recovered_success_payload_keeps_the_command_available_for_recovery() {
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        cache
            .record_command_intent("r", "c", "h", "d", "http://127.0.0.1:1/", "")
            .unwrap();
        let access = Arc::new(
            AccessClient::new(url::Url::parse("http://127.0.0.1:1").unwrap(), None).unwrap(),
        );
        let service = CommandService::new(access, cache.clone());
        let before = cache.pending_commands().unwrap();
        let result = CommandResult {
            found: true,
            status: OnlineStatus::Succeeded as i32,
            error_code: 0,
            result_payload: vec![255],
        };
        assert!(service
            .handle_from_command_result("r", "c", &result)
            .is_err());
        assert_eq!(cache.pending_commands().unwrap(), before);
    }

    #[test]
    fn unknown_status_with_an_unknown_lifecycle_receipt_is_recoverable() {
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        let access = Arc::new(
            AccessClient::new(url::Url::parse("http://127.0.0.1:1").unwrap(), None).unwrap(),
        );
        let service = CommandService::new(access, cache);
        let payload = ResponsePayload {
            payload: Some(
                whalerein_proto::response_payload::Payload::LifecycleReceipt(
                    whalerein_proto::LifecycleReceipt {
                        session_id: String::new(),
                        parent_session_id: String::new(),
                        operation: "create".into(),
                        disposition: whalerein_proto::LifecycleDisposition::Unknown as i32,
                        warning_code: String::new(),
                        operation_id: "operation-1".into(),
                        phase: "NATIVE".into(),
                        reason_code: "NATIVE_OUTCOME_UNKNOWN".into(),
                        retryable: false,
                        reconcile_mode: "OPERATION_ID".into(),
                        reconcile_key: "operation-1".into(),
                        warning_detail: None,
                    },
                ),
            ),
            ..Default::default()
        };
        let result = CommandResult {
            found: true,
            status: OnlineStatus::Unknown as i32,
            error_code: 0,
            result_payload: payload.encode_to_vec(),
        };
        let handle = service
            .handle_from_command_result("r", "c", &result)
            .unwrap();
        assert_eq!(handle.status, DomainStatus::Unknown);
        let receipt = handle.lifecycle_receipt.unwrap();
        assert_eq!(receipt.disposition, "UNKNOWN");
        assert_eq!(receipt.operation_id.as_deref(), Some("operation-1"));
    }

    #[test]
    fn lifecycle_recovery_validates_kind_after_reopening_the_command_journal() {
        for (kind, wrong_kind) in [("create", "fork"), ("fork", "create")] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("commands.sqlite");
            let cache = Cache::open(&path, [1; 32]).unwrap();
            cache
                .record_typed_command_intent(CommandIntent {
                    request_id: "r",
                    command_id: "c",
                    host_id: "h",
                    device_id: "d",
                    access_url: "http://127.0.0.1:1/",
                    host_fingerprint: "fingerprint",
                    command_kind: kind,
                    command_parent_session_id: if kind == "fork" { "parent" } else { "" },
                })
                .unwrap();
            drop(cache);
            let cache = Arc::new(Cache::open(&path, [1; 32]).unwrap());
            let access = Arc::new(
                AccessClient::new(url::Url::parse("http://127.0.0.1:1").unwrap(), None).unwrap(),
            );
            let service = CommandService::new(access, cache.clone());
            let mut result = CommandResult {
                found: true,
                status: OnlineStatus::Succeeded as i32,
                // A valid but non-lifecycle variant: the recovered result
                // cannot prove the recorded command kind.
                result_payload: response(whalerein_proto::response_payload::Payload::Pong(
                    Default::default(),
                ))
                .encode_to_vec(),
                error_code: 0,
            };
            assert!(matches!(
                service.handle_from_command_result("r", "c", &result),
                Err(CoreError::UnknownResult)
            ));
            let encode = |operation: &str| {
                ResponsePayload {
                    payload: Some(
                        whalerein_proto::response_payload::Payload::LifecycleReceipt(
                            whalerein_proto::LifecycleReceipt {
                                session_id: "child".into(),
                                parent_session_id: if operation == "fork" {
                                    "parent".into()
                                } else {
                                    String::new()
                                },
                                operation: operation.into(),
                                disposition: whalerein_proto::LifecycleDisposition::Ready as i32,
                                warning_code: String::new(),
                                operation_id: String::new(),
                                phase: String::new(),
                                reason_code: String::new(),
                                retryable: false,
                                reconcile_mode: String::new(),
                                reconcile_key: String::new(),
                                warning_detail: None,
                            },
                        ),
                    ),
                    ..Default::default()
                }
                .encode_to_vec()
            };
            result.result_payload = encode(wrong_kind);
            assert!(matches!(
                service.handle_from_command_result("r", "c", &result),
                Err(CoreError::UnknownResult)
            ));
            assert_eq!(cache.pending_commands().unwrap().len(), 1);
            if kind == "fork" {
                let mut wrong_parent = ResponsePayload::decode(encode("fork").as_slice()).unwrap();
                if let Some(whalerein_proto::response_payload::Payload::LifecycleReceipt(
                    ref mut receipt,
                )) = wrong_parent.payload
                {
                    receipt.parent_session_id = "different-parent".into();
                }
                result.result_payload = wrong_parent.encode_to_vec();
                assert!(matches!(
                    service.handle_from_command_result("r", "c", &result),
                    Err(CoreError::UnknownResult)
                ));
                assert_eq!(cache.pending_commands().unwrap().len(), 1);
            }
            result.result_payload = encode(kind);
            let handle = service
                .handle_from_command_result("r", "c", &result)
                .unwrap();
            assert_eq!(handle.lifecycle_receipt.unwrap().operation, kind);
            assert!(cache.pending_commands().unwrap().is_empty());
        }
    }

    #[test]
    fn recovered_definitive_failure_settles_the_intent_before_returning_its_domain_error() {
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        cache
            .record_command_intent("r", "c", "h", "d", "http://127.0.0.1:1/", "")
            .unwrap();
        let access = Arc::new(
            AccessClient::new(url::Url::parse("http://127.0.0.1:1").unwrap(), None).unwrap(),
        );
        let service = CommandService::new(access, cache.clone());
        let result = CommandResult {
            found: true,
            status: OnlineStatus::Failed as i32,
            error_code: ErrorCode::Expired as i32,
            result_payload: ResponsePayload {
                error_code: ErrorCode::Expired as i32,
                ..Default::default()
            }
            .encode_to_vec(),
        };
        assert!(matches!(
            service.handle_from_command_result("r", "c", &result),
            Err(CoreError::Host {
                code: Some(ErrorCode::Expired),
                ..
            })
        ));
        assert!(cache.pending_commands().unwrap().is_empty());
        assert_eq!(cache.get_command("r").unwrap().unwrap().1, "failed");
    }

    #[test]
    fn unicode_host_error_details_are_bounded_without_panicking() {
        let error = response_error(&ResponsePayload {
            error_code: ErrorCode::Conflict as i32,
            error_detail: format!("x{}", "🦀".repeat(60)),
            ..Default::default()
        });
        match error {
            CoreError::Host { code, message } => {
                assert_eq!(code, Some(ErrorCode::Conflict));
                assert_eq!(message, format!("x{}", "🦀".repeat(49)));
            }
            _ => panic!("host error expected"),
        }
    }

    #[test]
    fn message_id_is_extracted() {
        let payload = response(whalerein_proto::response_payload::Payload::CommandAccepted(
            CommandAccepted {
                message_id: "m-1".into(),
                delivery_id: "d-1".into(),
            },
        ));
        assert_eq!(message_id_of(&payload).as_deref(), Some("m-1"));
        assert_eq!(delivery_id_of(&payload).as_deref(), Some("d-1"));
        assert!(
            message_id_of(&response(whalerein_proto::response_payload::Payload::Pong(
                Default::default()
            )))
            .is_none()
        );
    }

    #[test]
    fn decodes_command_result_payload_and_message_id() {
        let payload = response(whalerein_proto::response_payload::Payload::Pong(
            Default::default(),
        ));
        let result = CommandResult {
            found: true,
            status: OnlineStatus::Succeeded as i32,
            result_payload: payload.encode_to_vec(),
            error_code: 0,
        };
        assert!(CommandService::decode_command_result(&result)
            .unwrap()
            .is_some());
        let bad = CommandResult {
            found: true,
            status: OnlineStatus::Succeeded as i32,
            result_payload: b"garbage".to_vec(),
            ..Default::default()
        };
        assert!(CommandService::decode_command_result(&bad).is_err());
    }

    #[test]
    fn a_successful_command_result_requires_a_known_payload_variant() {
        // A bodyless success is valid for control commands that have no typed
        // response (queue edit, interrupt, approval decision).
        let success_empty = CommandResult {
            found: true,
            status: OnlineStatus::Succeeded as i32,
            result_payload: Vec::new(),
            error_code: 0,
        };
        assert!(CommandService::decode_command_result(&success_empty)
            .unwrap()
            .is_none());

        // A non-empty payload with no known oneof is a newer/unknown variant:
        // reject it instead of reporting an empty success. Field 99 is unknown
        // to this schema but is a valid protobuf field, so it decodes to a
        // payload with no known variant.
        let unknown_variant = CommandResult {
            found: true,
            status: OnlineStatus::Succeeded as i32,
            result_payload: vec![0x98, 0x06, 0x01],
            error_code: 0,
        };
        let error = CommandService::decode_command_result(&unknown_variant).unwrap_err();
        assert_eq!(error.code(), "invalid_argument");
        assert_eq!(error.exit_code(), 2);
        assert!(
            error.to_string().contains("protocol version"),
            "the error must name the version check: {error}"
        );

        // A genuinely bodyless negative status stays valid: the status itself
        // proves the failure and there is no typed result to decode.
        for status in [
            OnlineStatus::Failed,
            OnlineStatus::Rejected,
            OnlineStatus::HostOffline,
        ] {
            let failed_empty = CommandResult {
                found: true,
                status: status as i32,
                result_payload: Vec::new(),
                error_code: 0,
            };
            assert!(
                CommandService::decode_command_result(&failed_empty)
                    .unwrap()
                    .is_none(),
                "{status:?}"
            );
        }

        // An unknown oneof is skipped by prost and decodes to no variant; it
        // must fail rather than look like an empty success.
        let unknown_oneof = CommandResult {
            found: true,
            status: OnlineStatus::Succeeded as i32,
            // Field 9999, varint 1: a variant this build does not know.
            result_payload: vec![0xF8, 0xF1, 0x04, 0x01],
            error_code: 0,
        };
        let error = CommandService::decode_command_result(&unknown_oneof).unwrap_err();
        assert_eq!(error.code(), "invalid_argument");
        assert_eq!(error.exit_code(), 2);
        assert!(
            error.to_string().contains("ResponsePayload"),
            "the error must name the payload type: {error}"
        );
    }

    #[test]
    fn encodes_encrypted_frame_as_protobuf_bytes() {
        let frame = channel()
            .seal_request(
                &RequestAad {
                    request_id: "r",
                    host_id: "h",
                    device_id: "d",
                    kind: RequestKind::Query,
                    command_id: "",
                    expires_at_unix_ms: 1,
                },
                b"x",
            )
            .unwrap();
        assert_eq!(encode_encrypted_frame(&frame), frame.encode_to_vec());
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::api::test_support::FakeAccess;
    use crate::runtime::channel::ensure_channel;
    use crate::runtime::Runtime;
    use crate::security::StaticIdentity;

    #[tokio::test]
    async fn query_control_and_recovery_through_fake_host() {
        let access = FakeAccess::start().await;
        let access_client =
            Arc::new(AccessClient::new(access.url(), Some("device-token".into())).unwrap());
        let runtime = Runtime::new();
        let identity = StaticIdentity::generate();
        let channel = ensure_channel(
            &runtime,
            &access_client,
            &identity,
            "host-1",
            "d-1",
            false,
            None,
        )
        .await
        .unwrap();
        let cache = Arc::new(Cache::open_in_memory([7u8; 32]).unwrap());
        let service = CommandService::new(access_client, cache.clone());

        let response = service
            .query(
                "host-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::ListSessions(
                        whalerein_proto::ListSessions {},
                    )),
                },
            )
            .await
            .unwrap();
        match response.payload {
            Some(whalerein_proto::response_payload::Payload::SessionList(list)) => {
                assert_eq!(list.sessions[0].session_id, "session-1");
            }
            _ => panic!("unexpected response"),
        }

        let handle = service
            .control(
                "host-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                        whalerein_proto::SendMessage {
                            delivery_mode: whalerein_proto::DeliveryMode::StartIfIdle as i32,
                            session_id: "session-1".into(),
                            text: "hello".into(),
                        },
                    )),
                },
            )
            .await
            .unwrap();
        assert_eq!(handle.status, DomainStatus::HostAccepted);
        assert_eq!(handle.message_id.as_deref(), Some("message-1"));
        assert_eq!(cache.command_counts().unwrap(), (1, 0));

        let result = service
            .get_command_result("host-1", "d-1", &channel, &handle.command_id)
            .await
            .unwrap();
        assert!(result.found);
        let confirmed = service
            .await_command_result(
                "host-1",
                "d-1",
                &channel,
                &handle.request_id,
                &handle.command_id,
            )
            .await
            .unwrap();
        assert_eq!(confirmed.status, DomainStatus::Succeeded);
        assert_eq!(confirmed.message_id.as_deref(), Some("message-1"));
        assert_eq!(confirmed.request_id, handle.request_id);
        assert_eq!(confirmed.command_id, handle.command_id);
        assert_eq!(access.host.lock().unwrap().commands_seen, 1);
        assert_eq!(cache.command_counts().unwrap(), (0, 0));
    }

    #[tokio::test]
    async fn lost_submit_receipts_reuse_identity_and_preserve_an_uncertain_command_for_recovery() {
        use crate::api::test_support::{MockResponse, MockServer};
        use std::sync::Mutex;

        let persisted = Arc::new(Mutex::new(Vec::<HostRequest>::new()));
        let observed = persisted.clone();
        let server = MockServer::start(Arc::new(move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/hosts/h/requests");
            observed
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&request.body).unwrap());
            MockResponse::empty(0)
        }))
        .await;
        let access = Arc::new(AccessClient::new(server.url(), None).unwrap());
        let channel = SecureChannel::from_handshake(&[9; 32], "p", "chan", "h", "d").unwrap();
        let cache = Arc::new(Cache::open_in_memory([8; 32]).unwrap());
        let service = CommandService::new(access, cache.clone());
        assert!(service
            .control(
                "h",
                "d",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                        whalerein_proto::Interrupt {
                            session_id: "s".into()
                        }
                    )),
                }
            )
            .await
            .is_err());
        let requests = persisted.lock().unwrap();
        assert_eq!(requests.len(), MAX_SUBMIT_ATTEMPTS as usize);
        assert!(!requests[0].command_id.is_empty());
        for request in requests.iter() {
            // The wire field is the encoded HTTPS result bound, not the raw
            // logical envelope. Access converts it back to a raw allowance.
            assert_eq!(
                request.transport.as_ref().unwrap().result_max_bytes,
                crate::access::http::https_body_limit(
                    crate::access::transport::TransportBudget::default().envelope_bytes
                ) as u64
            );
            assert_eq!(request.transport.as_ref().unwrap().grant_generation, 0);
            assert!(
                request.request_id == requests[0].request_id,
                "retry changed request identity"
            );
            assert!(
                request.command_id == requests[0].command_id,
                "retry changed command identity"
            );
            assert!(
                request.payload_digest == requests[0].payload_digest,
                "retry changed digest"
            );
            assert!(
                request.body == requests[0].body,
                "retry changed encrypted body"
            );
        }
        // Access has persisted the request; absence of a receipt cannot prove it was unsent.
        assert_eq!(
            cache.command_counts().unwrap(),
            (0, 1),
            "uncertain command must remain recoverable, not disappear"
        );
    }

    #[tokio::test]
    async fn cancellation_after_remote_receipt_keeps_routing_metadata_across_cache_reopen() {
        use crate::api::test_support::{MockResponse, MockServer};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = Arc::new(Cache::open(&path, [8; 32]).unwrap());
        let received = Arc::new(tokio::sync::Notify::new());
        let ready = received.clone();
        let journal = cache.clone();
        let server = MockServer::start(Arc::new(move |_request| {
            // The durable identity must exist before remote acceptance is possible.
            assert_eq!(journal.pending_commands().unwrap().len(), 1);
            ready.notify_one();
            MockResponse::empty(0)
        }))
        .await;
        let service = CommandService::new(
            Arc::new(AccessClient::new(server.url(), None).unwrap()),
            cache.clone(),
        );
        let channel = SecureChannel::from_handshake(&[9; 32], "p", "chan", "h", "d").unwrap();
        let task = tokio::spawn(async move {
            service
                .control(
                    "h",
                    "d",
                    &channel,
                    RequestPayload {
                        payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                            whalerein_proto::Interrupt {
                                session_id: "s".into(),
                            },
                        )),
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let before = cache.pending_commands().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].host_id, "h");
        assert_eq!(before[0].device_id, "d");
        assert_eq!(before[0].status, DomainStatus::Unknown);
        drop(server);
        drop(cache);
        assert_eq!(
            Cache::open(&path, [8; 32])
                .unwrap()
                .pending_commands()
                .unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn connection_refusals_after_a_lost_receipt_do_not_erase_uncertain_intent() {
        use crate::api::test_support::{MockResponse, MockServer};
        let cache = Arc::new(Cache::open_in_memory([8; 32]).unwrap());
        let received = Arc::new(tokio::sync::Notify::new());
        let ready = received.clone();
        let server = MockServer::start(Arc::new(move |_request| {
            ready.notify_one();
            MockResponse::empty(0)
        }))
        .await;
        let service = CommandService::new(
            Arc::new(AccessClient::new(server.url(), None).unwrap()),
            cache.clone(),
        );
        let channel = SecureChannel::from_handshake(&[9; 32], "p", "chan", "h", "d").unwrap();
        let task = tokio::spawn(async move {
            service
                .control(
                    "h",
                    "d",
                    &channel,
                    RequestPayload {
                        payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                            whalerein_proto::Interrupt {
                                session_id: "s".into(),
                            },
                        )),
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), received.notified())
            .await
            .unwrap();
        drop(server);
        assert!(task.await.unwrap().is_err());
        assert_eq!(cache.command_counts().unwrap(), (0, 1));
    }

    #[tokio::test]
    async fn failed_submit_never_writes_an_unsent_command() {
        let access = Arc::new(
            AccessClient::new(url::Url::parse("http://127.0.0.1:1").unwrap(), None).unwrap(),
        );
        let channel = SecureChannel::from_handshake(&[9u8; 32], "p", "chan", "h", "d").unwrap();
        let cache = Arc::new(Cache::open_in_memory([8u8; 32]).unwrap());
        let service = CommandService::new(access, cache.clone());
        let error = service
            .control(
                "h",
                "d",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::Interrupt(
                        whalerein_proto::Interrupt {
                            session_id: "s".into(),
                        },
                    )),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Unavailable(_)));
        assert_eq!(cache.command_counts().unwrap(), (0, 0));
    }
    #[tokio::test]
    async fn oversized_question_answer_is_rejected_before_journal_or_io() {
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        let fake = crate::api::test_support::FakeAccess::start().await;
        let access = Arc::new(AccessClient::new(fake.url().clone(), None).unwrap());
        let service = CommandService::new(access, cache.clone());
        let payload = RequestPayload {
            payload: Some(whalerein_proto::request_payload::Payload::AnswerQuestion(
                whalerein_proto::AnswerQuestion {
                    session_id: "s".into(),
                    batch_id: "b".into(),
                    answers: vec![whalerein_proto::QuestionAnswerItem {
                        id: "q".into(),
                        custom: Some("x".repeat(crate::access::transport::MESSAGE_BYTES)),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )),
        };
        let test_channel =
            SecureChannel::from_handshake(&[9u8; 32], "p", "chan", "h-1", "d-1").unwrap();
        let error = service
            .control("h-1", "d-1", &test_channel, payload)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CoreError::Api {
                code: Some(ErrorCode::PayloadTooLarge),
                ..
            }
        ));
        assert!(cache.pending_commands().unwrap().is_empty());
        assert!(fake.host.lock().unwrap().request_traces.is_empty());
        assert_eq!(fake.host.lock().unwrap().commands_seen, 0);
    }
    #[tokio::test]
    async fn accepted_command_result_query_rejection_remains_unknown() {
        use crate::api::test_support::{MockResponse, MockServer};

        let server = MockServer::start(Arc::new(|request| {
            if request.method == "POST" && request.path.ends_with("/heartbeat") {
                return MockResponse::empty(204);
            }
            let query: HostRequest = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(query.kind, RequestKind::Query as i32);
            MockResponse::json(
                202,
                &RequestReceipt {
                    request_id: query.request_id,
                    command_id: String::new(),
                    status: whalerein_proto::OnlineStatus::Rejected as i32,
                    expires_at_unix_ms: query.expires_at_unix_ms,
                },
            )
        }))
        .await;
        let cache = Arc::new(Cache::open_in_memory([4; 32]).unwrap());
        cache
            .record_command_intent(
                "original-request",
                "original-command",
                "h-1",
                "d-1",
                server.url().as_str(),
                "",
            )
            .unwrap();
        let service = CommandService::new(
            Arc::new(AccessClient::new(server.url(), None).unwrap()),
            cache.clone(),
        );
        let result = service
            .await_command_result(
                "h-1",
                "d-1",
                &SecureChannel::from_handshake(&[5u8; 32], "p", "chan", "h-1", "d-1").unwrap(),
                "original-request",
                "original-command",
            )
            .await;
        assert_eq!(result.unwrap_err().code(), "unknown");
        let pending = cache.pending_commands().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, "original-request");
        assert_eq!(pending[0].command_id, "original-command");
    }
}

fn model_selection_of(payload: &ResponsePayload) -> Option<crate::api::ModelSelectionReceipt> {
    match &payload.payload {
        Some(whalerein_proto::response_payload::Payload::ModelSelectionReceipt(receipt)) => {
            Some(receipt.clone().into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod directional_budget_tests {
    use super::*;

    #[test]
    fn outbound_request_is_bounded_by_the_lower_directional_envelope() {
        let request = build_request(
            "request",
            "host",
            "device",
            RequestKind::Query,
            "",
            Vec::new(),
            now_unix_ms() + 1_000,
            "channel",
            vec![7; 2048],
        );
        let encoded = request.encoded_len() as u64;
        assert!(ensure_forwardable_with(&request, encoded + 16).is_ok());
        assert!(ensure_forwardable_with(&request, encoded).is_ok());
        let error = ensure_forwardable_with(&request, encoded - 1).unwrap_err();
        assert_eq!(error.code(), "payload_too_large");
        assert!(matches!(
            error,
            CoreError::Api {
                code: Some(ErrorCode::PayloadTooLarge),
                ..
            }
        ));
    }

    #[test]
    fn a_larger_local_override_is_not_clamped_before_the_peer_minimum() {
        use crate::access::transport::{TransportProfile, TransportSettings};

        let roomy = TransportBudget::resolve(&TransportSettings {
            profile: TransportProfile::Roomy,
            overrides: None,
        })
        .unwrap();
        let access = Arc::new(
            AccessClient::new(
                url::Url::parse("http://127.0.0.1:1").unwrap(),
                Some("token".into()),
            )
            .unwrap(),
        );
        let cache = Arc::new(Cache::open_in_memory([1u8; 32]).unwrap());
        let service =
            CommandService::new(access.clone(), cache.clone()).with_transport_budget(roomy);
        assert_eq!(service.outbound_envelope_bytes, roomy.envelope_bytes);
        let smaller = service
            .with_outbound_envelope(8 * 1024 * 1024)
            .outbound_envelope_bytes;
        assert_eq!(smaller, 8 * 1024 * 1024);

        // The peer minimum also applies to a re-created service.
        let service = CommandService::new(access, cache)
            .with_transport_budget(roomy)
            .with_outbound_envelope(roomy.envelope_bytes + 1);
        assert_eq!(service.outbound_envelope_bytes, roomy.envelope_bytes);
    }

    #[test]
    fn stored_result_body_is_checked_before_decryption() {
        let state = RequestState {
            result_body: vec![0; 3],
            ..Default::default()
        };
        assert!(check_result_bound(&state, 3).is_ok());
        let error = check_result_bound(&state, 2).unwrap_err();
        assert_eq!(error.code(), "payload_too_large");
    }
}

#[cfg(test)]
mod relay_command_tests {
    use super::*;
    use crate::api::test_support::{MockResponse, MockServer};
    use std::sync::{Arc, Mutex};
    use whalerein_proto::{
        RelayObject, RelayObjectReservation, RequestKind, RequestReceipt, RequestState,
    };

    fn service(server: &MockServer, cache: Arc<Cache>) -> CommandService {
        CommandService::new(
            Arc::new(AccessClient::new(server.url(), Some("device-token".into())).unwrap()),
            cache,
        )
    }

    fn request(body: Vec<u8>, kind: RequestKind) -> HostRequest {
        let digest = (!body.is_empty()).then(|| sha256(&body).to_vec());
        build_request(
            "r-1",
            "h-1",
            "d-1",
            kind,
            if kind == RequestKind::Command {
                "c-1"
            } else {
                ""
            },
            digest.unwrap_or_default(),
            now_unix_ms() + 30_000,
            "chan",
            body,
        )
    }

    async fn start_relay_server(
        order: Arc<Mutex<Vec<String>>>,
        state: Arc<Mutex<Option<HostRequest>>>,
    ) -> MockServer {
        let object_id = "f".repeat(32);
        MockServer::start(Arc::new(move |mock| {
            order
                .lock()
                .unwrap()
                .push(format!("{} {}", mock.method, mock.path));
            match (mock.method.as_str(), mock.path.as_str()) {
                ("POST", "/v1/relay/objects") => MockResponse::json(
                    200,
                    &RelayObjectReservation {
                        object_id: object_id.clone(),
                        owner_node_id: "node-1".into(),
                        upload_path: format!("/v1/relay/objects/{object_id}"),
                        max_bytes: 1024 * 1024,
                        expires_at_unix_ms: 9,
                        upload_timeout_ms: 5_000,
                    },
                ),
                ("PUT", _) => MockResponse::empty(204),
                ("POST", path) if path.ends_with("/finish") => {
                    let finish: RelayObjectFinishRequest =
                        serde_json::from_slice(&mock.body).unwrap();
                    MockResponse::json(
                        200,
                        &RelayObject {
                            object_id: object_id.clone(),
                            owner_node_id: "node-1".into(),
                            purpose: PayloadPurpose::Request as i32,
                            ciphertext_bytes: finish.ciphertext_bytes,
                            sha256: finish.sha256,
                            expires_at_unix_ms: 9,
                        },
                    )
                }
                ("POST", "/v1/hosts/h-1/requests") => {
                    let submitted: HostRequest = serde_json::from_slice(&mock.body).unwrap();
                    *state.lock().unwrap() = Some(submitted.clone());
                    MockResponse::json(
                        200,
                        &RequestReceipt {
                            request_id: submitted.request_id,
                            status: whalerein_proto::OnlineStatus::HostOffline as i32,
                            command_id: submitted.command_id,
                            expires_at_unix_ms: submitted.expires_at_unix_ms,
                        },
                    )
                }
                ("POST", path) if path.ends_with("/heartbeat") => MockResponse::empty(204),
                ("GET", path) if path.starts_with("/v1/requests/") => {
                    let saved = state.lock().unwrap().clone().expect("submitted first");
                    MockResponse::json(
                        200,
                        &RequestState {
                            request_id: saved.request_id,
                            host_id: saved.host_id,
                            device_id: saved.device_id,
                            status: whalerein_proto::OnlineStatus::HostOffline as i32,
                            command_id: saved.command_id,
                            ..Default::default()
                        },
                    )
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await
    }

    #[tokio::test]
    async fn inline_body_below_and_at_the_budget_stays_inline_and_above_becomes_a_reference() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let submitted = Arc::new(Mutex::new(None));
        let server = start_relay_server(order.clone(), submitted).await;
        let cache = Arc::new(Cache::open_in_memory([1; 32]).unwrap());
        let service = service(&server, cache);
        let budget = service.transport;
        let limit = service.outbound_inline_bytes;

        let encoded_at =
            |len: usize| request(vec![0; len], RequestKind::Command).encoded_len() as u64;
        // The length prefix is not linear across a varint boundary, so find the
        // exact inclusive boundary with a small bounded adjustment.
        let mut at = limit
            .checked_sub(encoded_at(64) - 64)
            .expect("inline budget covers framing") as usize;
        while encoded_at(at) > limit {
            at -= 1;
        }
        while encoded_at(at + 1) <= limit {
            at += 1;
        }
        assert_eq!(encoded_at(at), limit);
        assert_eq!(encoded_at(at + 1), limit + 1);

        for (len, referenced) in [(at - 1, false), (at, false), (at + 1, true)] {
            order.lock().unwrap().clear();
            let mut request = request(vec![7; len], RequestKind::Command);
            service.ensure_representation(&mut request).await.unwrap();
            if referenced {
                let reference = request.body_reference.as_ref().expect("reference");
                assert!(request.body.is_empty());
                assert_eq!(reference.ciphertext_bytes, len as u64);
                assert_eq!(request.payload_digest, reference.sha256);
                assert_eq!(reference.purpose, PayloadPurpose::Request as i32);
                assert_eq!(reference.object_id.len(), 32);
                assert_eq!(
                    order.lock().unwrap().as_slice(),
                    [
                        "POST /v1/relay/objects".to_owned(),
                        format!("PUT /v1/relay/objects/{}", "f".repeat(32)),
                        format!("POST /v1/relay/objects/{}/finish", "f".repeat(32)),
                    ]
                );
            } else {
                assert!(!request.body.is_empty());
                assert!(request.body_reference.is_none());
                assert!(order.lock().unwrap().is_empty());
            }
            assert_eq!(budget.packet_bytes, 64 * 1024);
        }
    }

    #[tokio::test]
    async fn a_reference_is_finished_before_the_request_is_submitted() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let submitted = Arc::new(Mutex::new(None));
        let server = start_relay_server(order.clone(), submitted.clone()).await;
        let cache = Arc::new(Cache::open_in_memory([2; 32]).unwrap());
        let service = service(&server, cache).with_transport_budget(TransportBudget {
            packet_bytes: 4096,
            packet_overhead_bytes: 512,
            envelope_bytes: 16 * 1024 * 1024,
            ..TransportBudget::default()
        });
        let channel = SecureChannel::from_handshake(&[3u8; 32], "p", "chan", "h-1", "d-1").unwrap();
        let result = service
            .control(
                "h-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                        whalerein_proto::SendMessage {
                            session_id: "s".into(),
                            text: "x".repeat(8 * 1024),
                            delivery_mode: whalerein_proto::DeliveryMode::StartIfIdle as i32,
                        },
                    )),
                },
            )
            .await;
        // The terminal copy is bodyless, so the command path reports that the
        // original identity needs recovery; the ordering facts below are what
        // this case verifies.
        assert!(result.is_err(), "{result:?}");
        let recorded = order.lock().unwrap().clone();
        let submit_at = recorded
            .iter()
            .position(|call| call.contains("/requests"))
            .expect("submitted");
        let finish_at = recorded
            .iter()
            .position(|call| call.ends_with("/finish"))
            .expect("finished");
        assert!(
            finish_at < submit_at,
            "a reference is never submitted before finish: {recorded:?}"
        );
        assert!(recorded.iter().any(|call| call.starts_with("PUT ")));
        let submitted = submitted.lock().unwrap().clone().expect("request body");
        assert!(submitted.body.is_empty());
        let reference = submitted.body_reference.expect("body reference");
        assert_eq!(submitted.payload_digest, reference.sha256);
    }

    #[tokio::test]
    async fn a_small_body_is_submitted_inline_without_touching_the_directory() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let submitted = Arc::new(Mutex::new(None));
        let server = start_relay_server(order.clone(), submitted.clone()).await;
        let cache = Arc::new(Cache::open_in_memory([4; 32]).unwrap());
        let service = service(&server, cache);
        let channel = SecureChannel::from_handshake(&[5u8; 32], "p", "chan", "h-1", "d-1").unwrap();
        let result = service
            .control(
                "h-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::SendMessage(
                        whalerein_proto::SendMessage {
                            session_id: "s".into(),
                            text: "small".into(),
                            delivery_mode: whalerein_proto::DeliveryMode::StartIfIdle as i32,
                        },
                    )),
                },
            )
            .await;
        // The terminal copy is bodyless, so the command path reports that the
        // original identity needs recovery; the ordering facts below are what
        // this case verifies.
        assert!(result.is_err(), "{result:?}");
        assert!(!order
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.contains("/relay/")));
        let submitted = submitted.lock().unwrap().clone().expect("request body");
        assert!(!submitted.body.is_empty());
        assert!(submitted.body_reference.is_none());
    }

    #[tokio::test]
    async fn wait_reserves_exactly_one_final_authoritative_lookup_on_timeout() {
        let reads = Arc::new(Mutex::new(0u32));
        let served = reads.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            if mock.method == "GET" {
                *served.lock().unwrap() += 1;
                return MockResponse::json(
                    200,
                    &RequestState {
                        request_id: "r-1".into(),
                        host_id: "h-1".into(),
                        device_id: "d-1".into(),
                        status: whalerein_proto::OnlineStatus::Received as i32,
                        ..Default::default()
                    },
                );
            }
            MockResponse::empty(204)
        }))
        .await;
        let mut access = AccessClient::new(server.url(), None).unwrap();
        access.set_poll_timing(Duration::from_millis(150), Duration::from_millis(1));
        let cache = Arc::new(Cache::open_in_memory([6; 32]).unwrap());
        let service = CommandService::new(Arc::new(access), cache);
        let request = request(vec![1], RequestKind::Query);
        let error = service
            .wait_foreground(None, &request, false, None)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Timeout));
        assert_eq!(
            *reads.lock().unwrap(),
            2,
            "one reconcile read plus exactly one reserved final lookup"
        );
    }

    #[tokio::test]
    async fn reconnect_reconciles_the_original_request_identity() {
        let reads = Arc::new(Mutex::new(0u32));
        let served = reads.clone();
        let channel_id = Arc::new(Mutex::new(String::new()));
        let observed_channel = channel_id.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            if mock.method != "GET" {
                return MockResponse::empty(204);
            }
            let mut count = served.lock().unwrap();
            *count += 1;
            let status = if *count == 1 {
                whalerein_proto::OnlineStatus::Received
            } else {
                whalerein_proto::OnlineStatus::Succeeded
            };
            MockResponse::json(
                200,
                &RequestState {
                    request_id: "r-1".into(),
                    host_id: "h-1".into(),
                    device_id: "d-1".into(),
                    status: status as i32,
                    channel_id: observed_channel.lock().unwrap().clone(),
                    ..Default::default()
                },
            )
        }))
        .await;
        let mut access = AccessClient::new(server.url(), None).unwrap();
        access.set_poll_timing(Duration::from_secs(5), Duration::from_millis(1));
        let cache = Arc::new(Cache::open_in_memory([7; 32]).unwrap());
        let service = CommandService::new(Arc::new(access), cache);
        let request = request(vec![1], RequestKind::Query);
        *channel_id.lock().unwrap() = "chan".into();

        let (tx, rx) = mpsc::channel(4);
        tx.send(NoticeEvent::Reconnected).await.unwrap();
        let mut notices = ForegroundNotices {
            signals: rx,
            _guard: crate::connection::test_notice_guard(),
            receiver_ready: false,
            last_seen: None,
        };
        let state = service
            .wait_foreground(Some(&mut notices), &request, false, None)
            .await
            .unwrap();
        assert_eq!(
            state.status,
            whalerein_proto::OnlineStatus::Succeeded as i32
        );
        assert_eq!(*reads.lock().unwrap(), 2);
    }

    fn notice(request_id: &str, updated: i64) -> RequestStateNotice {
        RequestStateNotice {
            request_id: request_id.into(),
            host_id: "h-1".into(),
            device_id: "d-1".into(),
            status: whalerein_proto::OnlineStatus::Succeeded as i32,
            command_id: "c-1".into(),
            updated_at_unix_ms: updated,
            result_reference: None,
            relay_version: crate::access::transport::RELAY_EPOCH_V5,
        }
    }

    #[tokio::test]
    async fn notice_routing_ignores_foreign_stale_and_duplicate_notices() {
        let (_tx, rx) = mpsc::channel(1);
        let mut notices = ForegroundNotices {
            signals: rx,
            _guard: crate::connection::test_notice_guard(),
            receiver_ready: false,
            last_seen: None,
        };
        let request = request(vec![1], RequestKind::Command);
        assert!(!notices.accept(&notice("foreign", 2), &request));
        assert!(notices.accept(&notice("r-1", 2), &request));
        assert!(!notices.accept(&notice("r-1", 2), &request), "duplicate");
        assert!(!notices.accept(&notice("r-1", 1), &request), "stale");
        assert!(notices.accept(&notice("r-1", 3), &request), "newer");
    }

    #[tokio::test]
    async fn an_exact_heartbeat_stops_on_a_terminal_response() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = calls.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            observed
                .lock()
                .unwrap()
                .push(format!("{} {}", mock.method, mock.path));
            MockResponse::json(
                404,
                &whalerein_proto::ErrorResponse {
                    code: ErrorCode::NotFound as i32,
                    message: "request not found".into(),
                    ..Default::default()
                },
            )
        }))
        .await;
        let access = Arc::new(AccessClient::new(server.url(), None).unwrap());
        let heartbeat = CommandHeartbeat::start(access, "r-1".into());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["POST /v1/requests/r-1/heartbeat".to_owned()],
            "only the exact request is renewed and a terminal response stops it"
        );
        assert_eq!(heartbeat.outcome(), Some(HeartbeatOutcome::Closed));
        drop(heartbeat);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "no detached heartbeat survives termination"
        );
    }

    #[tokio::test]
    async fn dropping_the_guard_stops_renewal_without_a_detached_heartbeat() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = calls.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            observed
                .lock()
                .unwrap()
                .push(format!("{} {}", mock.method, mock.path));
            MockResponse::empty(204)
        }))
        .await;
        let access = Arc::new(AccessClient::new(server.url(), None).unwrap());
        let heartbeat = CommandHeartbeat::start(access, "r-1".into());
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(heartbeat);
        let settled = calls.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert_eq!(calls.lock().unwrap().len(), settled);
    }

    #[tokio::test]
    async fn shared_connection_notices_attach_by_exact_device_and_request() {
        use crate::api::test_support::FakeAccess;

        let access = FakeAccess::start().await;
        let settings = crate::access::MqttSettings::parse(&access.mqtt_url()).unwrap();
        let connection = crate::connection::test_connection(settings, "h-1", "d-1", "token");
        connection
            .wait_ready(&crate::api::Cancellation::new())
            .await
            .unwrap();
        let cache = Arc::new(Cache::open_in_memory([8; 32]).unwrap());
        let service = CommandService::new(
            Arc::new(AccessClient::new(access.url(), None).unwrap()),
            cache,
        )
        .with_host_connection(connection);
        // A connection for another Device never accepts this invocation.
        let other = request(vec![2], RequestKind::Command);
        let mut other = other;
        other.device_id = "d-2".into();
        assert!(service.attach_notices(&other).is_none());
        let notices = service
            .attach_notices(&request(vec![1], RequestKind::Command))
            .expect("scoped waiter attached to the shared connection");
        assert!(
            notices.receiver_ready,
            "the shared connection is subscribed"
        );
        drop(notices);
    }

    /// Every response must be bounded. The reference carrier exists to deliver
    /// content above the inline bound, but a reference may not declare more than
    /// the result bound the request admitted to Access: the client refuses it
    /// with `payload_too_large` before fetching, instead of sizing the directory
    /// read from the peer-declared `ciphertext_bytes` and buffering the object
    /// first. A Host, or anything able to publish an envelope under a valid
    /// Device token, can no longer make the client buffer an arbitrary amount of
    /// memory for one result or event.
    #[tokio::test]
    async fn a_referenced_result_above_the_admitted_bound_is_not_downloaded() {
        use whalerein_proto::Ping;

        let object_id = "e".repeat(32);
        let oversized = vec![7u8; 2 * 1024 * 1024];
        let served = Arc::new(Mutex::new(0usize));
        let submitted: Arc<Mutex<Option<HostRequest>>> = Arc::new(Mutex::new(None));
        let reference = PayloadReference {
            object_id: object_id.clone(),
            owner_node_id: "node-1".into(),
            ciphertext_bytes: oversized.len() as u64,
            sha256: sha256(&oversized).to_vec(),
            expires_at_unix_ms: 9,
            purpose: PayloadPurpose::Result as i32,
        };
        let handler_reference = reference.clone();
        let handler_body = oversized.clone();
        let handler_served = served.clone();
        let handler_submitted = submitted.clone();
        let server = MockServer::start(Arc::new(move |mock| {
            match (mock.method.as_str(), mock.path.as_str()) {
                ("POST", "/v1/hosts/h-1/requests") => {
                    let request: HostRequest = serde_json::from_slice(&mock.body).unwrap();
                    let receipt = RequestReceipt {
                        request_id: request.request_id.clone(),
                        status: whalerein_proto::OnlineStatus::Received as i32,
                        command_id: request.command_id.clone(),
                        expires_at_unix_ms: request.expires_at_unix_ms,
                    };
                    *handler_submitted.lock().unwrap() = Some(request);
                    MockResponse::json(200, &receipt)
                }
                ("GET", path) if path.starts_with("/v1/requests/") => {
                    let request = handler_submitted
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("submitted first");
                    MockResponse::json(
                        200,
                        &RequestState {
                            request_id: request.request_id.clone(),
                            host_id: request.host_id.clone(),
                            device_id: request.device_id.clone(),
                            status: whalerein_proto::OnlineStatus::Succeeded as i32,
                            command_id: request.command_id.clone(),
                            result_reference: Some(handler_reference.clone()),
                            ..Default::default()
                        },
                    )
                }
                ("GET", path) if path.starts_with("/v1/relay/objects/") => {
                    *handler_served.lock().unwrap() += handler_body.len();
                    MockResponse {
                        status: 200,
                        body: handler_body.clone(),
                        delay_ms: 0,
                    }
                }
                _ => MockResponse::empty(404),
            }
        }))
        .await;
        let service = service(&server, Arc::new(Cache::open_in_memory([8; 32]).unwrap()))
            .with_transport_budget(TransportBudget {
                packet_bytes: 4096,
                packet_overhead_bytes: 512,
                envelope_bytes: 16 * 1024,
                reserved_bytes: 16 * 1024,
                bulk_concurrency: 1,
                transfer_timeout_ms: 2_000,
                profile: crate::access::transport::TransportProfile::Default,
            });
        let channel = SecureChannel::from_handshake(&[9u8; 32], "p", "chan", "h-1", "d-1").unwrap();
        let error = service
            .query(
                "h-1",
                "d-1",
                &channel,
                RequestPayload {
                    payload: Some(whalerein_proto::request_payload::Payload::Ping(Ping {
                        client_time_unix_ms: 1,
                    })),
                },
            )
            .await
            .expect_err("a result above the admitted bound must be refused");
        let bound = submitted
            .lock()
            .unwrap()
            .clone()
            .expect("submitted request")
            .transport
            .expect("transport metadata")
            .result_max_bytes;
        assert!(
            reference.ciphertext_bytes > bound,
            "the fixture reference must exceed the admitted result bound"
        );
        let downloaded = *served.lock().unwrap() as u64;
        assert!(
            downloaded <= bound,
            "the client downloaded {downloaded} bytes for one result although it admitted at most {bound} \
             (error was {}, so the whole object was fetched before any bound applied)",
            error.code()
        );
        assert_eq!(
            error.code(),
            "payload_too_large",
            "the client must refuse a referenced result above its admitted bound"
        );
    }
}
