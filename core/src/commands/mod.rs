//! Command submission, result polling and the command projection.
//!
//! Only this module writes the command journal. Intent is durable before I/O;
//! only proven-unsent failures remove it. Network retries reuse the same
//! request identity and encrypted payload. Cancellation keeps recovery metadata.

use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use whalerein_proto::{
    CommandAccepted, CommandResult, ErrorCode, GetCommandResult, HostRequest, RequestKind,
    RequestPayload, RequestReceipt, RequestState, ResponsePayload,
};

use crate::access::AccessClient;
use crate::api::{
    error_code_name, CommandHandle, CoreError, CoreResult, OnlineStatus as DomainStatus,
};
use crate::cache::Cache;
use crate::pairing::PROTOCOL_VERSION;
use crate::security::{decode_frame, encode_frame, RequestAad, SecureChannel};
use crate::util::{now_unix_ms, random_id, sha256};

pub const REQUEST_TTL_MS: i64 = 30_000;
pub const MAX_SUBMIT_ATTEMPTS: u32 = 3;

struct SubmitFailure {
    error: CoreError,
    definitely_unsent: bool,
}

pub struct CommandService {
    access: Arc<AccessClient>,
    cache: Arc<Cache>,
    host_fingerprint: Option<String>,
}

impl CommandService {
    pub fn new(access: Arc<AccessClient>, cache: Arc<Cache>) -> Self {
        Self {
            access,
            cache,
            host_fingerprint: None,
        }
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
        self.query_inner(host_id, device_id, channel, payload).await
    }

    async fn query_inner(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        payload: RequestPayload,
    ) -> CoreResult<ResponsePayload> {
        let request_id = random_id(16);
        let expires_at = now_unix_ms() + REQUEST_TTL_MS;
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
        let receipt = self
            .submit(&request)
            .await
            .map_err(|failure| failure.error)?;
        if let Some(status) = DomainStatus::from_proto(receipt.status) {
            if status.is_terminal() && status != DomainStatus::Succeeded {
                return Err(status_error(status, None));
            }
        }
        let state = self.access.wait_for_request(&request_id, false).await?;
        let status = DomainStatus::from_proto(state.status).unwrap_or(DomainStatus::Unknown);
        let result = decrypt_response(channel, &aad, &state);
        match status {
            DomainStatus::Succeeded => result,
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                // A failed result may still carry an encrypted domain error.
                match result {
                    Ok(payload) => Err(response_error(&payload)),
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
        let command_id = random_id(16);
        let request_id = random_id(16);
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
        self.cache.record_command_intent(
            &request_id,
            &command_id,
            host_id,
            device_id,
            self.access.base_url().as_str(),
            self.host_fingerprint.as_deref().unwrap_or(""),
        )?;
        let receipt = match self.submit(&request).await {
            Ok(receipt) => receipt,
            Err(failure) => {
                if failure.definitely_unsent {
                    self.cache.remove_unsent_command(&request_id)?;
                }
                return Err(failure.error);
            }
        };
        let receipt_status =
            DomainStatus::from_proto(receipt.status).unwrap_or(DomainStatus::Unknown);
        // A receipt can report an already completed retry, but cannot replace
        // authenticating and decoding that command's result.
        if receipt_status != DomainStatus::Succeeded {
            self.project(&request_id, &command_id, receipt_status, None)?;
        }
        if receipt_status.is_terminal() && receipt_status != DomainStatus::Succeeded {
            return Err(status_error(receipt_status, None));
        }

        let state = self.access.wait_for_request(&request_id, true).await?;
        let status = DomainStatus::from_proto(state.status).unwrap_or(DomainStatus::Unknown);
        if status != DomainStatus::Succeeded {
            self.project(&request_id, &command_id, status, error_code_of(&state))?;
        }

        let decrypted = decrypt_response(channel, &aad, &state);
        match status {
            DomainStatus::HostAccepted | DomainStatus::Succeeded => {
                let message_id = match decrypted {
                    Ok(payload) => message_id_of(&payload),
                    Err(CoreError::ChannelMismatch) => {
                        return Err(CoreError::StaleCommand {
                            request_id: request_id.clone(),
                            command_id: command_id.clone(),
                        })
                    }
                    Err(CoreError::UnknownResult)
                        if status == DomainStatus::HostAccepted && state.result_body.is_empty() =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };
                if status == DomainStatus::Succeeded {
                    self.project(&request_id, &command_id, status, error_code_of(&state))?;
                }
                Ok(CommandHandle {
                    request_id,
                    command_id,
                    status,
                    message_id,
                })
            }
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                match decrypted {
                    Ok(payload) => Err(response_error(&payload)),
                    Err(CoreError::ChannelMismatch) => Err(CoreError::StaleCommand {
                        request_id: request_id.clone(),
                        command_id: command_id.clone(),
                    }),
                    Err(CoreError::UnknownResult) if state.result_body.is_empty() => {
                        Err(status_error(status, error_code_of(&state)))
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
    /// command, using the encrypted `GetCommandResult` query.
    pub async fn await_command_result(
        &self,
        host_id: &str,
        device_id: &str,
        channel: &SecureChannel,
        request_id: &str,
        command_id: &str,
    ) -> CoreResult<CommandHandle> {
        for attempt in 0..50u32 {
            match self
                .get_command_result(host_id, device_id, channel, command_id)
                .await
            {
                Ok(result)
                    if matches!(
                        DomainStatus::from_proto(result.status),
                        Some(DomainStatus::Received | DomainStatus::HostAccepted)
                    ) =>
                {
                    if attempt == 49 {
                        return Err(CoreError::Timeout);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok(result) => {
                    return self.handle_from_command_result(request_id, command_id, &result)
                }
                Err(CoreError::UnknownResult) if attempt < 49 => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(CoreError::Timeout)
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
    pub fn decode_command_result(result: &CommandResult) -> CoreResult<Option<ResponsePayload>> {
        if result.result_payload.is_empty() {
            return Ok(None);
        }
        let payload = ResponsePayload::decode(result.result_payload.as_slice())
            .map_err(|_| CoreError::Internal("invalid command result payload".into()))?;
        if payload.error_code != 0 {
            return Err(response_error(&payload));
        }
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
        self.project_status(request_id, command_id, status, code)?;
        match status {
            DomainStatus::Succeeded | DomainStatus::HostAccepted => Ok(CommandHandle {
                request_id: request_id.to_owned(),
                command_id: command_id.to_owned(),
                status,
                message_id,
            }),
            DomainStatus::Failed | DomainStatus::Rejected | DomainStatus::HostOffline => {
                Err(status_error(status, code))
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

    async fn submit(&self, request: &HostRequest) -> Result<RequestReceipt, SubmitFailure> {
        let mut last_error = None;
        let mut definitely_unsent = true;
        for attempt in 0..MAX_SUBMIT_ATTEMPTS {
            match self.access.submit_request(&request.host_id, request).await {
                Ok(receipt) => return Ok(receipt),
                Err(error) if error.is_retryable_transport() => {
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
                    if let Ok(state) = self.access.get_request(&request.request_id).await {
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
                    })
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
        client_transport_version: 2,
        relay_transport_version: 0,
        trace_id: random_id(16),
        protocol_version: PROTOCOL_VERSION,
        channel_id: channel_id.to_owned(),
        body,
    }
}

fn decrypt_response(
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
    if payload.error_code != 0 {
        return Err(response_error(&payload));
    }
    Ok(payload)
}

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

fn message_id_of(payload: &ResponsePayload) -> Option<String> {
    match &payload.payload {
        Some(whalerein_proto::response_payload::Payload::CommandAccepted(CommandAccepted {
            message_id,
        })) => Some(message_id.clone()),
        _ => None,
    }
}

fn error_code_of(state: &RequestState) -> Option<ErrorCode> {
    ErrorCode::try_from(state.error_code).ok()
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
        }
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
            if mock.method == "POST" {
                let request: HostRequest = serde_json::from_slice(&mock.body).unwrap();
                let receipt = RequestReceipt {
                    request_id: request.request_id.clone(),
                    command_id: request.command_id.clone(),
                    status: match mode {
                        "lost-result" => OnlineStatus::Succeeded as i32,
                        "rejected-receipt" => OnlineStatus::Rejected as i32,
                        "unknown-receipt" => OnlineStatus::Unknown as i32,
                        "offline-receipt" => OnlineStatus::HostOffline as i32,
                        _ => OnlineStatus::Received as i32,
                    },
                    expires_at_unix_ms: request.expires_at_unix_ms,
                };
                *saved.lock().unwrap() = Some(request);
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
                    },
                ))
            };
            let status = match mode {
                "accepted-empty" => DomainStatus::HostAccepted,
                "failed-domain" | "failed-empty" | "failed-frame" => DomainStatus::Failed,
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
                "accepted-empty" | "succeeded-empty" => result.result_body.clear(),
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
    async fn definitive_failures_keep_their_domain_or_security_error_without_becoming_success() {
        for mode in ["failed-domain", "failed-empty", "failed-frame"] {
            let (result, cache) = control_with_result_fixture(mode).await;
            if mode == "failed-frame" {
                assert!(matches!(result, Err(CoreError::Security(_))));
            } else {
                assert!(matches!(
                    result,
                    Err(CoreError::Host {
                        code: Some(ErrorCode::Conflict),
                        ..
                    })
                ));
            }
            assert!(cache.pending_commands().unwrap().is_empty());
            assert_eq!(cache.command_counts().unwrap(), (0, 0));
        }
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
    async fn receipt_rejection_and_offline_are_terminal_while_uncertainty_stays_pending() {
        for mode in ["rejected-receipt", "offline-receipt", "unknown-receipt"] {
            let (result, cache) = control_with_result_fixture(mode).await;
            match mode {
                "rejected-receipt" => assert!(matches!(result, Err(CoreError::HostRejected))),
                "offline-receipt" => assert!(matches!(result, Err(CoreError::HostOffline))),
                _ => assert!(matches!(result, Err(CoreError::UnknownResult))),
            }
            assert_eq!(
                cache.pending_commands().unwrap().len(),
                usize::from(mode == "unknown-receipt")
            );
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
            },
        ));
        assert_eq!(message_id_of(&payload).as_deref(), Some("m-1"));
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
        let empty = CommandResult {
            found: true,
            ..Default::default()
        };
        assert!(CommandService::decode_command_result(&empty)
            .unwrap()
            .is_none());
        let bad = CommandResult {
            found: true,
            result_payload: b"garbage".to_vec(),
            ..Default::default()
        };
        assert!(CommandService::decode_command_result(&bad).is_err());
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
}
