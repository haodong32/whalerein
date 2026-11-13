//! Test-only fake Access HTTP server and fake DSH host used by Core tests.
//!
//! This is not compiled into the library. It exercises the real HTTP client,
//! Noise handshake, request polling and command paths without a network peer.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::pairing::{default_capabilities, PROTOCOL_VERSION};
use crate::security::{
    channel_prologue, pairing_prologue, NoiseSession, SecureChannel, StaticIdentity,
};
use crate::util::{sha256, sha256_hex};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use whalerein_proto::{
    ChannelHello, ChannelReady, CommandResult, ErrorCode, ErrorResponse, HostInfo, HostList,
    HostRequest, OnlineStatus, PairingClaimRequest, PairingClaimResponse, PairingFinish,
    PairingHello, PairingHelloReply, PairingPhase, RequestKind, RequestReceipt, RequestState,
    ResponsePayload, SessionList, SessionSummary,
};

pub struct MockRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct MockResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub delay_ms: u64,
}

impl MockResponse {
    pub fn json<T: serde::Serialize>(status: u16, value: &T) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap(),
            delay_ms: 0,
        }
    }

    pub fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
            delay_ms: 0,
        }
    }
}

type Handler = Arc<dyn Fn(MockRequest) -> MockResponse + Send + Sync>;

pub struct MockServer {
    url: url::Url,
    task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    pub async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    if let Some(request) = read_request(&mut stream).await {
                        let response = handler(request);
                        if response.delay_ms > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(response.delay_ms))
                                .await;
                        }
                        // Status zero simulates a persisted request whose HTTP receipt is lost.
                        if response.status == 0 {
                            return;
                        }
                        let reason = if response.status < 300 { "OK" } else { "Error" };
                        let header = format!(
                            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            response.status,
                            reason,
                            response.body.len()
                        );
                        let _ = stream.write_all(header.as_bytes()).await;
                        let _ = stream.write_all(&response.body).await;
                        let _ = stream.shutdown().await;
                    }
                });
            }
        });
        Self {
            url: url::Url::parse(&format!("http://{address}")).unwrap(),
            task,
        }
    }

    pub fn url(&self) -> url::Url {
        self.url.clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<MockRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = find_header_end(&buffer) {
            break position;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Some(MockRequest {
        method,
        path,
        headers,
        body: buffer[body_start..(body_start + content_length).min(buffer.len())].to_vec(),
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

pub struct FakeHost {
    pub identity: StaticIdentity,
    pub host_id: String,
    pub token: String,
    pub host_online: bool,
    pub hide_primary_from_hosts: bool,
    pub extra_host: bool,
    pub extra_host_online: bool,
    pub extra_host_session: String,
    pub revoked: bool,
    pub fail_revoke_before: bool,
    pub drop_revoke_receipt: bool,
    pub drop_pair_finish_once: bool,
    pub pair_finish_calls: usize,
    pub pair_hello_calls: usize,
    pub pair_hello_delay_ms: u64,
    pub hello_terminal_response: Option<(ErrorCode, u16)>,
    pub unconfirmed_pair_finish_status: Option<u16>,
    pub committed_pair_finish_status: Option<u16>,
    pub fence_pair_finish_once: bool,
    pub fence_pair_finish_status: u16,
    pub saved_pair_finish: Option<(Vec<u8>, Vec<u8>)>,
    pub fail_hosts: bool,
    pub hosts_delay_ms: u64,
    pub fail_pending_queue: bool,
    pub query_error: Option<ErrorCode>,
    pub legacy_capabilities_error: Option<ErrorCode>,
    pub wait_timeouts: usize,
    pub wait_queries: usize,
    pub wait_last_timeout_ms: u32,
    pub wait_bad_id: bool,
    pub wait_drop_channel_once: bool,
    pub wait_replaced_once: bool,
    pub wait_extra_replacements: usize,
    pub wait_source_unauthorized_once: bool,
    pub wait_corrupt_source_error_once: bool,
    pub wait_conflicting_source_error_once: bool,
    pub wait_forged_success_once: bool,
    pub wait_foreign_frame_once: bool,
    corrupt_source_error: bool,
    conflicting_source_error: bool,
    forged_source_success: bool,
    foreign_frame_error: bool,
    pub wait_replace_at: usize,
    pub fail_handshake_polls_after_replacement: bool,
    pub fail_handshake_polls: bool,
    handshake_request_ids: HashSet<String>,
    pending_source_error: Option<ErrorCode>,
    pub wait_ids: Vec<String>,
    pub wait_cancel_ids: Vec<String>,
    pub wait_timeout_values: Vec<u32>,
    pub wait_receipt_delay_ms: u64,
    pub channel_receipt_delay_ms: u64,
    pub channel_handshake_steps: usize,
    pending_wait_delay_ms: u64,
    pub drop_channel_after_command: bool,
    pub reject_channel_handshakes: bool,
    pub revoke_after_command: bool,
    pub drop_command_receipts: bool,
    pub command_receipt_status_override: Option<OnlineStatus>,
    pub commands_seen: usize,
    pub last_delivery_mode: Option<i32>,
    pub model_selection_available: bool,
    pub model_observed: Option<String>,
    pub session_create_supported: bool,
    pub pending_result_polls: usize,
    pub settled_command_status: OnlineStatus,
    pub request_traces: Vec<String>,
    pairing: Option<NoiseSession>,
    pending_noise: Option<NoiseSession>,
    channel: Option<SecureChannel>,
    device_id: Option<String>,
    requests: HashMap<String, RequestState>,
    command_results: HashMap<String, ResponsePayload>,
}

impl FakeHost {
    pub fn set_query_unavailable(&mut self, value: bool) {
        self.query_error = value.then_some(ErrorCode::Unavailable);
    }

    pub fn new() -> Self {
        Self {
            identity: StaticIdentity::generate(),
            host_id: "host-1".into(),
            token: "one-time-token".into(),
            host_online: true,
            hide_primary_from_hosts: false,
            extra_host: false,
            extra_host_online: true,
            extra_host_session: "session-1".into(),
            revoked: false,
            fail_revoke_before: false,
            drop_revoke_receipt: false,
            drop_pair_finish_once: false,
            pair_finish_calls: 0,
            pair_hello_calls: 0,
            pair_hello_delay_ms: 0,
            hello_terminal_response: None,
            unconfirmed_pair_finish_status: None,
            committed_pair_finish_status: None,
            fence_pair_finish_once: false,
            fence_pair_finish_status: 409,
            saved_pair_finish: None,
            fail_hosts: false,
            hosts_delay_ms: 0,
            fail_pending_queue: false,
            query_error: None,
            legacy_capabilities_error: None,
            wait_timeouts: 0,
            wait_queries: 0,
            wait_last_timeout_ms: 0,
            wait_bad_id: false,
            wait_drop_channel_once: false,
            wait_replaced_once: false,
            wait_extra_replacements: 0,
            wait_source_unauthorized_once: false,
            wait_corrupt_source_error_once: false,
            wait_conflicting_source_error_once: false,
            wait_forged_success_once: false,
            wait_foreign_frame_once: false,
            corrupt_source_error: false,
            conflicting_source_error: false,
            forged_source_success: false,
            foreign_frame_error: false,
            wait_replace_at: 0,
            fail_handshake_polls_after_replacement: false,
            fail_handshake_polls: false,
            handshake_request_ids: HashSet::new(),
            pending_source_error: None,
            wait_ids: Vec::new(),
            wait_cancel_ids: Vec::new(),
            wait_timeout_values: Vec::new(),
            wait_receipt_delay_ms: 0,
            channel_receipt_delay_ms: 0,
            channel_handshake_steps: 0,
            pending_wait_delay_ms: 0,
            drop_channel_after_command: false,
            reject_channel_handshakes: false,
            revoke_after_command: false,
            drop_command_receipts: false,
            command_receipt_status_override: None,
            commands_seen: 0,
            last_delivery_mode: None,
            model_selection_available: true,
            model_observed: None,
            session_create_supported: false,
            pending_result_polls: 0,
            settled_command_status: OnlineStatus::Succeeded,
            request_traces: Vec::new(),
            pairing: None,
            pending_noise: None,
            channel: None,
            device_id: None,
            requests: HashMap::new(),
            command_results: HashMap::new(),
        }
    }

    pub fn handle(&mut self, request: MockRequest) -> MockResponse {
        let path = request.path.clone();
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (request.method.as_str(), segments.as_slice()) {
            ("POST", ["v1", "pairing", "claims"]) => self.pairing_claim(&request.body),
            ("GET", ["v1", "hosts"]) => {
                if self.revoked {
                    return error_response(401, ErrorCode::Unauthorized, "unauthorized");
                }
                if self.fail_hosts {
                    return error_response(503, ErrorCode::Unavailable, "unavailable");
                }
                let mut response = MockResponse::json(
                    200,
                    &HostList {
                        hosts: (!self.hide_primary_from_hosts)
                            .then(|| HostInfo {
                                host_id: self.host_id.clone(),
                                online: self.host_online,
                                protocol_version: PROTOCOL_VERSION,
                                fingerprint: sha256_hex(self.identity.public_key()),
                            })
                            .into_iter()
                            .chain(self.extra_host.then(|| HostInfo {
                                host_id: "host-2".into(),
                                online: self.extra_host_online,
                                protocol_version: PROTOCOL_VERSION,
                                fingerprint: sha256_hex(self.identity.public_key()),
                            }))
                            .collect(),
                    },
                );
                response.delay_ms = self.hosts_delay_ms;
                response
            }
            ("POST", ["v1", "hosts", host_id, "requests"]) => {
                self.host_request(host_id, &request.body)
            }
            ("GET", ["v1", "requests", request_id]) => match self.requests.get(*request_id) {
                Some(state) => MockResponse::json(200, state),
                None => error_response(404, ErrorCode::NotFound, "request not found"),
            },
            ("DELETE", ["v1", "devices", device_id]) => {
                if self.revoked {
                    return error_response(401, ErrorCode::Unauthorized, "unauthorized");
                }
                if Some(*device_id) == self.device_id.as_deref() {
                    self.revoked = true;
                }
                MockResponse::empty(204)
            }
            _ => error_response(404, ErrorCode::NotFound, "route not found"),
        }
    }

    pub fn drop_channel(&mut self) {
        self.channel = None;
        self.pending_noise = None;
    }

    pub fn channel_id(&self) -> Option<String> {
        self.channel
            .as_ref()
            .map(|channel| channel.channel_id().to_owned())
    }

    pub fn device_id(&self) -> Option<String> {
        self.device_id.clone()
    }

    pub fn request_mut(&mut self, request_id: &str) -> Option<&mut RequestState> {
        self.requests.get_mut(request_id)
    }

    pub fn last_request(&self) -> Option<&RequestState> {
        self.requests.values().next()
    }

    fn pairing_claim(&mut self, body: &[u8]) -> MockResponse {
        let claim: PairingClaimRequest = match serde_json::from_slice(body) {
            Ok(claim) => claim,
            Err(_) => return error_response(400, ErrorCode::InvalidArgument, "bad claim"),
        };
        if claim.token != self.token {
            return error_response(401, ErrorCode::Unauthorized, "invalid token");
        }
        self.device_id = Some(claim.device_id.clone());
        match PairingPhase::try_from(claim.phase).unwrap_or_default() {
            PairingPhase::Hello => {
                let token_hash = sha256_hex(self.token.as_bytes());
                let prologue = pairing_prologue(&self.host_id, &token_hash, &claim.claim_id);
                let mut noise = match NoiseSession::responder(&self.identity, &prologue) {
                    Ok(noise) => noise,
                    Err(_) => return error_response(500, ErrorCode::Internal, "noise failed"),
                };
                let payload = match noise.read(&claim.noise_message) {
                    Ok(payload) => payload,
                    Err(_) => return error_response(400, ErrorCode::InvalidArgument, "bad msg1"),
                };
                let hello = match PairingHello::decode(payload.as_slice()) {
                    Ok(hello) => hello,
                    Err(_) => return error_response(400, ErrorCode::InvalidArgument, "bad hello"),
                };
                let reply = PairingHelloReply {
                    host_id: self.host_id.clone(),
                    device_id: hello.device_id.clone(),
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: Some(default_capabilities()),
                    host_nonce: "host-nonce".into(),
                };
                let message = match noise.write(&reply.encode_to_vec()) {
                    Ok(message) => message,
                    Err(_) => return error_response(500, ErrorCode::Internal, "noise failed"),
                };
                self.pairing = Some(noise);
                MockResponse::json(
                    200,
                    &PairingClaimResponse {
                        claim_id: claim.claim_id,
                        host_id: self.host_id.clone(),
                        phase: PairingPhase::Hello as i32,
                        noise_message: message,
                        host_public_key: self.identity.public_key().to_vec(),
                        host_fingerprint: sha256_hex(self.identity.public_key()),
                        device_id: claim.device_id,
                        protocol_version: PROTOCOL_VERSION,
                        ..Default::default()
                    },
                )
            }
            PairingPhase::Finish => {
                self.pair_finish_calls += 1;
                let mut noise = match self.pairing.take() {
                    Some(noise) => noise,
                    None => return error_response(409, ErrorCode::Conflict, "no hello"),
                };
                let payload = match noise.read(&claim.noise_message) {
                    Ok(payload) => payload,
                    Err(_) => return error_response(400, ErrorCode::InvalidArgument, "bad msg3"),
                };
                if PairingFinish::decode(payload.as_slice()).is_err() {
                    return error_response(400, ErrorCode::InvalidArgument, "bad finish");
                }
                self.revoked = false;
                MockResponse::json(
                    200,
                    &PairingClaimResponse {
                        claim_id: claim.claim_id,
                        host_id: self.host_id.clone(),
                        phase: PairingPhase::Finish as i32,
                        device_id: claim.device_id,
                        device_token: claim.device_token.clone(),
                        device_public_key: noise.remote_static().unwrap().to_vec(),
                        protocol_version: PROTOCOL_VERSION,
                        ..Default::default()
                    },
                )
            }
            _ => error_response(400, ErrorCode::InvalidArgument, "bad phase"),
        }
    }

    fn host_request(&mut self, host_id: &str, body: &[u8]) -> MockResponse {
        let request: HostRequest = match serde_json::from_slice(body) {
            Ok(request) => request,
            Err(_) => return error_response(400, ErrorCode::InvalidArgument, "bad request"),
        };
        if request.kind == RequestKind::Handshake as i32 {
            self.handshake_request_ids
                .insert(request.request_id.clone());
        }
        self.request_traces.push(request.trace_id.clone());
        if host_id != self.host_id && !(self.extra_host && host_id == "host-2") {
            return error_response(404, ErrorCode::NotFound, "unknown host");
        }
        if self.revoked {
            return error_response(403, ErrorCode::Forbidden, "device revoked");
        }
        if !self.host_online || (host_id == "host-2" && !self.extra_host_online) {
            let state = RequestState {
                request_id: request.request_id.clone(),
                host_id: host_id.into(),
                device_id: request.device_id.clone(),
                status: OnlineStatus::HostOffline as i32,
                command_id: request.command_id.clone(),
                payload_digest: request.payload_digest.clone(),
                expires_at_unix_ms: request.expires_at_unix_ms,
                ..Default::default()
            };
            self.requests.insert(request.request_id.clone(), state);
            return MockResponse::json(
                200,
                &RequestReceipt {
                    request_id: request.request_id,
                    status: OnlineStatus::HostOffline as i32,
                    command_id: request.command_id,
                    expires_at_unix_ms: request.expires_at_unix_ms,
                },
            );
        }
        if request.kind == RequestKind::Command as i32 {
            if sha256(&request.body).to_vec() != request.payload_digest {
                return error_response(
                    409,
                    ErrorCode::PayloadDigestMismatch,
                    "payload digest mismatch",
                );
            }
            if let Some(existing) = self
                .requests
                .values()
                .find(|state| state.command_id == request.command_id)
            {
                if existing.payload_digest != request.payload_digest {
                    return error_response(
                        409,
                        ErrorCode::PayloadDigestMismatch,
                        "payload digest mismatch",
                    );
                }
                return MockResponse::json(
                    200,
                    &RequestReceipt {
                        request_id: existing.request_id.clone(),
                        status: OnlineStatus::Received as i32,
                        command_id: existing.command_id.clone(),
                        expires_at_unix_ms: existing.expires_at_unix_ms,
                    },
                );
            }
        }

        let mut error_code = 0;
        let (status, result_body) = match RequestKind::try_from(request.kind).unwrap_or_default() {
            RequestKind::Handshake => match self.handshake(&request) {
                Ok((status, body)) => (status, body),
                Err(code) => return error_response(400, code, "handshake failed"),
            },
            RequestKind::Query => match self.query(&request) {
                Ok(mut body) => {
                    if let Some(code) = self.pending_source_error.take() {
                        if self.forged_source_success {
                            self.forged_source_success = false;
                            (OnlineStatus::Succeeded, body)
                        } else {
                            if self.foreign_frame_error {
                                self.foreign_frame_error = false;
                                let mut frame =
                                    whalerein_proto::EncryptedFrame::decode(body.as_slice())
                                        .unwrap();
                                frame.channel_id = "foreign-channel".into();
                                body = frame.encode_to_vec();
                                error_code = ErrorCode::ChannelReplaced as i32;
                            } else if self.corrupt_source_error {
                                self.corrupt_source_error = false;
                                if let Some(byte) = body.last_mut() {
                                    *byte ^= 1;
                                }
                                error_code = ErrorCode::ChannelReplaced as i32;
                            } else if self.conflicting_source_error {
                                self.conflicting_source_error = false;
                                error_code = ErrorCode::ChannelReplaced as i32;
                            } else {
                                error_code = code as i32;
                            }
                            (OnlineStatus::Failed, body)
                        }
                    } else {
                        (OnlineStatus::Succeeded, body)
                    }
                }
                Err(code) => {
                    error_code = code as i32;
                    (OnlineStatus::Rejected, Vec::new())
                }
            },
            RequestKind::Command => match self.command(&request) {
                Ok((status, body)) => (status, body),
                Err(code) => return error_response(400, code, "command failed"),
            },
            _ => return error_response(400, ErrorCode::InvalidArgument, "unsupported kind"),
        };

        let state = RequestState {
            request_id: request.request_id.clone(),
            host_id: host_id.into(),
            device_id: request.device_id.clone(),
            status: status as i32,
            command_id: request.command_id.clone(),
            payload_digest: request.payload_digest.clone(),
            result_body,
            error_code,
            expires_at_unix_ms: request.expires_at_unix_ms,
            channel_id: request.channel_id.clone(),
            ..Default::default()
        };
        self.requests.insert(request.request_id.clone(), state);
        let mut response = MockResponse::json(
            200,
            &RequestReceipt {
                request_id: request.request_id,
                status: if request.kind == RequestKind::Command as i32 {
                    self.command_receipt_status_override
                        .unwrap_or(OnlineStatus::Received) as i32
                } else {
                    OnlineStatus::Received as i32
                },
                command_id: request.command_id,
                expires_at_unix_ms: request.expires_at_unix_ms,
            },
        );
        response.delay_ms = if request.kind == RequestKind::Handshake as i32 {
            std::mem::take(&mut self.channel_receipt_delay_ms)
        } else {
            std::mem::take(&mut self.pending_wait_delay_ms)
        };
        response
    }

    fn handshake(&mut self, request: &HostRequest) -> Result<(OnlineStatus, Vec<u8>), ErrorCode> {
        self.channel_handshake_steps += 1;
        if self.reject_channel_handshakes {
            return Err(ErrorCode::Unauthorized);
        }
        let prologue = channel_prologue(&request.host_id, &request.device_id, &request.channel_id);
        if let Some(mut noise) = self.pending_noise.take() {
            let payload = noise
                .read(&request.body)
                .map_err(|_| ErrorCode::InvalidArgument)?;
            if !payload.is_empty() {
                return Err(ErrorCode::InvalidArgument);
            }
            let hash = noise.handshake_hash().map_err(|_| ErrorCode::Internal)?;
            self.channel = Some(
                SecureChannel::from_handshake(
                    &hash,
                    &prologue,
                    &request.channel_id,
                    &request.host_id,
                    &request.device_id,
                )
                .map_err(|_| ErrorCode::Internal)?,
            );
            return Ok((OnlineStatus::Succeeded, Vec::new()));
        }

        let mut noise =
            NoiseSession::responder(&self.identity, &prologue).map_err(|_| ErrorCode::Internal)?;
        let payload = noise
            .read(&request.body)
            .map_err(|_| ErrorCode::InvalidArgument)?;
        let hello =
            ChannelHello::decode(payload.as_slice()).map_err(|_| ErrorCode::InvalidArgument)?;
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(ErrorCode::ProtocolVersionIncompatible);
        }
        let ready = ChannelReady {
            host_id: request.host_id.clone(),
            device_id: request.device_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Some(default_capabilities()),
            host_nonce: "host-channel-nonce".into(),
        };
        let message = noise
            .write(&ready.encode_to_vec())
            .map_err(|_| ErrorCode::Internal)?;
        self.pending_noise = Some(noise);
        Ok((OnlineStatus::Succeeded, message))
    }

    fn query(&mut self, request: &HostRequest) -> Result<Vec<u8>, ErrorCode> {
        if let Some(code) = self.query_error {
            return Err(code);
        }
        let channel = self.channel.clone().ok_or(ErrorCode::Unauthorized)?;
        let aad = crate::security::RequestAad {
            request_id: &request.request_id,
            host_id: &request.host_id,
            device_id: &request.device_id,
            kind: RequestKind::Query,
            command_id: "",
            expires_at_unix_ms: request.expires_at_unix_ms,
        };
        let frame =
            crate::security::decode_frame(&request.body).map_err(|_| ErrorCode::InvalidArgument)?;
        let plaintext = channel
            .peer_view()
            .open_request(&aad, &frame)
            .map_err(|_| ErrorCode::InvalidArgument)?;
        let payload = whalerein_proto::RequestPayload::decode(plaintext.as_slice())
            .map_err(|_| ErrorCode::InvalidArgument)?;
        let response = self.respond(payload, &request.host_id)?;
        let sealed = channel
            .peer_view()
            .seal_request(&aad, &response.encode_to_vec())
            .map_err(|_| ErrorCode::Internal)?;
        Ok(sealed.encode_to_vec())
    }

    fn respond(
        &mut self,
        payload: whalerein_proto::RequestPayload,
        host_id: &str,
    ) -> Result<ResponsePayload, ErrorCode> {
        use whalerein_proto::request_payload::Payload;
        let response = match payload.payload {
            Some(Payload::ListSessions(_)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::SessionList(
                    SessionList {
                        sessions: vec![SessionSummary {
                            session_id: if host_id == "host-2" {
                                self.extra_host_session.clone()
                            } else {
                                "session-1".into()
                            },
                            title: "test session".into(),
                            cwd: "/work".into(),
                            running: true,
                            agent_available: true,
                            source_capabilities: Some(whalerein_proto::SourceCapabilities {
                                model_selection: self.model_selection_available,
                                ..Default::default()
                            }),
                            history_cursor: String::new(),
                            ..Default::default()
                        }],
                    },
                )),
                ..Default::default()
            },
            Some(Payload::GetSession(get)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::SessionInfo(
                    SessionSummary {
                        session_id: get.session_id,
                        title: "test session".into(),
                        source_capabilities: Some(whalerein_proto::SourceCapabilities {
                            model_selection: self.model_selection_available,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            Some(Payload::ReadPendingQueue(_)) if self.fail_pending_queue => {
                return Err(ErrorCode::Unavailable)
            }
            Some(Payload::ReadPendingQueue(read)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::PendingQueue(
                    whalerein_proto::PendingQueue {
                        session_id: read.session_id,
                        available: true,
                        running: true,
                        items: vec![whalerein_proto::PendingQueueItem {
                            item_id: "queue-1".into(),
                            placement: whalerein_proto::QueuePlacement::Queued as i32,
                            blocks: vec![whalerein_proto::ContentBlock {
                                block_id: "text".into(),
                                content: Some(whalerein_proto::content_block::Content::Text(
                                    whalerein_proto::TextContent {
                                        text: "fixture".into(),
                                    },
                                )),
                            }],
                            actions: vec![
                                whalerein_proto::QueueAction::Edit as i32,
                                whalerein_proto::QueueAction::Remove as i32,
                            ],
                            ..Default::default()
                        }],
                    },
                )),
                ..Default::default()
            },
            Some(Payload::ListQuestions(list)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::QuestionPage(
                    whalerein_proto::QuestionPage {
                        batches: vec![fake_question(&list.session_id)],
                        next_cursor: String::new(),
                    },
                )),
                ..Default::default()
            },
            Some(Payload::GetQuestion(get)) if get.batch_id != "batch-1" => {
                return Err(ErrorCode::NotFound);
            }
            Some(Payload::GetQuestion(get)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::QuestionBatch(
                    fake_question(&get.session_id),
                )),
                ..Default::default()
            },
            Some(Payload::CancelQuestionAnswer(_)) => ResponsePayload {
                payload: Some(
                    whalerein_proto::response_payload::Payload::QuestionCancelReceipt(
                        whalerein_proto::QuestionCancelReceipt {
                            cancellation_requested: true,
                        },
                    ),
                ),
                ..Default::default()
            },
            Some(Payload::ReadHistory(read)) => {
                use whalerein_proto::*;
                let changes = if read.after_cursor == "bookmark:committed" {
                    vec![]
                } else {
                    vec![MessageUpsert {
                        message: Some(ConversationMessage {
                            message_id: "message-1".into(),
                            role: MessageRole::Assistant as i32,
                            blocks: vec![ContentBlock {
                                block_id: "text".into(),
                                content: Some(content_block::Content::Text(TextContent {
                                    text: "x".into(),
                                })),
                            }],
                            ..Default::default()
                        }),
                        origin: None,
                    }]
                };
                ResponsePayload {
                    payload: Some(response_payload::Payload::HistoryPage(HistoryPage {
                        session: Some(SessionKey {
                            host_id: "host-1".into(),
                            session_id: read.session_id,
                        }),
                        batch: Some(HistoryBatch {
                            from_cursor: if read.after_cursor.is_empty() {
                                "bookmark:initial".into()
                            } else {
                                read.after_cursor.clone()
                            },
                            to_cursor: "bookmark:committed".into(),
                            batch_id: format!("batch:{}:bookmark:committed", read.after_cursor),
                            changes,
                        }),
                        cut_cursor: "bookmark:committed".into(),
                        complete: true,
                        snapshot: read.snapshot,
                    })),
                    ..Default::default()
                }
            }
            Some(Payload::WatchSession(watch)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::WatchReady(
                    fake_watch(&watch.session_id),
                )),
                ..Default::default()
            },
            Some(Payload::RefreshWatches(_)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::WatchStates(
                    whalerein_proto::WatchStates {
                        watches: vec![fake_watch("session-1")],
                    },
                )),
                ..Default::default()
            },
            Some(Payload::UnwatchSession(_)) => ResponsePayload::default(),
            Some(Payload::WaitSession(wait)) => {
                self.wait_queries += 1;
                self.wait_last_timeout_ms = wait.timeout_ms;
                self.wait_ids.push(wait.wait_id.clone());
                self.wait_timeout_values.push(wait.timeout_ms);
                self.pending_wait_delay_ms = std::mem::take(&mut self.wait_receipt_delay_ms);
                if self.wait_foreign_frame_once {
                    self.wait_foreign_frame_once = false;
                    self.foreign_frame_error = true;
                    self.pending_source_error = Some(ErrorCode::Unauthorized);
                    return Ok(ResponsePayload {
                        error_code: ErrorCode::Unauthorized as i32,
                        error_detail: "source authorization failed".into(),
                        ..Default::default()
                    });
                }
                if self.wait_corrupt_source_error_once {
                    self.wait_corrupt_source_error_once = false;
                    self.corrupt_source_error = true;
                    self.pending_source_error = Some(ErrorCode::Unauthorized);
                    return Ok(ResponsePayload {
                        error_code: ErrorCode::Unauthorized as i32,
                        error_detail: "source authorization failed".into(),
                        ..Default::default()
                    });
                }
                if self.wait_conflicting_source_error_once {
                    self.wait_conflicting_source_error_once = false;
                    self.conflicting_source_error = true;
                    self.pending_source_error = Some(ErrorCode::Unauthorized);
                    return Ok(ResponsePayload {
                        error_code: ErrorCode::Unauthorized as i32,
                        error_detail: "source authorization failed".into(),
                        ..Default::default()
                    });
                }
                if self.wait_forged_success_once {
                    self.wait_forged_success_once = false;
                    self.forged_source_success = true;
                    self.pending_source_error = Some(ErrorCode::Unauthorized);
                    return Ok(ResponsePayload {
                        error_code: ErrorCode::Unauthorized as i32,
                        error_detail: "source authorization failed".into(),
                        ..Default::default()
                    });
                }
                if self.wait_replace_at == self.wait_queries {
                    self.channel = None;
                    self.fail_handshake_polls = self.fail_handshake_polls_after_replacement;
                    return Err(ErrorCode::ChannelReplaced);
                }
                if self.wait_source_unauthorized_once {
                    self.wait_source_unauthorized_once = false;
                    self.pending_source_error = Some(ErrorCode::Unauthorized);
                    return Ok(ResponsePayload {
                        error_code: ErrorCode::Unauthorized as i32,
                        error_detail: "source authorization failed".into(),
                        ..Default::default()
                    });
                }
                if self.wait_replaced_once {
                    self.wait_replaced_once = false;
                    self.channel = None;
                    return Err(ErrorCode::ChannelReplaced);
                }
                if self.wait_extra_replacements > 0 {
                    self.wait_extra_replacements -= 1;
                    self.channel = None;
                    return Err(ErrorCode::ChannelReplaced);
                }
                if self.wait_drop_channel_once {
                    self.wait_drop_channel_once = false;
                    self.channel = None;
                    return Err(ErrorCode::Unauthorized);
                }
                let reason = if self.wait_timeouts > 0 {
                    self.wait_timeouts -= 1;
                    whalerein_proto::WaitReason::WaitTimeout
                } else {
                    whalerein_proto::WaitReason::WaitIdle
                };
                ResponsePayload {
                    payload: Some(
                        whalerein_proto::response_payload::Payload::SessionWaitResult(
                            whalerein_proto::SessionWaitResult {
                                session_id: wait.session_id,
                                wait_id: if self.wait_bad_id {
                                    "other".into()
                                } else {
                                    wait.wait_id
                                },
                                reason: reason as i32,
                                state: Some(whalerein_proto::SessionWaitState {
                                    active: false,
                                    running: false,
                                    queue_available: false,
                                    pending_input: vec![],
                                }),
                            },
                        ),
                    ),
                    ..Default::default()
                }
            }
            Some(Payload::CancelSessionWait(cancel)) => {
                self.wait_cancel_ids.push(cancel.wait_id);
                ResponsePayload::default()
            }
            Some(Payload::ReadWaitState(read)) => ResponsePayload {
                payload: Some(
                    whalerein_proto::response_payload::Payload::SessionWaitStateResult(
                        whalerein_proto::SessionWaitStateResult {
                            session_id: if self.wait_bad_id {
                                "other".into()
                            } else {
                                read.session_id
                            },
                            state: Some(whalerein_proto::SessionWaitState {
                                active: false,
                                running: false,
                                queue_available: false,
                                pending_input: vec![],
                            }),
                        },
                    ),
                ),
                ..Default::default()
            },
            Some(Payload::GetCommandResult(get)) => {
                if self.pending_result_polls > 0 {
                    self.pending_result_polls -= 1;
                    ResponsePayload {
                        payload: Some(whalerein_proto::response_payload::Payload::CommandResult(
                            CommandResult {
                                found: true,
                                status: OnlineStatus::HostAccepted as i32,
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    }
                } else {
                    let terminal = self
                        .command_results
                        .get(&get.command_id)
                        .cloned()
                        .unwrap_or_default();
                    ResponsePayload {
                        payload: Some(whalerein_proto::response_payload::Payload::CommandResult(
                            CommandResult {
                                found: self.command_results.contains_key(&get.command_id),
                                status: self.settled_command_status as i32,
                                result_payload: terminal.encode_to_vec(),
                                error_code: 0,
                            },
                        )),
                        ..Default::default()
                    }
                }
            }
            Some(Payload::GetHostCapabilities(_)) if self.legacy_capabilities_error.is_some() => {
                return Err(self.legacy_capabilities_error.unwrap());
            }
            Some(Payload::GetHostCapabilities(_)) => ResponsePayload {
                payload: Some(
                    whalerein_proto::response_payload::Payload::HostCapabilities(
                        whalerein_proto::HostCapabilities {
                            model_catalog: self.model_selection_available,
                            model_read: self.model_selection_available,
                            session_create: self.session_create_supported,
                            session_fork: true,
                        },
                    ),
                ),
                ..Default::default()
            },
            Some(Payload::ListModels(_)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::ModelCatalog(
                    whalerein_proto::ModelCatalog {
                        default_selection: Some(whalerein_proto::ModelChoice {
                            provider: "provider".into(),
                            model: "model".into(),
                            effort: None,
                        }),
                        effect_scope: "session_and_source_default_attempt".into(),
                        complete: true,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            Some(Payload::ReadSessionModel(_)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::SessionModel(
                    whalerein_proto::SessionModel {
                        next: self.model_observed.as_ref().map(|model| {
                            whalerein_proto::ModelChoice {
                                provider: "provider".into(),
                                model: model.clone(),
                                effort: None,
                            }
                        }),
                        default_selection: Some(whalerein_proto::ModelChoice {
                            provider: "provider".into(),
                            model: "model".into(),
                            effort: None,
                        }),
                        default_is_fallback: true,
                        pending_known: true,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            Some(Payload::Ping(_)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::Pong(
                    whalerein_proto::Pong {
                        host_time_unix_ms: 1,
                    },
                )),
                ..Default::default()
            },
            _ => {
                return Err(ErrorCode::UnsupportedCapability);
            }
        };
        Ok(response)
    }

    fn command(&mut self, request: &HostRequest) -> Result<(OnlineStatus, Vec<u8>), ErrorCode> {
        let channel = self.channel.clone().ok_or(ErrorCode::Conflict)?;
        let aad = crate::security::RequestAad {
            request_id: &request.request_id,
            host_id: &request.host_id,
            device_id: &request.device_id,
            kind: RequestKind::Command,
            command_id: &request.command_id,
            expires_at_unix_ms: request.expires_at_unix_ms,
        };
        let frame =
            crate::security::decode_frame(&request.body).map_err(|_| ErrorCode::InvalidArgument)?;
        let plaintext = channel
            .peer_view()
            .open_request(&aad, &frame)
            .map_err(|_| ErrorCode::InvalidArgument)?;
        let payload = whalerein_proto::RequestPayload::decode(plaintext.as_slice())
            .map_err(|_| ErrorCode::InvalidArgument)?;
        if let Some(whalerein_proto::request_payload::Payload::SendMessage(ref send)) =
            payload.payload
        {
            self.last_delivery_mode = Some(send.delivery_mode);
        }
        let response = ResponsePayload {
            payload: Some(match payload.payload {
                Some(whalerein_proto::request_payload::Payload::CreateSession(_)) => {
                    whalerein_proto::response_payload::Payload::LifecycleReceipt(
                        whalerein_proto::LifecycleReceipt {
                            parent_session_id: String::new(),
                            session_id: "created-1".into(),
                            operation: "create".into(),
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
                    )
                }
                Some(whalerein_proto::request_payload::Payload::SelectSessionModel(selection)) => {
                    whalerein_proto::response_payload::Payload::ModelSelectionReceipt(
                        whalerein_proto::ModelSelectionReceipt {
                            selected: Some(whalerein_proto::ModelChoice {
                                provider: selection.provider,
                                model: selection.model,
                                effort: selection.effort,
                            }),
                            effect_scope: "session_and_source_default_attempt".into(),
                            default_persistence: "unconfirmed".into(),
                        },
                    )
                }
                Some(whalerein_proto::request_payload::Payload::AnswerQuestion(answer)) => {
                    whalerein_proto::response_payload::Payload::QuestionReceipt(
                        whalerein_proto::QuestionReceipt {
                            batch_id: answer.batch_id,
                            phase: whalerein_proto::QuestionPhase::QuestionReplyQueued as i32,
                        },
                    )
                }
                _ => whalerein_proto::response_payload::Payload::CommandAccepted(
                    whalerein_proto::CommandAccepted {
                        message_id: "message-1".into(),
                        delivery_id: "delivery-1".into(),
                    },
                ),
            }),
            ..Default::default()
        };
        self.command_results
            .insert(request.command_id.clone(), response.clone());
        let sealed = channel
            .peer_view()
            .seal_request(&aad, &response.encode_to_vec())
            .map_err(|_| ErrorCode::Internal)?;
        self.commands_seen += 1;
        if self.drop_channel_after_command {
            self.drop_channel();
        }
        if self.revoke_after_command {
            self.revoked = true;
        }
        Ok((OnlineStatus::HostAccepted, sealed.encode_to_vec()))
    }
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

fn error_response(status: u16, code: ErrorCode, message: &str) -> MockResponse {
    MockResponse::json(
        status,
        &ErrorResponse {
            code: code as i32,
            message: message.into(),
            ..Default::default()
        },
    )
}

/// A fake Access + fake host pair wired into one HTTP server.
pub struct FakeAccess {
    pub host: Arc<Mutex<FakeHost>>,
    pub server: MockServer,
}

impl FakeAccess {
    pub async fn start() -> Self {
        let host = Arc::new(Mutex::new(FakeHost::new()));
        let handler_host = host.clone();
        let server = MockServer::start(Arc::new(move |request| {
            let mut host = handler_host.lock().expect("fake host");
            if host.fail_handshake_polls
                && request.method == "GET"
                && request.path.starts_with("/v1/requests/")
                && host
                    .handshake_request_ids
                    .contains(request.path.trim_start_matches("/v1/requests/"))
            {
                return MockResponse::empty(0);
            }
            let hello = request.method == "POST"
                && request.path == "/v1/pairing/claims"
                && serde_json::from_slice::<PairingClaimRequest>(&request.body)
                    .is_ok_and(|claim| claim.phase == PairingPhase::Hello as i32);
            if hello {
                if let Some((code, status)) = host.hello_terminal_response.take() {
                    return error_response(status, code, "synthetic response");
                }
            }
            let finish = request.method == "POST"
                && request.path == "/v1/pairing/claims"
                && serde_json::from_slice::<PairingClaimRequest>(&request.body)
                    .is_ok_and(|claim| claim.phase == PairingPhase::Finish as i32);
            if finish {
                if let Some(status) = host.unconfirmed_pair_finish_status.take() {
                    return error_response(
                        status,
                        ErrorCode::PairingCommittedUnconfirmed,
                        "committed pairing Host confirmation unknown",
                    );
                }
                if let Some(status) = host.committed_pair_finish_status.take() {
                    return error_response(
                        status,
                        ErrorCode::PairingCommittedUnavailable,
                        "committed pairing unavailable",
                    );
                }
            }
            if finish && host.fence_pair_finish_once {
                host.fence_pair_finish_once = false;
                return error_response(
                    host.fence_pair_finish_status,
                    ErrorCode::PairingFenced,
                    "pairing fenced by revocation",
                );
            }
            if finish {
                if let Some((saved_request, saved_response)) = &host.saved_pair_finish {
                    if saved_request == &request.body {
                        if host.drop_pair_finish_once {
                            return MockResponse::empty(0);
                        }
                        return MockResponse {
                            status: 200,
                            body: saved_response.clone(),
                            delay_ms: 0,
                        };
                    }
                }
            }
            let finish_body = if finish {
                Some(request.body.clone())
            } else {
                None
            };
            let revoke = request.method == "DELETE" && request.path.starts_with("/v1/devices/");
            if revoke && host.fail_revoke_before {
                return MockResponse::empty(0);
            }
            let drop_revoke_receipt = revoke && host.drop_revoke_receipt;
            let drop_receipt = host.drop_command_receipts
                && request.method == "POST"
                && request.path.ends_with("/requests")
                && serde_json::from_slice::<HostRequest>(&request.body)
                    .is_ok_and(|request| request.kind == RequestKind::Command as i32);
            let mut response = host.handle(request);
            if hello {
                host.pair_hello_calls += 1;
                response.delay_ms = host.pair_hello_delay_ms;
            }
            if host.drop_pair_finish_once && finish && response.status == 200 {
                host.saved_pair_finish = Some((finish_body.unwrap(), response.body));
                return MockResponse::empty(0);
            }
            if drop_receipt || drop_revoke_receipt {
                MockResponse::empty(0)
            } else {
                response
            }
        }))
        .await;
        Self { host, server }
    }

    pub fn url(&self) -> url::Url {
        self.server.url()
    }

    pub fn pairing_uri(&self) -> String {
        let host = self.host.lock().unwrap();
        format!(
            "whalerein://pair?v=1&access={}&host={}&token={}",
            url::form_urlencoded::byte_serialize(self.url().as_str().as_bytes())
                .collect::<String>(),
            host.host_id,
            host.token
        )
    }

    pub fn revoke(&self) {
        self.host.lock().unwrap().revoked = true;
    }

    pub fn fail_hosts(&self, fail: bool) {
        self.host.lock().unwrap().fail_hosts = fail;
    }
}

fn fake_watch(session: &str) -> whalerein_proto::WatchReady {
    whalerein_proto::WatchReady {
        watch_id: "watch-1".into(),
        lease_ms: 30000,
        state: Some(whalerein_proto::SessionState {
            session: Some(whalerein_proto::SessionKey {
                host_id: "host-1".into(),
                session_id: session.into(),
            }),
            live_epoch: "epoch".into(),
            revision: 1,
            history_head: "bookmark:committed".into(),
            run_phase: whalerein_proto::RunPhase::Idle as i32,
            approvals: vec![],
        }),
    }
}

// CLI assertions inspect the same redacted diagnostic record as Core.
pub use crate::diagnostics::journal::load_failure;

pub fn fake_unknown_status() -> OnlineStatus {
    OnlineStatus::Unknown
}
pub fn fake_succeeded_status() -> OnlineStatus {
    OnlineStatus::Succeeded
}

fn fake_question(session_id: &str) -> whalerein_proto::QuestionBatch {
    whalerein_proto::QuestionBatch {
        session_id: session_id.into(),
        batch_id: "batch-1".into(),
        source_epoch: "epoch".into(),
        version: 1,
        digest: vec![7; 32],
        phase: whalerein_proto::QuestionPhase::QuestionContinued as i32,
        questions: vec![whalerein_proto::QuestionItem {
            id: "q".into(),
            question: "Synthetic question".into(),
            options: vec![whalerein_proto::QuestionOption {
                label: "A".into(),
                description: Some("Option".into()),
            }],
            ..Default::default()
        }],
        answerable: true,
        content_unavailable: false,
    }
}
