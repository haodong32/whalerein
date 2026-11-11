//! Test-only fake Access HTTP server and fake DSH host used by Core tests.
//!
//! This is not compiled into the library. It exercises the real HTTP client,
//! Noise handshake, request polling and command paths without a network peer.

use std::collections::HashMap;
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
}

impl MockResponse {
    pub fn json<T: serde::Serialize>(status: u16, value: &T) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap(),
        }
    }

    pub fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
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
    pub revoked: bool,
    pub fail_hosts: bool,
    pub query_error: Option<ErrorCode>,
    pub drop_channel_after_command: bool,
    pub revoke_after_command: bool,
    pub drop_command_receipts: bool,
    pub commands_seen: usize,
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
    pub fn new() -> Self {
        Self {
            identity: StaticIdentity::generate(),
            host_id: "host-1".into(),
            token: "one-time-token".into(),
            host_online: true,
            revoked: false,
            fail_hosts: false,
            query_error: None,
            drop_channel_after_command: false,
            revoke_after_command: false,
            drop_command_receipts: false,
            commands_seen: 0,
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
                MockResponse::json(
                    200,
                    &HostList {
                        hosts: vec![HostInfo {
                            host_id: self.host_id.clone(),
                            online: self.host_online,
                            protocol_version: PROTOCOL_VERSION,
                            fingerprint: sha256_hex(self.identity.public_key()),
                        }],
                    },
                )
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
        self.request_traces.push(request.trace_id.clone());
        if host_id != self.host_id {
            return error_response(404, ErrorCode::NotFound, "unknown host");
        }
        if self.revoked {
            return error_response(403, ErrorCode::Forbidden, "device revoked");
        }
        if !self.host_online {
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
                Ok(body) => (OnlineStatus::Succeeded, body),
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
        MockResponse::json(
            200,
            &RequestReceipt {
                request_id: request.request_id,
                status: OnlineStatus::Received as i32,
                command_id: request.command_id,
                expires_at_unix_ms: request.expires_at_unix_ms,
            },
        )
    }

    fn handshake(&mut self, request: &HostRequest) -> Result<(OnlineStatus, Vec<u8>), ErrorCode> {
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
        let response = self.respond(payload)?;
        let sealed = channel
            .peer_view()
            .seal_request(&aad, &response.encode_to_vec())
            .map_err(|_| ErrorCode::Internal)?;
        Ok(sealed.encode_to_vec())
    }

    fn respond(
        &mut self,
        payload: whalerein_proto::RequestPayload,
    ) -> Result<ResponsePayload, ErrorCode> {
        use whalerein_proto::request_payload::Payload;
        let response = match payload.payload {
            Some(Payload::ListSessions(_)) => ResponsePayload {
                payload: Some(whalerein_proto::response_payload::Payload::SessionList(
                    SessionList {
                        sessions: vec![SessionSummary {
                            session_id: "session-1".into(),
                            title: "test session".into(),
                            cwd: "/work".into(),
                            running: true,
                            agent_available: true,
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
                        ..Default::default()
                    },
                )),
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
        let _payload = whalerein_proto::RequestPayload::decode(plaintext.as_slice())
            .map_err(|_| ErrorCode::InvalidArgument)?;
        let response = ResponsePayload {
            payload: Some(whalerein_proto::response_payload::Payload::CommandAccepted(
                whalerein_proto::CommandAccepted {
                    message_id: "message-1".into(),
                },
            )),
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
            let drop_receipt = host.drop_command_receipts
                && request.method == "POST"
                && request.path.ends_with("/requests")
                && serde_json::from_slice::<HostRequest>(&request.body)
                    .is_ok_and(|request| request.kind == RequestKind::Command as i32);
            let response = host.handle(request);
            if drop_receipt {
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
