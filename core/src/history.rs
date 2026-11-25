//! Independent, bounded committed-history reads.
//!
//! One public path serves a latest-first page and an exact committed-message
//! read. Online reads own their request budget and never create a watch lease,
//! MQTT subscription or cache write. Cache reads use the existing encrypted
//! cache reader and are served locally.
use crate::api::{Core, CoreError, CoreResult};
use crate::conversation::{ConversationMessage, ToolActivities, ToolActivity};
use crate::session::SessionKey;
use crate::sync::{validate_batch, validate_message};
use base64::Engine;
use serde::{Deserialize, Serialize};
use whalerein_proto::{
    request_payload, response_payload, CommittedMessageRef, HistoryPageSelection, HistoryRead,
    RequestPayload,
};

const DEFAULT_MAX_MESSAGES: u32 = 100;
const MAX_PAGE_MESSAGES: u32 = 1000;
const DEFAULT_MAX_BYTES: u32 = 256 * 1024;
const MAX_PAGE_BYTES: u32 = 1024 * 1024;
/// Continuations are a bounded, user-facing continuation, never an
/// authorization token. Expiry forces an explicit restart instead of silently
/// walking a changed cut.
const CONTINUATION_TTL_MS: i64 = 60 * 60 * 1000;
const MAX_TOKEN_CHARS: usize = 8192;

#[cfg(test)]
thread_local! {
    /// Test-only switch used by the isolated first-history comparator.
    static BROAD_FIRST_PAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn set_broad_first_page(enabled: bool) {
    BROAD_FIRST_PAGE.with(|flag| flag.set(enabled));
}

/// Requested history source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMode {
    #[default]
    Source,
    Cache,
}

/// Concrete source of a returned page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    Online,
    Cache,
}

impl HistorySource {
    fn as_wire(self) -> i32 {
        match self {
            Self::Online => whalerein_proto::HistorySource::Online as i32,
            Self::Cache => whalerein_proto::HistorySource::Cache as i32,
        }
    }
    fn from_wire(value: i32) -> Option<Self> {
        match whalerein_proto::HistorySource::try_from(value).ok()? {
            whalerein_proto::HistorySource::Online => Some(Self::Online),
            whalerein_proto::HistorySource::Cache => Some(Self::Cache),
            whalerein_proto::HistorySource::Unspecified => None,
        }
    }
}

/// One latest-first bounded page request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPageRequest {
    pub session: SessionKey,
    pub mode: HistoryMode,
    pub continuation: Option<HistoryContinuation>,
    pub max_messages: u32,
    pub max_bytes: u32,
}

impl HistoryPageRequest {
    pub fn new(session: SessionKey) -> Self {
        Self {
            session,
            mode: HistoryMode::Source,
            continuation: None,
            max_messages: DEFAULT_MAX_MESSAGES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
    pub fn cache(mut self) -> Self {
        self.mode = HistoryMode::Cache;
        self
    }
    pub fn continuation(mut self, continuation: HistoryContinuation) -> Self {
        self.continuation = Some(continuation);
        self
    }
}

/// One returned page. Messages are chronological Source order within the page;
/// later pages step toward older history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub source: HistorySource,
    pub projection: String,
    pub complete: bool,
    pub messages: Vec<ConversationMessage>,
    pub activities: Vec<ToolActivity>,
    pub continuation: Option<HistoryContinuation>,
}

/// Core-owned opaque continuation. It binds the validated Host/session scope,
/// projection, fixed cut, next position and read source. Consumers never parse
/// it; a wrong-scope, expired, projection-mismatched or non-progressing value
/// fails with `snapshot_required`.
#[derive(Clone, PartialEq, Eq)]
pub struct HistoryContinuation {
    token: String,
}

impl std::fmt::Debug for HistoryContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HistoryContinuation(..)")
    }
}

impl std::fmt::Display for HistoryContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.token)
    }
}

impl std::str::FromStr for HistoryContinuation {
    type Err = CoreError;
    fn from_str(value: &str) -> CoreResult<Self> {
        if value.is_empty() || value.len() > MAX_TOKEN_CHARS {
            return Err(CoreError::InvalidInput(
                "invalid history continuation".into(),
            ));
        }
        Ok(Self {
            token: value.to_owned(),
        })
    }
}

impl Serialize for HistoryContinuation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.token)
    }
}

impl<'de> Deserialize<'de> for HistoryContinuation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// A scoped canonical committed-message reference. Resolve it through
/// [`Core::history_message`]; message bodies stay in history.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageReference {
    session: SessionKey,
    message_id: String,
    source_locator: String,
    projection_id: String,
}

impl std::fmt::Debug for MessageReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MessageReference(..)")
    }
}

impl MessageReference {
    pub fn new(
        session: SessionKey,
        message_id: impl Into<String>,
        source_locator: impl Into<String>,
        projection_id: impl Into<String>,
    ) -> CoreResult<Self> {
        let message_id = message_id.into();
        let source_locator = source_locator.into();
        let projection_id = projection_id.into();
        if message_id.is_empty() || message_id.len() > 512 {
            return Err(CoreError::InvalidInput("invalid message identity".into()));
        }
        if source_locator.is_empty() || source_locator.len() > 4096 {
            return Err(CoreError::InvalidInput("invalid message locator".into()));
        }
        if projection_id.is_empty() || projection_id.len() > 128 {
            return Err(CoreError::InvalidInput("invalid content projection".into()));
        }
        Ok(Self {
            session,
            message_id,
            source_locator,
            projection_id,
        })
    }

    pub fn session(&self) -> &SessionKey {
        &self.session
    }
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
    pub fn projection_id(&self) -> &str {
        &self.projection_id
    }
    /// Source-provided locator needed to reconstruct this exact reference.
    /// It is opaque to callers and never contains message content.
    pub fn source_locator(&self) -> &str {
        &self.source_locator
    }

    pub(crate) fn from_committed(
        session: &SessionKey,
        committed: &CommittedMessageRef,
    ) -> CoreResult<Self> {
        let wire_session = committed
            .session
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing committed session scope".into()))?;
        if wire_session.host_id != session.host_id || wire_session.session_id != session.session_id
        {
            return Err(CoreError::Security(
                "committed message scope mismatch".into(),
            ));
        }
        Self::new(
            session.clone(),
            committed.message_id.clone(),
            committed.source_locator.clone(),
            committed.projection_id.clone(),
        )
        .map_err(|_| CoreError::Security("invalid committed message reference".into()))
    }

    fn wire(&self) -> CommittedMessageRef {
        CommittedMessageRef {
            session: Some((&self.session).into()),
            message_id: self.message_id.clone(),
            source_locator: self.source_locator.clone(),
            projection_id: self.projection_id.clone(),
        }
    }

    /// Opaque user-facing token for `--message` and JSON output.
    pub fn to_token(&self) -> String {
        let payload = ReferencePayload {
            v: 1,
            host: self.session.host_id.clone(),
            session: self.session.session_id.clone(),
            projection: self.projection_id.clone(),
            message_id: self.message_id.clone(),
            locator: self.source_locator.clone(),
            issued_at_unix_ms: crate::util::now_unix_ms(),
        };
        encode_token(REFERENCE_DOMAIN, &payload)
    }
}

impl std::fmt::Display for MessageReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.to_token())
    }
}

impl std::str::FromStr for MessageReference {
    type Err = CoreError;
    fn from_str(value: &str) -> CoreResult<Self> {
        let payload: ReferencePayload = decode_token(REFERENCE_DOMAIN, value)?;
        if payload.v != 1 {
            return Err(CoreError::InvalidInput("invalid message reference".into()));
        }
        let session = SessionKey::new(payload.host, payload.session)?;
        Self::new(
            session,
            payload.message_id,
            payload.locator,
            payload.projection,
        )
    }
}

impl Serialize for MessageReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_token())
    }
}

impl<'de> Deserialize<'de> for MessageReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

const CONTINUATION_DOMAIN: &str = "whalerein.history.continuation.v1";
const REFERENCE_DOMAIN: &str = "whalerein.history.reference.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContinuationPayload {
    v: u8,
    host: String,
    session: String,
    projection: String,
    cut: String,
    next: String,
    source: String,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    install: String,
    issued_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReferencePayload {
    v: u8,
    host: String,
    session: String,
    projection: String,
    message_id: String,
    locator: String,
    issued_at_unix_ms: i64,
}

fn encode_token<T: Serialize>(domain: &str, payload: &T) -> String {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    let digest = crate::util::sha256(
        [domain.as_bytes(), b"\0", bytes.as_slice()]
            .concat()
            .as_slice(),
    );
    format!(
        "whr1.{}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes),
        hex::encode(digest)
    )
}

fn decode_token<T: for<'de> Deserialize<'de>>(domain: &str, token: &str) -> CoreResult<T> {
    if token.is_empty() || token.len() > MAX_TOKEN_CHARS {
        return Err(CoreError::InvalidInput("invalid history token".into()));
    }
    let mut parts = token.split('.');
    let prefix = parts.next();
    let body = parts.next();
    let digest = parts.next();
    if prefix != Some("whr1") || parts.next().is_some() {
        return Err(CoreError::InvalidInput("invalid history token".into()));
    }
    let (body, digest) = body
        .zip(digest)
        .ok_or_else(|| CoreError::InvalidInput("invalid history token".into()))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| CoreError::InvalidInput("invalid history token".into()))?;
    if bytes.len() > 4096 {
        return Err(CoreError::InvalidInput("invalid history token".into()));
    }
    let expected = hex::encode(crate::util::sha256(
        [domain.as_bytes(), b"\0", bytes.as_slice()]
            .concat()
            .as_slice(),
    ));
    if expected != digest {
        return Err(CoreError::InvalidInput("invalid history token".into()));
    }
    let payload: T = serde_json::from_slice(&bytes)
        .map_err(|_| CoreError::InvalidInput("invalid history token".into()))?;
    Ok(payload)
}

fn snapshot_required(message: &str) -> CoreError {
    CoreError::Api {
        code: Some(whalerein_proto::ErrorCode::SnapshotRequired),
        message: message.into(),
    }
}

fn bounded(max_messages: u32, max_bytes: u32) -> (u32, u32) {
    (
        max_messages.clamp(1, MAX_PAGE_MESSAGES),
        max_bytes.clamp(1, MAX_PAGE_BYTES),
    )
}

fn cache_fingerprint(messages: &[ConversationMessage]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for message in messages {
        hasher.update(message.message_id.as_bytes());
        hasher.update([0u8]);
        hasher.update(message.authored_at_unix_ms.to_le_bytes());
        for block in &message.blocks {
            hasher.update(block.block_id.as_bytes());
            hasher.update([0u8]);
            match &block.content {
                Some(crate::conversation::content_block::Content::Text(text)) => {
                    hasher.update(text.text.as_bytes());
                }
                Some(crate::conversation::content_block::Content::Reasoning(reasoning)) => {
                    hasher.update(reasoning.text.as_bytes());
                }
                Some(crate::conversation::content_block::Content::Notice(notice)) => {
                    hasher.update(notice.summary.as_deref().unwrap_or_default().as_bytes());
                }
                Some(crate::conversation::content_block::Content::ToolActivity(activity)) => {
                    hasher.update(activity.id.call_id.as_bytes());
                }
                Some(crate::conversation::content_block::Content::Attachment(attachment)) => {
                    hasher.update(attachment.attachment_ref.as_bytes());
                }
                None => {}
            }
            hasher.update([0u8]);
        }
    }
    hex::encode(hasher.finalize())
}

fn estimated_bytes(message: &ConversationMessage) -> usize {
    let mut total = message.message_id.len() + 8;
    for block in &message.blocks {
        total += block.block_id.len() + 8;
        total += match &block.content {
            Some(crate::conversation::content_block::Content::Text(text)) => text.text.len(),
            Some(crate::conversation::content_block::Content::Reasoning(reasoning)) => {
                reasoning.text.len()
            }
            Some(crate::conversation::content_block::Content::Notice(notice)) => {
                notice.summary.as_ref().map_or(0, String::len)
            }
            Some(crate::conversation::content_block::Content::Attachment(attachment)) => {
                attachment.name.len()
                    + attachment.media_type.len()
                    + attachment.attachment_ref.len()
            }
            Some(crate::conversation::content_block::Content::ToolActivity(activity)) => {
                activity.name.len()
                    + activity.summary.as_ref().map_or(0, String::len)
                    + activity.progress.as_ref().map_or(0, String::len)
                    + activity
                        .result
                        .iter()
                        .map(|part| match part {
                            crate::conversation::ToolResultPart::Text(text) => text.len(),
                            crate::conversation::ToolResultPart::Attachment(attachment) => {
                                attachment.name.len() + attachment.attachment_ref.len()
                            }
                        })
                        .sum::<usize>()
            }
            None => 0,
        };
    }
    total
}

struct ContinuationBinding {
    projection: String,
    cut: String,
    next: String,
    source: HistorySource,
    generation: u64,
    install: String,
}

impl Core {
    fn history_diagnostic(&self, stage: &'static str, outcome: &'static str, code: &str) {
        self.diagnostics.record_stage("core", stage, outcome, code);
    }

    fn continuation_from_token(
        &self,
        token: &HistoryContinuation,
        session: &SessionKey,
        mode: HistoryMode,
    ) -> CoreResult<ContinuationBinding> {
        let payload: ContinuationPayload = decode_token(CONTINUATION_DOMAIN, &token.token)
            .map_err(|_| {
                self.history_diagnostic("history_continuation", "rejected", "invalid_continuation");
                snapshot_required("history continuation is invalid")
            })?;
        let age = crate::util::now_unix_ms().saturating_sub(payload.issued_at_unix_ms);
        if payload.v != 1
            || !(0..=CONTINUATION_TTL_MS).contains(&age)
            || payload.host != session.host_id
            || payload.session != session.session_id
        {
            self.history_diagnostic("history_continuation", "rejected", "invalid_continuation");
            return Err(snapshot_required(
                "history continuation has expired or changed scope",
            ));
        }
        let source = match payload.source.as_str() {
            "online" => HistorySource::Online,
            "cache" => HistorySource::Cache,
            _ => {
                self.history_diagnostic("history_continuation", "rejected", "invalid_continuation");
                return Err(snapshot_required(
                    "history continuation has an invalid source",
                ));
            }
        };
        if (mode == HistoryMode::Source) != (source == HistorySource::Online)
            || payload.projection.is_empty()
            || payload.projection.len() > 128
            || payload.cut.is_empty()
            || payload.cut.len() > 4096
            || payload.next.is_empty()
            || payload.next.len() > 4096
        {
            self.history_diagnostic("history_continuation", "rejected", "invalid_continuation");
            return Err(snapshot_required("history continuation is incomplete"));
        }
        Ok(ContinuationBinding {
            projection: payload.projection,
            cut: payload.cut,
            next: payload.next,
            source,
            generation: payload.generation,
            install: payload.install,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn page_continuation(
        &self,
        session: &SessionKey,
        projection: &str,
        cut: &str,
        next: &str,
        source: HistorySource,
        generation: u64,
        install: &str,
    ) -> CoreResult<HistoryContinuation> {
        if next.is_empty() {
            return Err(snapshot_required("history pagination did not progress"));
        }
        let payload = ContinuationPayload {
            v: 1,
            host: session.host_id.clone(),
            session: session.session_id.clone(),
            projection: projection.to_owned(),
            cut: cut.to_owned(),
            next: next.to_owned(),
            source: match source {
                HistorySource::Online => "online".into(),
                HistorySource::Cache => "cache".into(),
            },
            generation,
            install: install.to_owned(),
            issued_at_unix_ms: crate::util::now_unix_ms(),
        };
        Ok(HistoryContinuation {
            token: encode_token(CONTINUATION_DOMAIN, &payload),
        })
    }

    /// Read one latest-first bounded page over the single `HistoryRead` path.
    pub async fn history_page(&self, request: &HistoryPageRequest) -> CoreResult<HistoryPage> {
        self.authorized_session(&request.session)?;
        let (max_messages, max_bytes) = bounded(request.max_messages, request.max_bytes);
        let result = match request.mode {
            HistoryMode::Source => {
                self.history_page_online(request, max_messages, max_bytes)
                    .await
            }
            HistoryMode::Cache => self.history_page_cache(request, max_messages, max_bytes),
        };
        match &result {
            Ok(_) => self.history_diagnostic("history_page", "succeeded", "none"),
            Err(error) => self.history_diagnostic("history_page", "failed", error.code()),
        }
        result
    }

    /// Largest first visible page that can travel inline on the MQTT reply
    /// lane. The packet includes the HostResult envelope, HistoryPage and
    /// batch framing, so only the remainder is available to committed
    /// messages. Later pages and bulk synchronization keep their caller bound.
    #[cfg(test)]
    fn broad_first_page() -> bool {
        BROAD_FIRST_PAGE.with(|flag| flag.get())
    }

    fn first_page_inline_bytes(&self, host_id: &str) -> u32 {
        #[cfg(test)]
        if Self::broad_first_page() {
            // Test-only comparator mode: reproduce the pre-change page target so
            // the isolated comparison can measure both packings on the same
            // seeded history. Production never selects this.
            return MAX_PAGE_BYTES;
        }
        let local = self.transport_budget();
        let receive = self.runtime.host_receive(host_id).unwrap_or(local);
        let packet = local.packet_bytes.min(receive.packet_bytes);
        packet.saturating_sub(4 * 1024).clamp(1024, MAX_PAGE_BYTES)
    }

    async fn history_page_online(
        &self,
        request: &HistoryPageRequest,
        max_messages: u32,
        max_bytes: u32,
    ) -> CoreResult<HistoryPage> {
        let max_bytes = if request.continuation.is_none() {
            max_bytes.min(self.first_page_inline_bytes(&request.session.host_id))
        } else {
            max_bytes
        };
        let binding = match &request.continuation {
            Some(continuation) => Some(self.continuation_from_token(
                continuation,
                &request.session,
                HistoryMode::Source,
            )?),
            None => None,
        };
        let before_cursor = binding
            .as_ref()
            .map(|binding| binding.next.clone())
            .unwrap_or_default();
        let cut_cursor = binding
            .as_ref()
            .map(|binding| binding.cut.clone())
            .unwrap_or_default();
        let read = HistoryRead {
            session_id: request.session.session_id.clone(),
            snapshot: false,
            after_cursor: String::new(),
            cut_cursor,
            max_bytes,
            max_messages,
            page: Some(HistoryPageSelection { before_cursor }),
            exact: None,
            continuation: String::new(),
            source: HistorySource::Online.as_wire(),
        };
        let response = self
            .query(
                &request.session.host_id,
                RequestPayload {
                    payload: Some(request_payload::Payload::ReadHistory(read)),
                },
            )
            .await?;
        let Some(response_payload::Payload::HistoryPage(page)) = response.payload else {
            return Err(CoreError::Security(
                "unexpected history response payload".into(),
            ));
        };
        self.normalize_page(&request.session, page, binding, max_messages)
    }

    fn normalize_page(
        &self,
        session: &SessionKey,
        page: whalerein_proto::HistoryPage,
        binding: Option<ContinuationBinding>,
        max_messages: u32,
    ) -> CoreResult<HistoryPage> {
        if page.session.as_ref() != Some(&whalerein_proto::SessionKey::from(session)) {
            return Err(CoreError::Security("history scope mismatch".into()));
        }
        let source = HistorySource::from_wire(page.source)
            .ok_or_else(|| CoreError::Security("invalid history source".into()))?;
        if source != HistorySource::Online {
            return Err(CoreError::Security("history source mismatch".into()));
        }
        if page.projection_id.is_empty() || page.projection_id.len() > 128 {
            return Err(self.projection_reset());
        }
        let batch = page
            .batch
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing history batch".into()))?;
        validate_batch(batch)?;
        if batch.projection_id != page.projection_id {
            return Err(self.projection_reset());
        }
        if page.cut_cursor.is_empty() || page.cut_cursor.len() > 4096 {
            return Err(CoreError::Security("invalid history cut".into()));
        }
        if let Some(binding) = &binding {
            if binding.source != HistorySource::Online {
                return Err(self.projection_reset());
            }
            if binding.projection != page.projection_id {
                return Err(self.projection_reset());
            }
            if binding.cut != page.cut_cursor {
                return Err(snapshot_required("history cut changed"));
            }
            if batch.from_cursor.is_empty() || batch.from_cursor == binding.next {
                return Err(snapshot_required("history continuation did not progress"));
            }
        }
        let mut messages = Vec::with_capacity(batch.changes.len());
        for change in &batch.changes {
            let message = change
                .message
                .as_ref()
                .ok_or_else(|| CoreError::Security("missing history message".into()))?;
            validate_message(message)?;
            messages.push(message.clone().into());
        }
        if messages.len() > max_messages as usize {
            return Err(CoreError::Security(
                "history page exceeded its bound".into(),
            ));
        }
        let mut activities = ToolActivities::default();
        activities.absorb(
            messages
                .iter()
                .flat_map(ConversationMessage::tool_activities),
        );
        let continuation = if page.complete {
            None
        } else {
            if batch.from_cursor.is_empty() {
                return Err(snapshot_required(
                    "history continuation position is missing",
                ));
            }
            Some(self.page_continuation(
                session,
                &page.projection_id,
                &page.cut_cursor,
                &batch.from_cursor,
                HistorySource::Online,
                0,
                "",
            )?)
        };
        Ok(HistoryPage {
            source: HistorySource::Online,
            projection: page.projection_id,
            complete: page.complete,
            messages,
            activities: activities.ordered().into_iter().cloned().collect(),
            continuation,
        })
    }

    fn history_page_cache(
        &self,
        request: &HistoryPageRequest,
        max_messages: u32,
        max_bytes: u32,
    ) -> CoreResult<HistoryPage> {
        let binding = match &request.continuation {
            Some(continuation) => Some(self.continuation_from_token(
                continuation,
                &request.session,
                HistoryMode::Cache,
            )?),
            None => None,
        };
        let cache = self.cache()?;
        let Some(view) = cache.history_view(&request.session)? else {
            // No cache installation for this session is a truthful finite
            // empty read, not a fenced obsolete projection. A continuation
            // still refers to an install that was replaced or evicted.
            if binding.is_some() {
                return Err(snapshot_required(
                    "cached history continuation refers to a replaced or evicted install",
                ));
            }
            return Ok(HistoryPage {
                source: HistorySource::Cache,
                projection: String::new(),
                complete: true,
                messages: Vec::new(),
                activities: Vec::new(),
                continuation: None,
            });
        };
        if view.projection.is_empty() || view.projection.len() > 128 {
            return Err(snapshot_required(
                "cached history belongs to a previous content projection",
            ));
        }
        let projection = view.projection;
        let generation = cache.cache_generation()?;
        let install = cache.cache_install_id()?;
        let messages = cache.history_messages(&request.session)?;
        let end = if let Some(binding) = &binding {
            if binding.source != HistorySource::Cache
                || binding.projection != projection
                || binding.generation != generation
                || binding.install != install
            {
                return Err(self.projection_reset());
            }
            let end: usize = binding
                .next
                .parse()
                .map_err(|_| snapshot_required("history continuation position is invalid"))?;
            if end > messages.len() || cache_fingerprint(&messages[..end]) != binding.cut {
                return Err(snapshot_required("cached history was replaced or evicted"));
            }
            end
        } else {
            messages.len()
        };
        let mut start = end.saturating_sub(max_messages as usize);
        while start < end
            && messages[start..end]
                .iter()
                .map(estimated_bytes)
                .sum::<usize>()
                > max_bytes as usize
        {
            start += 1;
        }
        if start == end && end > 0 {
            return Err(CoreError::Api {
                code: Some(whalerein_proto::ErrorCode::PayloadTooLarge),
                message: "cached message exceeds the page budget".into(),
            });
        }
        let page_messages = messages[start..end].to_vec();
        let mut activities = ToolActivities::default();
        activities.absorb(
            page_messages
                .iter()
                .flat_map(ConversationMessage::tool_activities),
        );
        let complete = start == 0;
        let continuation = if complete {
            None
        } else {
            Some(self.page_continuation(
                &request.session,
                &projection,
                &cache_fingerprint(&messages[..start]),
                &start.to_string(),
                HistorySource::Cache,
                generation,
                &install,
            )?)
        };
        Ok(HistoryPage {
            source: HistorySource::Cache,
            projection,
            complete,
            messages: page_messages,
            activities: activities.ordered().into_iter().cloned().collect(),
            continuation,
        })
    }

    fn projection_reset(&self) -> CoreError {
        self.history_diagnostic("history_projection", "reset", "snapshot_required");
        snapshot_required("content projection changed; restart history")
    }

    /// Resolve one exact scoped committed message. It uses the same
    /// authorization, scope, identity and canonical decoding path as paging.
    pub async fn history_message(
        &self,
        reference: &MessageReference,
    ) -> CoreResult<ConversationMessage> {
        self.authorized_session(&reference.session)?;
        let max_messages = 1;
        let max_bytes = DEFAULT_MAX_BYTES;
        let read = HistoryRead {
            session_id: reference.session.session_id.clone(),
            snapshot: false,
            after_cursor: String::new(),
            cut_cursor: String::new(),
            max_bytes,
            max_messages,
            page: None,
            exact: Some(reference.wire()),
            continuation: String::new(),
            source: HistorySource::Online.as_wire(),
        };
        let result = async {
            let response = self
                .query(
                    &reference.session.host_id,
                    RequestPayload {
                        payload: Some(request_payload::Payload::ReadHistory(read)),
                    },
                )
                .await?;
            let Some(response_payload::Payload::HistoryPage(page)) = response.payload else {
                return Err(CoreError::Security(
                    "unexpected history response payload".into(),
                ));
            };
            if page.session.as_ref() != Some(&whalerein_proto::SessionKey::from(&reference.session))
            {
                return Err(CoreError::Security("history scope mismatch".into()));
            }
            if HistorySource::from_wire(page.source) != Some(HistorySource::Online) {
                return Err(CoreError::Security("history source mismatch".into()));
            }
            if page.projection_id != reference.projection_id {
                return Err(self.projection_reset());
            }
            let batch = page
                .batch
                .as_ref()
                .ok_or_else(|| CoreError::Security("missing history batch".into()))?;
            validate_batch(batch)?;
            if batch.projection_id != reference.projection_id {
                return Err(self.projection_reset());
            }
            let mut matched = None;
            for change in &batch.changes {
                let message = change
                    .message
                    .as_ref()
                    .ok_or_else(|| CoreError::Security("missing history message".into()))?;
                validate_message(message)?;
                if message.message_id == reference.message_id {
                    if matched.is_some() {
                        return Err(CoreError::Security(
                            "history exact read returned duplicate messages".into(),
                        ));
                    }
                    matched = Some(message.clone().into());
                }
            }
            matched.ok_or_else(|| {
                CoreError::NotFound("committed message is no longer available".into())
            })
        }
        .await;
        match &result {
            Ok(_) => self.history_diagnostic("history_page", "succeeded", "none"),
            Err(error) => self.history_diagnostic("history_page", "failed", error.code()),
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{history_message, FakeAccess};
    use crate::api::{ClientConfig, ClientService, Core, MemorySecretStore, PairingDecision};
    use std::sync::Arc;

    async fn paired() -> (tempfile::TempDir, Core, FakeAccess) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let secrets = Arc::new(MemorySecretStore::new());
        let core = Core::open(&path, secrets).unwrap();
        let access = FakeAccess::start().await;
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        (directory, core, access)
    }

    fn key(_core: &Core) -> SessionKey {
        SessionKey::new("host-1", "session-1").unwrap()
    }

    #[tokio::test]
    async fn latest_first_pages_walk_older_history_without_gaps_and_stay_chronological() {
        let (_directory, core, access) = paired().await;
        {
            let mut host = access.host.lock().unwrap();
            host.history_messages = vec![
                history_message("m1", "one"),
                history_message("m2", "two"),
                history_message("m3", "three"),
                history_message("m4", "four"),
                history_message("m5", "five"),
            ];
            host.history_page_size = 2;
        }
        let session = key(&core);
        let mut request = HistoryPageRequest::new(session.clone());
        request.max_messages = 2;
        let first = core.history_page(&request).await.unwrap();
        assert_eq!(first.source, HistorySource::Online);
        assert_eq!(first.projection, "projection-1");
        assert!(!first.complete);
        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["m4", "m5"]
        );
        assert!(first.continuation.is_some());
        let second = core
            .history_page(
                &request
                    .clone()
                    .continuation(first.continuation.clone().unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(
            second
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["m2", "m3"]
        );
        assert!(!second.complete);
        let third = core
            .history_page(
                &request
                    .clone()
                    .continuation(second.continuation.clone().unwrap()),
            )
            .await
            .unwrap();
        assert_eq!(
            third
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["m1"]
        );
        assert!(third.complete);
        assert!(third.continuation.is_none());
        assert_eq!(access.host.lock().unwrap().history_reads, 3);
        assert!(!core.data_dir().join("watch.lock").exists());
    }

    #[tokio::test]
    async fn exact_read_resolves_the_same_canonical_message_as_a_page() {
        let (_directory, core, access) = paired().await;
        access.host.lock().unwrap().history_messages =
            vec![history_message("m1", "one"), history_message("m2", "two")];
        let session = key(&core);
        let page = core
            .history_page(&HistoryPageRequest::new(session.clone()))
            .await
            .unwrap();
        let reference =
            MessageReference::new(session, "m2", "locator:m2", page.projection.clone()).unwrap();
        let message = core.history_message(&reference).await.unwrap();
        assert_eq!(message.message_id, "m2");
        assert_eq!(
            crate::conversation::message_text(&message),
            crate::conversation::message_text(&page.messages[1])
        );
        // The token form is the only form consumers persist.
        let token = reference.to_token();
        let parsed: MessageReference = token.parse().unwrap();
        assert_eq!(parsed.message_id(), "m2");
        assert_eq!(parsed, reference);
    }

    #[tokio::test]
    async fn wrong_scope_tampered_and_replayed_continuations_fail_closed() {
        let (_directory, core, access) = paired().await;
        {
            let mut host = access.host.lock().unwrap();
            host.history_messages = vec![
                history_message("m1", "one"),
                history_message("m2", "two"),
                history_message("m3", "three"),
            ];
            host.history_page_size = 1;
        }
        let session = key(&core);
        let mut request = HistoryPageRequest::new(session.clone());
        request.max_messages = 1;
        let first = core.history_page(&request).await.unwrap();
        let token = first.continuation.unwrap();

        let mut tampered = token.to_string();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'a' { 'b' } else { 'a' });
        let tampered = tampered.parse::<HistoryContinuation>().unwrap();
        let error = core
            .history_page(&request.clone().continuation(tampered))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");

        let other = SessionKey::new("host-1", "session-2").unwrap();
        let mut other_request = HistoryPageRequest::new(other);
        other_request.max_messages = 1;
        let error = core
            .history_page(&other_request.continuation(token.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");

        // A projection change cannot walk the continuation forward.
        access.host.lock().unwrap().history_projection = "projection-2".into();
        let error = core
            .history_page(&request.clone().continuation(token))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");
    }

    #[tokio::test]
    async fn exact_read_of_a_missing_or_stale_reference_is_an_explicit_failure() {
        let (_directory, core, access) = paired().await;
        access.host.lock().unwrap().history_messages.clear();
        let reference =
            MessageReference::new(key(&core), "missing", "locator:missing", "projection-1")
                .unwrap();
        let error = core.history_message(&reference).await.unwrap_err();
        assert_eq!(error.code(), "not_found");
        access.host.lock().unwrap().history_messages = vec![history_message("message-1", "x")];
        let stale = MessageReference::new(
            key(&core),
            "message-1",
            "locator:message-1",
            "projection-old",
        )
        .unwrap();
        let error = core.history_message(&stale).await.unwrap_err();
        assert_eq!(error.code(), "snapshot_required");
    }

    #[tokio::test]
    async fn history_diagnostics_are_stable_redacted_stages() {
        use crate::diagnostics::journal::CapturingSink;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let sink = Arc::new(CapturingSink::default());
        let core = Core::open_with_diagnostic_sink(
            &path,
            Arc::new(MemorySecretStore::new()),
            sink.clone(),
        )
        .unwrap();
        let access = FakeAccess::start().await;
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        {
            let mut host = access.host.lock().unwrap();
            host.history_messages = vec![
                history_message("m1", "HISTORY-BODY-MARKER"),
                history_message("m2", "second"),
                history_message("m3", "third"),
            ];
            host.history_page_size = 1;
        }
        let mut request = HistoryPageRequest::new(key(&core));
        request.max_messages = 1;
        let page = core.history_page(&request).await.unwrap();
        let token = page.continuation.unwrap();
        let mut tampered = token.to_string();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'a' { 'b' } else { 'a' });
        let error = core
            .history_page(&request.continuation(tampered.parse::<HistoryContinuation>().unwrap()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");
        let events = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(events.contains("history_page"));
        assert!(events.contains("history_continuation"));
        assert!(events.contains("invalid_continuation"));
        assert!(events.contains("snapshot_required"));
        assert!(!events.contains("HISTORY-BODY-MARKER"));
        assert!(!events.contains("whr1."));
        assert!(!events.contains("page:"));
        assert!(!events.contains("session-1"));
    }

    /// Isolated test-only comparator (task 6.5): the same seeded history is
    /// read with the pre-change broad page target and with the epoch-5 fitted
    /// first page under the same transport profile and connection. It records
    /// the first-page Host request bytes and initially visible records, and
    /// proves full traversal identity.
    #[tokio::test]
    async fn first_history_comparison_records_the_packing_tradeoff() {
        use crate::api::test_support::history_message;
        let cases = [
            (
                "small",
                vec![
                    history_message("s1", "one"),
                    history_message("s2", "two"),
                    history_message("s3", "three"),
                ],
            ),
            ("mixed", {
                let mut messages = Vec::new();
                for index in 0..6 {
                    messages.push(history_message(&format!("m{index}"), "mixed small record"));
                }
                messages.push(history_message("big", &"y".repeat(12 * 1024)));
                for index in 7..12 {
                    messages.push(history_message(&format!("m{index}"), "mixed small record"));
                }
                messages
            }),
            ("large-leading", {
                let mut messages = vec![history_message("large", &"z".repeat(48 * 1024))];
                for index in 1..5 {
                    messages.push(history_message(&format!("n{index}"), "tail record"));
                }
                messages
            }),
        ];
        let mut summary = Vec::new();
        for (name, messages) in cases {
            let (_directory, core, access) = paired().await;
            access.host.lock().unwrap().history_messages = messages;
            let session = key(&core);
            set_broad_first_page(true);
            let broad = core
                .history_page(&HistoryPageRequest::new(session.clone()))
                .await
                .unwrap();
            set_broad_first_page(false);
            let broad_ids: Vec<String> = broad
                .messages
                .iter()
                .map(|m| m.message_id.clone())
                .collect();
            let fitted = core
                .history_page(&HistoryPageRequest::new(session.clone()))
                .await
                .unwrap();
            let fitted_ids: Vec<String> = fitted
                .messages
                .iter()
                .map(|m| m.message_id.clone())
                .collect();
            assert!(
                broad_ids.ends_with(&fitted_ids),
                "{name}: a fitted page is the newest suffix of the broad page"
            );
            let mut all = Vec::new();
            let mut request = HistoryPageRequest::new(session.clone());
            loop {
                let page = core.history_page(&request).await.unwrap();
                all.extend(page.messages.iter().map(|m| m.message_id.clone()));
                match page.continuation {
                    Some(continuation) => {
                        request = HistoryPageRequest::new(session.clone());
                        request.continuation = Some(continuation);
                    }
                    None => break,
                }
            }
            assert_eq!(
                all, broad_ids,
                "{name}: full traversal keeps every record once"
            );
            let reads = access.host.lock().unwrap().history_request_bytes.clone();
            summary.push(format!(
                "{{\"case\":\"{name}\",\"records\":{},\"broad_records\":{},\"broad_request_bytes\":{},\"fitted_records\":{},\"fitted_request_bytes\":{}}}",
                all.len(),
                broad_ids.len(),
                reads.first().copied().unwrap_or(0),
                fitted_ids.len(),
                reads.get(1).copied().unwrap_or(0),
            ));
        }
        println!("FIRST_HISTORY_COMPARISON {}", summary.join(","));
    }

    #[tokio::test]
    async fn first_visible_page_fits_the_inline_reply_budget_and_continuations_keep_the_caller_bound(
    ) {
        let (_directory, core, access) = paired().await;
        access.host.lock().unwrap().history_messages =
            vec![history_message("m1", "one"), history_message("m2", "two")];
        let session = key(&core);
        let mut first = HistoryPageRequest::new(session.clone());
        first.max_messages = 1;
        first.max_bytes = 64 * 1024;
        let page = core.history_page(&first).await.unwrap();
        let reads = access.host.lock().unwrap().history_request_bytes.clone();
        assert_eq!(reads.len(), 1);
        assert!(
            reads[0] < 64 * 1024,
            "first page is fitted to the MQTT reply budget: {}",
            reads[0]
        );
        assert!(reads[0] >= 1024);
        let continuation = page
            .continuation
            .expect("two messages with max one per page");
        let mut next = HistoryPageRequest::new(session);
        next.continuation = Some(continuation);
        next.max_bytes = 12_345;
        core.history_page(&next).await.unwrap();
        let reads = access.host.lock().unwrap().history_request_bytes.clone();
        assert_eq!(
            reads.last().copied(),
            Some(12_345),
            "a continuation keeps the caller bound"
        );
    }

    #[tokio::test]
    async fn page_budgets_change_packing_not_canonical_content_or_identity() {
        let (_directory, core, access) = paired().await;
        access.host.lock().unwrap().history_messages = vec![
            history_message("m1", "one"),
            history_message("m2", "two"),
            history_message("m3", "three"),
        ];
        let session = key(&core);
        let mut narrow = HistoryPageRequest::new(session.clone());
        narrow.max_messages = 1;
        narrow.max_bytes = 16;
        let narrow = core.history_page(&narrow).await.unwrap();
        assert_eq!(narrow.messages.len(), 1);
        let mut wide = HistoryPageRequest::new(session.clone());
        wide.max_messages = 3;
        wide.max_bytes = 256 * 1024;
        let wide = core.history_page(&wide).await.unwrap();
        assert_eq!(wide.messages.len(), 3);
        assert_eq!(narrow.projection, wide.projection);
        assert_eq!(
            narrow.messages[0].message_id,
            wide.messages.last().unwrap().message_id
        );
        assert_eq!(narrow.messages[0], *wide.messages.last().unwrap());
        let reference = MessageReference::from_committed(
            &session,
            &whalerein_proto::CommittedMessageRef {
                session: Some((&session).into()),
                message_id: narrow.messages[0].message_id.clone(),
                source_locator: "locator".into(),
                projection_id: narrow.projection.clone(),
            },
        )
        .unwrap();
        let exact = core.history_message(&reference).await.unwrap();
        assert_eq!(exact, narrow.messages[0]);
    }

    #[tokio::test]
    async fn an_oversized_cached_message_fails_before_the_cut_advances() {
        let (_directory, core, _access) = paired().await;
        let session = key(&core);
        let cache = core.cache().unwrap();
        cache
            .apply_history_page(&whalerein_proto::HistoryPage {
                session: Some((&session).into()),
                batch: Some(whalerein_proto::HistoryBatch {
                    from_cursor: "start".into(),
                    to_cursor: "cut".into(),
                    batch_id: "batch".into(),
                    projection_id: "cache".into(),
                    changes: vec![whalerein_proto::MessageUpsert {
                        message: Some(history_message("large", &"x".repeat(4096))),
                        origin: None,
                    }],
                }),
                cut_cursor: "cut".into(),
                complete: true,
                snapshot: true,
                continuation: String::new(),
                projection_id: "cache".into(),
                source: HistorySource::Cache.as_wire(),
            })
            .unwrap();
        let mut request = HistoryPageRequest::new(session.clone()).cache();
        request.max_bytes = 1;
        let error = core.history_page(&request).await.unwrap_err();
        assert_eq!(error.code(), "payload_too_large");
        // The failed attempt does not advance a cut or invalidate the view. A
        // later page with enough capacity returns the same canonical message.
        let cache = core.cache().unwrap();
        assert_eq!(
            cache.history_view(&session).unwrap().unwrap().projection,
            "cache"
        );
        let mut retry = request.clone();
        retry.max_bytes = 8 * 1024;
        let page = core.history_page(&retry).await.unwrap();
        assert_eq!(page.messages[0].message_id, "large");
        assert_eq!(
            page.messages[0],
            ConversationMessage::from(history_message("large", &"x".repeat(4096)))
        );
        assert_eq!(
            cache.history_view(&session).unwrap().unwrap().projection,
            "cache"
        );
    }

    #[tokio::test]
    async fn live_commit_and_cached_page_carry_identical_canonical_content() {
        let (_directory, core, _access) = paired().await;
        let session = key(&core);
        let cache = core.cache().unwrap();
        cache
            .apply_history_page(&whalerein_proto::HistoryPage {
                session: Some((&session).into()),
                batch: Some(whalerein_proto::HistoryBatch {
                    from_cursor: "start".into(),
                    to_cursor: "cut-1".into(),
                    batch_id: "snapshot".into(),
                    projection_id: "projection-1".into(),
                    changes: vec![whalerein_proto::MessageUpsert {
                        message: Some(history_message("m1", "one")),
                        origin: None,
                    }],
                }),
                cut_cursor: "cut-1".into(),
                complete: true,
                snapshot: true,
                continuation: String::new(),
                projection_id: "projection-1".into(),
                source: HistorySource::Cache.as_wire(),
            })
            .unwrap();
        let upsert = whalerein_proto::MessageUpsert {
            message: Some(history_message("m2", "two")),
            origin: None,
        };
        let mut conversation = crate::conversation::Conversation::default();
        let live = conversation.committed(&upsert).unwrap();
        let crate::conversation::ViewUpdate::Message {
            message: live_message,
            ..
        } = live
        else {
            panic!("expected a committed live Message")
        };
        cache
            .apply_history_page(&whalerein_proto::HistoryPage {
                session: Some((&session).into()),
                batch: Some(whalerein_proto::HistoryBatch {
                    from_cursor: "cut-1".into(),
                    to_cursor: "cut-2".into(),
                    batch_id: "live".into(),
                    projection_id: "projection-1".into(),
                    changes: vec![upsert],
                }),
                cut_cursor: "cut-2".into(),
                complete: true,
                snapshot: false,
                continuation: String::new(),
                projection_id: "projection-1".into(),
                source: HistorySource::Cache.as_wire(),
            })
            .unwrap();
        let mut request = HistoryPageRequest::new(session).cache();
        request.max_messages = 10;
        let page = core.history_page(&request).await.unwrap();
        assert_eq!(page.projection, "projection-1");
        assert_eq!(page.messages[1], live_message);
    }

    #[tokio::test]
    async fn no_install_returns_a_finite_empty_cache_page_without_touching_the_host() {
        let (_directory, core, access) = paired().await;
        let before = access.host.lock().unwrap().history_reads;
        let request = HistoryPageRequest::new(key(&core)).cache();
        let page = core.history_page(&request).await.unwrap();
        assert_eq!(page.source, HistorySource::Cache);
        assert!(page.complete);
        assert!(page.messages.is_empty());
        assert!(page.activities.is_empty());
        assert!(page.continuation.is_none());
        assert_eq!(access.host.lock().unwrap().history_reads, before);
        assert!(page.projection.is_empty());
    }

    #[tokio::test]
    async fn an_obsolete_cached_projection_is_fenced_explicitly() {
        let (_directory, core, _access) = paired().await;
        let session = key(&core);
        let cache = core.cache().unwrap();
        cache
            .apply_history_page(&whalerein_proto::HistoryPage {
                session: Some((&session).into()),
                batch: Some(whalerein_proto::HistoryBatch {
                    from_cursor: "start".into(),
                    to_cursor: "cut".into(),
                    batch_id: "old".into(),
                    projection_id: "projection-old".into(),
                    changes: vec![whalerein_proto::MessageUpsert {
                        message: Some(history_message("old", "obsolete")),
                        origin: None,
                    }],
                }),
                cut_cursor: "cut".into(),
                complete: true,
                snapshot: true,
                continuation: String::new(),
                projection_id: "projection-old".into(),
                source: HistorySource::Cache.as_wire(),
            })
            .unwrap();
        // Simulate a version-2 row migrated with no projection identity.
        cache.clear_projection_for_test(&session).unwrap();
        let error = core
            .history_page(&HistoryPageRequest::new(session).cache())
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");
    }

    #[tokio::test]
    async fn cache_pages_are_local_do_not_touch_the_host_and_are_fenced_on_replacement() {
        let (_directory, core, access) = paired().await;
        let session = key(&core);
        let cache = core.cache().unwrap();
        cache
            .apply_history_page(&whalerein_proto::HistoryPage {
                session: Some((&session).into()),
                batch: Some(whalerein_proto::HistoryBatch {
                    from_cursor: "start".into(),
                    to_cursor: "cut".into(),
                    batch_id: "batch".into(),
                    projection_id: "cache".into(),
                    changes: vec![
                        whalerein_proto::MessageUpsert {
                            message: Some(history_message("m1", "one")),
                            origin: None,
                        },
                        whalerein_proto::MessageUpsert {
                            message: Some(history_message("m2", "two")),
                            origin: None,
                        },
                        whalerein_proto::MessageUpsert {
                            message: Some(history_message("m3", "three")),
                            origin: None,
                        },
                    ],
                }),
                cut_cursor: "cut".into(),
                complete: true,
                snapshot: true,
                continuation: String::new(),
                projection_id: "cache".into(),
                source: HistorySource::Cache.as_wire(),
            })
            .unwrap();
        let before = access.host.lock().unwrap().history_reads;
        let mut request = HistoryPageRequest::new(session.clone()).cache();
        request.max_messages = 2;
        let page = core.history_page(&request).await.unwrap();
        assert_eq!(page.source, HistorySource::Cache);
        assert_eq!(
            page.messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["m2", "m3"]
        );
        assert_eq!(access.host.lock().unwrap().history_reads, before);
        let continuation = page.continuation.unwrap();
        let older = core
            .history_page(&request.clone().continuation(continuation.clone()))
            .await
            .unwrap();
        assert_eq!(older.messages.len(), 1);
        assert!(older.complete);

        // Replacement invalidates the previous local cut.
        cache
            .apply_history_page(&whalerein_proto::HistoryPage {
                session: Some((&session).into()),
                batch: Some(whalerein_proto::HistoryBatch {
                    from_cursor: "start".into(),
                    to_cursor: "new".into(),
                    batch_id: "replacement".into(),
                    projection_id: "cache".into(),
                    changes: vec![whalerein_proto::MessageUpsert {
                        message: Some(history_message("replacement", "new")),
                        origin: None,
                    }],
                }),
                cut_cursor: "new".into(),
                complete: true,
                snapshot: true,
                continuation: String::new(),
                projection_id: "cache".into(),
                source: HistorySource::Cache.as_wire(),
            })
            .unwrap();
        let error = core
            .history_page(&request.clone().continuation(continuation.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");

        // Eviction/clear also invalidates the install. The empty page is only
        // for a session that never had a cache installation.
        cache.clear().unwrap();
        let error = core
            .history_page(&request.clone().continuation(continuation))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "snapshot_required");
    }
}
