//! Typed, content-free latency observation facts.
//!
//! This module is the Rust adapter of the language-neutral contract in
//! `contracts/latency/observation.json`. Every event is a small typed value;
//! there is no public string map. Producers replace raw business identities
//! with run-local HMAC tokens before building an event, so the public API
//! cannot carry a session, stream, frame, request, watch, connection or object
//! identifier in the clear.
//!
//! Emission is bounded and best effort. A [`BenchContext`] only emits in
//! [`BenchMode::Trace`]; a sink must return promptly, and may drop a record.
//! The outlet that owns the drop count reports overflow; callers never block,
//! retry or fail business delivery because observation is unavailable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Version of the observation envelope and event manifest.
pub const SCHEMA_VERSION: u64 = 1;
/// Manifest name used by the cross-language contract.
pub const MANIFEST_NAME: &str = "whalerein/latency-observation";

/// Field names that must never appear in a fact. They would carry content,
/// credentials, cursors or raw identities.
pub const FORBIDDEN_FIELDS: &[&str] = &[
    "text",
    "payload",
    "body",
    "ciphertext",
    "token",
    "secret",
    "key",
    "cursor",
    "host_id",
    "session_id",
    "device_id",
    "approval",
    "path",
];

/// The process role that owns a boundary. A record may only use an event the
/// manifest assigns to its producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Producer {
    Fixture,
    Dsh,
    Host,
    Core,
    Cli,
    Access,
}

impl Producer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Dsh => "dsh",
            Self::Host => "host",
            Self::Core => "core",
            Self::Cli => "cli",
            Self::Access => "access",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fixture" => Some(Self::Fixture),
            "dsh" => Some(Self::Dsh),
            "host" => Some(Self::Host),
            "core" => Some(Self::Core),
            "cli" => Some(Self::Cli),
            "access" => Some(Self::Access),
            _ => None,
        }
    }
}

/// Capture mode owned by the benchmark composition root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchMode {
    Off,
    Aggregate,
    Trace,
}

impl BenchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Aggregate => "aggregate",
            Self::Trace => "trace",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "aggregate" => Some(Self::Aggregate),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    pub fn captures(self) -> bool {
        self == Self::Trace
    }
}

/// Domain separator for a run-local opaque token. Domains never mix: the same
/// raw identifier has a different token in each domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenDomain {
    Session,
    Stream,
    Block,
    Batch,
    Frame,
    Request,
    Watch,
    Connection,
    Object,
    Ordinal,
}

impl TokenDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Stream => "stream",
            Self::Block => "block",
            Self::Batch => "batch",
            Self::Frame => "frame",
            Self::Request => "request",
            Self::Watch => "watch",
            Self::Connection => "connection",
            Self::Object => "object",
            Self::Ordinal => "ordinal",
        }
    }
}

/// Decode an encoded capture key: even-length hex or standard base64.
pub fn decode_capture_key(value: &str) -> Result<Vec<u8>, BenchViolation> {
    let value = value.trim();
    if value.is_empty() {
        return Err(BenchViolation::InvalidEnvelope("capture key is empty"));
    }
    let bytes = if value.len() % 2 == 0 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        hex::decode(value).map_err(|_| BenchViolation::InvalidEnvelope("capture key hex"))?
    } else {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|_| BenchViolation::InvalidEnvelope("capture key base64"))?
    };
    if bytes.is_empty() {
        return Err(BenchViolation::InvalidEnvelope("capture key is empty"));
    }
    Ok(bytes)
}

/// HMAC-SHA256 tokenizer. The capture key is ephemeral and never leaves the
/// fixture/process composition root.
#[derive(Clone)]
pub struct BenchTokenizer {
    key: Arc<[u8]>,
}

impl BenchTokenizer {
    pub fn new(key: &[u8]) -> Self {
        Self {
            key: Arc::from(key.to_vec().into_boxed_slice()),
        }
    }

    /// Decode a capture key supplied as lowercase/uppercase hex or standard
    /// base64. Empty or malformed keys are rejected so a capture cannot
    /// accidentally fall back to a predictable token.
    pub fn from_encoded_key(value: &str) -> Result<Self, BenchViolation> {
        Ok(Self::new(&decode_capture_key(value)?))
    }

    /// Opaque, stable token: `hex(HMAC-SHA256(key, domain || 0 || raw)[..16])`.
    pub fn token(&self, domain: TokenDomain, raw: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key");
        mac.update(b"whalerein/latency-token/v1");
        mac.update(&[0]);
        mac.update(domain.as_str().as_bytes());
        mac.update(&[0]);
        mac.update(raw.as_bytes());
        let digest = mac.finalize().into_bytes();
        hex::encode(&digest[..16])
    }
}

/// A monotonic nanosecond clock. Production uses the Linux kernel
/// `CLOCK_MONOTONIC`; the non-Linux implementation is a documented
/// process-relative fallback for tests and development. Bench capture is
/// rejected on non-Linux because the contract requires one kernel-boot epoch.
pub trait BenchClock: Send + Sync {
    fn now_ns(&self) -> u64;
}

/// Kernel-clock reader.
#[derive(Debug, Default)]
pub struct KernelMonotonicClock;

impl BenchClock for KernelMonotonicClock {
    fn now_ns(&self) -> u64 {
        kernel_monotonic_ns()
    }
}

#[cfg(target_os = "linux")]
fn kernel_monotonic_ns() -> u64 {
    let mut reading = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `reading` is a valid `timespec` and CLOCK_MONOTONIC is a valid
    // clock id. A failed read cannot be represented; report 0 so a duration is
    // unavailable rather than invented.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut reading) };
    if status != 0 || reading.tv_sec < 0 {
        return 0;
    }
    (reading.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(reading.tv_nsec as u64)
}

/// Portable fallback: process-relative nanoseconds since the first read.
/// This is deliberately not a kernel-boot epoch; bench capture is rejected on
/// platforms without `CLOCK_MONOTONIC`.
#[cfg(not(target_os = "linux"))]
fn kernel_monotonic_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// Deterministic clock for contract tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    now_ns: AtomicU64,
}

impl ManualClock {
    pub fn new(now_ns: u64) -> Self {
        Self {
            now_ns: AtomicU64::new(now_ns),
        }
    }

    pub fn advance(&self, delta_ns: u64) {
        self.now_ns.fetch_add(delta_ns, Ordering::SeqCst);
    }

    pub fn set(&self, now_ns: u64) {
        self.now_ns.store(now_ns, Ordering::SeqCst);
    }
}

impl BenchClock for ManualClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.load(Ordering::SeqCst)
    }
}

/// Receives complete typed records. Implementations must return promptly and
/// must not panic; a full, stalled or failed outlet drops the record and owns
/// the loss counter.
pub trait BenchSink: Send + Sync {
    fn emit(&self, record: BenchRecord);
}

/// Drops every record. Used by `off`, `aggregate` and tests that only exercise
/// emission semantics.
#[derive(Debug, Default)]
pub struct NullSink;

impl BenchSink for NullSink {
    fn emit(&self, _record: BenchRecord) {}
}

/// Test sink that retains accepted records.
#[derive(Debug, Default)]
pub struct CapturingSink {
    records: std::sync::Mutex<Vec<BenchRecord>>,
}

impl CapturingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> Vec<BenchRecord> {
        self.records.lock().expect("capturing sink mutex").clone()
    }

    pub fn events(&self) -> Vec<BenchEvent> {
        self.records()
            .into_iter()
            .map(|record| record.event)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.records.lock().expect("capturing sink mutex").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl BenchSink for CapturingSink {
    fn emit(&self, record: BenchRecord) {
        self.records
            .lock()
            .expect("capturing sink mutex")
            .push(record);
    }
}

struct BenchInner {
    run_id: String,
    lifetime: String,
    mode: BenchMode,
    tokenizer: BenchTokenizer,
    sink: Arc<dyn BenchSink>,
    clock: Arc<dyn BenchClock>,
    seq: AtomicU64,
    write_ordinal: AtomicU64,
}

/// Shared observation context for one process lifetime.
///
/// `run_id`, `lifetime`, tokenizer, sink, clock and sequence counter are shared
/// by every producer role in the process. [`BenchContext::for_producer`] keeps
/// the same outlet and lifetime while changing only the role stamped on each
/// record, which is how Core and CLI facts join one process.
#[derive(Clone)]
pub struct BenchContext {
    inner: Arc<BenchInner>,
    producer: Producer,
}

impl BenchContext {
    pub fn new(
        run_id: impl Into<String>,
        lifetime: impl Into<String>,
        producer: Producer,
        mode: BenchMode,
        capture_key: &[u8],
        sink: Arc<dyn BenchSink>,
    ) -> Arc<Self> {
        Self::with_clock(
            run_id,
            lifetime,
            producer,
            mode,
            capture_key,
            sink,
            Arc::new(KernelMonotonicClock),
        )
    }

    pub fn with_clock(
        run_id: impl Into<String>,
        lifetime: impl Into<String>,
        producer: Producer,
        mode: BenchMode,
        capture_key: &[u8],
        sink: Arc<dyn BenchSink>,
        clock: Arc<dyn BenchClock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(BenchInner {
                run_id: run_id.into(),
                lifetime: lifetime.into(),
                mode,
                tokenizer: BenchTokenizer::new(capture_key),
                sink,
                clock,
                seq: AtomicU64::new(0),
                write_ordinal: AtomicU64::new(0),
            }),
            producer,
        })
    }

    /// A context that never emits. The disabled path allocates no writer and
    /// no platform clock dependency.
    pub fn disabled(producer: Producer) -> Arc<Self> {
        Self::with_clock(
            String::new(),
            String::new(),
            producer,
            BenchMode::Off,
            b"disabled",
            Arc::new(NullSink),
            Arc::new(ManualClock::default()),
        )
    }

    pub fn mode(&self) -> BenchMode {
        self.inner.mode
    }

    pub fn enabled(&self) -> bool {
        self.inner.mode == BenchMode::Trace
    }

    pub fn producer(&self) -> Producer {
        self.producer
    }

    pub fn run_id(&self) -> &str {
        &self.inner.run_id
    }

    pub fn lifetime(&self) -> &str {
        &self.inner.lifetime
    }

    pub fn tokenizer(&self) -> &BenchTokenizer {
        &self.inner.tokenizer
    }

    pub fn token(&self, domain: TokenDomain, raw: &str) -> String {
        if !self.enabled() {
            return String::new();
        }
        self.inner.tokenizer.token(domain, raw)
    }

    /// Current boundary reading, or 0 when the clock degraded. Never panics.
    pub fn now_ns(&self) -> u64 {
        self.inner.clock.now_ns()
    }

    /// Next process-lifetime write ordinal. Shared by every CLI write stamp.
    pub fn next_write_ordinal(&self) -> u64 {
        self.inner.write_ordinal.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Same outlet, lifetime, clock and sequence stream with a different
    /// producer role.
    pub fn for_producer(&self, producer: Producer) -> Arc<Self> {
        Arc::new(Self {
            inner: self.inner.clone(),
            producer,
        })
    }

    /// Emit one fact at the current boundary. A wrong-producer, `off` or
    /// `aggregate` context silently drops the fact. Never panics or blocks.
    pub fn emit(&self, event: BenchEvent) {
        if !self.enabled() || !event.producers().contains(&self.producer) {
            return;
        }
        let ts = self.now_ns();
        self.emit_at(event, ts);
    }

    /// Emit a fact whose boundary timestamp was captured earlier (for example
    /// a milestone recorded before the Watch identity was known).
    pub fn emit_at(&self, event: BenchEvent, ts: u64) {
        if !self.enabled() || !event.producers().contains(&self.producer) {
            return;
        }
        let seq = self.inner.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.sink.emit(BenchRecord {
            v: SCHEMA_VERSION,
            run: self.inner.run_id.clone(),
            producer: self.producer,
            lifetime: self.inner.lifetime.clone(),
            seq,
            ts,
            event,
        });
    }
}

/// One emitted fact with its common header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchRecord {
    pub v: u64,
    pub run: String,
    pub producer: Producer,
    pub lifetime: String,
    pub seq: u64,
    pub ts: u64,
    pub event: BenchEvent,
}

#[derive(Serialize)]
struct WireRecord<'a> {
    v: u64,
    run: &'a str,
    producer: Producer,
    lifetime: &'a str,
    seq: u64,
    ts: u64,
    #[serde(flatten)]
    event: &'a BenchEvent,
}

impl BenchRecord {
    /// NDJSON line body without the trailing newline. Serialization of a
    /// validated typed value cannot fail; a failure yields an empty buffer
    /// rather than a panic.
    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(&WireRecord {
            v: self.v,
            run: &self.run,
            producer: self.producer,
            lifetime: &self.lifetime,
            seq: self.seq,
            ts: self.ts,
            event: &self.event,
        })
        .unwrap_or_default()
    }

    pub fn to_json(&self) -> Value {
        serde_json::from_slice(&self.to_vec()).unwrap_or(Value::Null)
    }

    /// Validate the header, producer role and typed data of this record.
    pub fn validate(&self) -> Result<(), BenchViolation> {
        if self.v != SCHEMA_VERSION {
            return Err(BenchViolation::InvalidEnvelope("schema version"));
        }
        if self.run.is_empty() || self.lifetime.is_empty() {
            return Err(BenchViolation::InvalidEnvelope("run or lifetime"));
        }
        if !self.event.producers().contains(&self.producer) {
            return Err(BenchViolation::WrongProducer {
                event: self.event.name().to_owned(),
                producer: self.producer.as_str().to_owned(),
            });
        }
        Ok(())
    }
}

/// Reasons a record is rejected by the manifest-shaped validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchViolation {
    /// The envelope is missing, malformed or exceeds the schema.
    InvalidEnvelope(&'static str),
    /// A timestamp before the shared clock origin is never valid.
    NegativeTimestamp,
    /// Event name is not in the manifest.
    UnknownEvent(String),
    /// The event is owned by a different producer role.
    WrongProducer { event: String, producer: String },
    /// A content or identity field forbidden by the contract.
    ForbiddenField(String),
    /// A data object failed typed, enum or required-field validation.
    InvalidData(String),
}

impl std::fmt::Display for BenchViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEnvelope(detail) => {
                write!(formatter, "invalid observation envelope: {detail}")
            }
            Self::NegativeTimestamp => write!(formatter, "negative observation timestamp"),
            Self::UnknownEvent(event) => write!(formatter, "unknown observation event: {event}"),
            Self::WrongProducer { event, producer } => {
                write!(formatter, "producer {producer} cannot emit {event}")
            }
            Self::ForbiddenField(field) => {
                write!(formatter, "forbidden observation field: {field}")
            }
            Self::InvalidData(detail) => write!(formatter, "invalid observation data: {detail}"),
        }
    }
}

impl std::error::Error for BenchViolation {}

/// Validate one JSON record against the manifest and return its typed form.
pub fn validate_record(value: &Value) -> Result<BenchRecord, BenchViolation> {
    let object = value
        .as_object()
        .ok_or(BenchViolation::InvalidEnvelope("record is not an object"))?;
    const HEADER: &[&str] = &[
        "v", "run", "producer", "lifetime", "seq", "ts", "event", "data",
    ];
    if object.len() != HEADER.len() || !HEADER.iter().all(|key| object.contains_key(*key)) {
        return Err(BenchViolation::InvalidEnvelope(
            "header must contain exactly v, run, producer, lifetime, seq, ts, event, data",
        ));
    }
    let v = object
        .get("v")
        .and_then(Value::as_u64)
        .ok_or(BenchViolation::InvalidEnvelope("v is not an integer"))?;
    if v != SCHEMA_VERSION {
        return Err(BenchViolation::InvalidEnvelope(
            "unsupported schema version",
        ));
    }
    let run = object
        .get("run")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(BenchViolation::InvalidEnvelope("run is missing"))?;
    let lifetime = object
        .get("lifetime")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(BenchViolation::InvalidEnvelope("lifetime is missing"))?;
    let producer = object
        .get("producer")
        .and_then(Value::as_str)
        .and_then(Producer::parse)
        .ok_or(BenchViolation::InvalidEnvelope(
            "producer is not a manifest role",
        ))?;
    let seq = object
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or(BenchViolation::InvalidEnvelope(
            "seq is not an unsigned integer",
        ))?;
    let ts = match object.get("ts") {
        Some(Value::Number(number)) if number.as_i64().is_some_and(|value| value < 0) => {
            return Err(BenchViolation::NegativeTimestamp)
        }
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or(BenchViolation::InvalidEnvelope("ts is not an integer"))?,
        _ => return Err(BenchViolation::InvalidEnvelope("ts is not an integer")),
    };
    let event_name = object
        .get("event")
        .and_then(Value::as_str)
        .ok_or(BenchViolation::InvalidEnvelope("event is missing"))?;
    let event_spec = event_spec(event_name)
        .ok_or_else(|| BenchViolation::UnknownEvent(event_name.to_owned()))?;
    if !event_spec.contains(&producer) {
        return Err(BenchViolation::WrongProducer {
            event: event_name.to_owned(),
            producer: producer.as_str().to_owned(),
        });
    }
    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or(BenchViolation::InvalidEnvelope("data is not an object"))?;
    for key in data.keys() {
        if FORBIDDEN_FIELDS.contains(&key.as_str()) {
            return Err(BenchViolation::ForbiddenField(key.clone()));
        }
    }
    let mut wire = Map::new();
    wire.insert("event".to_owned(), Value::String(event_name.to_owned()));
    wire.insert("data".to_owned(), Value::Object(data.clone()));
    let event: BenchEvent = serde_json::from_value(Value::Object(wire))
        .map_err(|error| BenchViolation::InvalidData(error.to_string()))?;
    if event.name() != event_name {
        return Err(BenchViolation::InvalidData(
            "event name and data shape disagree".into(),
        ));
    }
    // Serde ignores unknown struct fields by default. Compare the accepted
    // typed data back to the input so an unlisted field cannot ride through,
    // including a forbidden content field.
    let typed = serde_json::to_value(&event)
        .map_err(|_| BenchViolation::InvalidData("typed event encoding".into()))?;
    let typed_data = typed
        .get("data")
        .and_then(Value::as_object)
        .ok_or(BenchViolation::InvalidData("typed event data".into()))?;
    for key in data.keys() {
        if !typed_data.contains_key(key) {
            return Err(BenchViolation::InvalidData(format!(
                "unknown field `{key}`"
            )));
        }
    }
    let record = BenchRecord {
        v,
        run: run.to_owned(),
        producer,
        lifetime: lifetime.to_owned(),
        seq,
        ts,
        event,
    };
    record.validate()?;
    Ok(record)
}

/// Check the baked contract against the checked-in manifest. The adapter tests
/// use it so a schema change cannot silently leave Rust behind.
pub fn validate_manifest(manifest: &Value) -> Result<(), BenchViolation> {
    if manifest.get("schema").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return Err(BenchViolation::InvalidEnvelope("manifest schema"));
    }
    if manifest.get("name").and_then(Value::as_str) != Some(MANIFEST_NAME) {
        return Err(BenchViolation::InvalidEnvelope("manifest name"));
    }
    let producers = manifest
        .get("producers")
        .and_then(Value::as_array)
        .ok_or(BenchViolation::InvalidEnvelope("manifest producers"))?;
    let manifest_producers: Vec<&str> = producers
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for producer in [
        Producer::Fixture,
        Producer::Dsh,
        Producer::Host,
        Producer::Core,
        Producer::Cli,
        Producer::Access,
    ] {
        if !manifest_producers.contains(&producer.as_str()) {
            return Err(BenchViolation::InvalidEnvelope("manifest producer role"));
        }
    }
    let events = manifest
        .get("events")
        .and_then(Value::as_object)
        .ok_or(BenchViolation::InvalidEnvelope("manifest events"))?;
    if events.len() != EVENT_NAMES.len() {
        return Err(BenchViolation::InvalidEnvelope(
            "manifest event count differs from the Rust adapter",
        ));
    }
    for name in EVENT_NAMES {
        let entry = events
            .get(*name)
            .ok_or(BenchViolation::UnknownEvent((*name).to_owned()))?;
        let expected = event_spec(name).expect("event spec");
        let listed = entry
            .get("producers")
            .and_then(Value::as_array)
            .ok_or(BenchViolation::InvalidEnvelope("event producers"))?;
        let listed: Vec<&str> = listed.iter().filter_map(Value::as_str).collect();
        if listed.len() != expected.len()
            || expected
                .iter()
                .any(|producer| !listed.contains(&producer.as_str()))
        {
            return Err(BenchViolation::WrongProducer {
                event: (*name).to_owned(),
                producer: "manifest".into(),
            });
        }
        if entry
            .get("fields")
            .and_then(Value::as_object)
            .is_none_or(Map::is_empty)
        {
            return Err(BenchViolation::InvalidEnvelope("event fields"));
        }
    }
    let forbidden = manifest
        .get("forbidden_fields")
        .and_then(Value::as_array)
        .ok_or(BenchViolation::InvalidEnvelope("forbidden fields"))?;
    let forbidden: Vec<&str> = forbidden.iter().filter_map(Value::as_str).collect();
    if forbidden.len() != FORBIDDEN_FIELDS.len()
        || FORBIDDEN_FIELDS
            .iter()
            .any(|field| !forbidden.contains(field))
    {
        return Err(BenchViolation::InvalidEnvelope("forbidden field policy"));
    }
    Ok(())
}

/// Validate every sample vector: all `valid` records are accepted and all
/// `invalid` records are rejected.
pub fn validate_vectors(vectors: &Value) -> Result<(), BenchViolation> {
    let valid = vectors
        .get("valid")
        .and_then(Value::as_array)
        .ok_or(BenchViolation::InvalidEnvelope("valid vectors"))?;
    let invalid = vectors
        .get("invalid")
        .and_then(Value::as_array)
        .ok_or(BenchViolation::InvalidEnvelope("invalid vectors"))?;
    for vector in valid {
        validate_record(vector)?;
    }
    for vector in invalid {
        let value = vector.get("value").unwrap_or(vector);
        if validate_record(value).is_ok() {
            return Err(BenchViolation::InvalidEnvelope(
                "an invalid sample vector was accepted",
            ));
        }
    }
    Ok(())
}

/// Every event name in the manifest. Kept in sync by `validate_manifest`.
pub const EVENT_NAMES: &[&str] = &[
    "fixture_clock_check",
    "fixture_slot",
    "fixture_socket_sample",
    "fixture_observe",
    "fixture_emit",
    "fixture_recovery",
    "dsh_callback",
    "dsh_handoff",
    "host_enqueue",
    "host_coalesce",
    "host_dequeue",
    "host_publish",
    "host_command",
    "host_subscribe",
    "core_receipt",
    "core_enqueue",
    "core_dequeue",
    "core_delivery",
    "core_join",
    "core_sync",
    "core_command",
    "cli_write",
    "cli_request",
    "access_query",
    "access_command",
    "access_relay",
    "access_notice",
    "access_transport",
];

fn event_spec(name: &str) -> Option<&'static [Producer]> {
    use Producer::*;
    Some(match name {
        "fixture_clock_check"
        | "fixture_slot"
        | "fixture_socket_sample"
        | "fixture_observe"
        | "fixture_emit"
        | "fixture_recovery" => &[Fixture],
        "dsh_callback" | "dsh_handoff" => &[Dsh],
        "host_enqueue" | "host_coalesce" | "host_dequeue" | "host_publish" | "host_command"
        | "host_subscribe" => &[Host],
        "core_receipt" | "core_enqueue" | "core_dequeue" | "core_delivery" | "core_join"
        | "core_sync" | "core_command" => &[Core],
        "cli_write" => &[Core, Cli],
        "cli_request" => &[Cli],
        "access_query" | "access_command" | "access_relay" | "access_notice"
        | "access_transport" => &[Access],
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Typed events
// ---------------------------------------------------------------------------

/// One typed, content-free boundary fact. Serialized as
/// `{"event": "<name>", "data": { ... }}`; no public string map is exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum BenchEvent {
    #[serde(rename = "fixture_clock_check")]
    FixtureClockCheck {
        producer: Producer,
        reading_ns: u64,
        before_ns: u64,
        after_ns: u64,
        clock_source: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "fixture_slot")]
    FixtureSlot {
        slot: u64,
        phase: FixturePhase,
        arm: FixtureArm,
        state: FixtureSlotState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pair: Option<String>,
    },
    #[serde(rename = "fixture_socket_sample")]
    FixtureSocketSample {
        role: SocketRole,
        generation: u64,
        state: SocketSampleState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rtt_us: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retransmits: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap_ms: Option<u64>,
    },
    #[serde(rename = "fixture_observe")]
    FixtureObserve {
        attempt: String,
        ordinal: u64,
        kind: ObserveKind,
        bytes: u64,
        prefix_bytes: u64,
        correct: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
    },
    #[serde(rename = "fixture_emit")]
    FixtureEmit {
        stream: String,
        attempt: String,
        block: String,
        seq_start: u64,
        seq_end: u64,
        bytes: u64,
    },
    #[serde(rename = "fixture_recovery")]
    FixtureRecovery {
        attempt: String,
        milestone: RecoveryMilestone,
        state: MilestoneState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generation: Option<u64>,
    },
    #[serde(rename = "dsh_callback")]
    DshCallback {
        stream: String,
        attempt: String,
        block: String,
        seq_start: u64,
        seq_end: u64,
        bytes: u64,
        native: NativeKind,
    },
    #[serde(rename = "dsh_handoff")]
    DshHandoff {
        stream: String,
        attempt: String,
        block: String,
        seq_start: u64,
        seq_end: u64,
        bytes: u64,
    },
    #[serde(rename = "host_enqueue")]
    HostEnqueue {
        stream: String,
        block: String,
        seq_start: u64,
        seq_end: u64,
        bytes: u64,
        queue_depth: u64,
        queue_bytes: u64,
    },
    #[serde(rename = "host_coalesce")]
    HostCoalesce {
        stream: String,
        block: String,
        seq_start: u64,
        seq_end: u64,
        frames: u64,
        bytes: u64,
    },
    #[serde(rename = "host_dequeue")]
    HostDequeue {
        stream: String,
        block: String,
        seq_start: u64,
        seq_end: u64,
        bytes: u64,
        wait_ns: u64,
    },
    #[serde(rename = "host_publish")]
    HostPublish {
        request: String,
        attempt: u64,
        kind: PublishKind,
        bytes: u64,
        outcome: PublishOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_end: Option<u64>,
    },
    #[serde(rename = "host_command")]
    HostCommand {
        request: String,
        attempt: u64,
        stage: CommandStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CommandOutcome>,
    },
    #[serde(rename = "host_subscribe")]
    HostSubscribe {
        connection: String,
        generation: u64,
        outcome: SubscribeOutcome,
    },
    #[serde(rename = "core_receipt")]
    CoreReceipt {
        frame: String,
        kind: ReceiptKind,
        bytes: u64,
        decode: Decode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_end: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "core_enqueue")]
    CoreEnqueue {
        frame: String,
        queue_depth: u64,
        queue_bytes: u64,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_end: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
    },
    #[serde(rename = "core_dequeue")]
    CoreDequeue {
        frame: String,
        wait_ns: u64,
        disposition: DequeueDisposition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_end: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
    },
    #[serde(rename = "core_delivery")]
    CoreDelivery {
        frame: String,
        delivery: u64,
        kind: DeliveryKind,
        bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_end: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
    },
    #[serde(rename = "core_join")]
    CoreJoin {
        watch: String,
        connection: String,
        generation: u64,
        milestone: JoinMilestone,
    },
    #[serde(rename = "core_sync")]
    CoreSync {
        request: String,
        page: u64,
        messages: u64,
        bytes: u64,
        verified: bool,
    },
    #[serde(rename = "core_command")]
    CoreCommand {
        request: String,
        attempt: u64,
        stage: CommandStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CommandOutcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "cli_write")]
    CliWrite {
        delivery: u64,
        ordinal: u64,
        bytes: u64,
        outcome: WriteOutcome,
        flush: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_start: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq_end: Option<u64>,
    },
    #[serde(rename = "cli_request")]
    CliRequest {
        request: String,
        attempt: u64,
        kind: RequestKind,
        stage: CliRequestStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CommandOutcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    #[serde(rename = "access_query")]
    AccessQuery {
        request: String,
        attempt: u64,
        route: AccessRoute,
        stage: AccessQueryStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CommandOutcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
    },
    #[serde(rename = "access_command")]
    AccessCommand {
        request: String,
        attempt: u64,
        stage: CommandStage,
        durable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CommandOutcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
    },
    #[serde(rename = "access_relay")]
    AccessRelay {
        request: String,
        object_op: String,
        stage: RelayStage,
        route: AccessRoute,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<CommandOutcome>,
    },
    #[serde(rename = "access_notice")]
    AccessNotice {
        connection: String,
        generation: u64,
        role: SocketRole,
        stage: AccessNoticeStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<SubscribeOutcome>,
    },
    #[serde(rename = "access_transport")]
    AccessTransport {
        connection: String,
        generation: u64,
        role: SocketRole,
        direction: TransportDirection,
        bytes: u64,
        outcome: TransportOutcome,
    },
}

impl BenchEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::FixtureClockCheck { .. } => "fixture_clock_check",
            Self::FixtureSlot { .. } => "fixture_slot",
            Self::FixtureSocketSample { .. } => "fixture_socket_sample",
            Self::FixtureObserve { .. } => "fixture_observe",
            Self::FixtureEmit { .. } => "fixture_emit",
            Self::FixtureRecovery { .. } => "fixture_recovery",
            Self::DshCallback { .. } => "dsh_callback",
            Self::DshHandoff { .. } => "dsh_handoff",
            Self::HostEnqueue { .. } => "host_enqueue",
            Self::HostCoalesce { .. } => "host_coalesce",
            Self::HostDequeue { .. } => "host_dequeue",
            Self::HostPublish { .. } => "host_publish",
            Self::HostCommand { .. } => "host_command",
            Self::HostSubscribe { .. } => "host_subscribe",
            Self::CoreReceipt { .. } => "core_receipt",
            Self::CoreEnqueue { .. } => "core_enqueue",
            Self::CoreDequeue { .. } => "core_dequeue",
            Self::CoreDelivery { .. } => "core_delivery",
            Self::CoreJoin { .. } => "core_join",
            Self::CoreSync { .. } => "core_sync",
            Self::CoreCommand { .. } => "core_command",
            Self::CliWrite { .. } => "cli_write",
            Self::CliRequest { .. } => "cli_request",
            Self::AccessQuery { .. } => "access_query",
            Self::AccessCommand { .. } => "access_command",
            Self::AccessRelay { .. } => "access_relay",
            Self::AccessNotice { .. } => "access_notice",
            Self::AccessTransport { .. } => "access_transport",
        }
    }

    /// Producer roles allowed to emit this event.
    pub fn producers(&self) -> &'static [Producer] {
        event_spec(self.name()).expect("every typed event has a spec")
    }
}

// Shared enums. Names match the checked-in manifest exactly.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixturePhase {
    Warmup,
    Measured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureArm {
    Idle,
    Bulk,
    Watch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureSlotState {
    Planned,
    Started,
    MeasurementEntered,
    Finished,
    SetupFailed,
    Interrupted,
    NotStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketRole {
    Host,
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketSampleState {
    Sampled,
    Idle,
    Unsupported,
    Replaced,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveKind {
    Preview,
    Replace,
    Reset,
    Terminal,
    Duplicate,
    Hidden,
    Integrity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryMilestone {
    BlackoutStart,
    LinkRestored,
    Subscribed,
    ChannelReady,
    WatchReady,
    FirstUpdate,
    FirstVisible,
    CaughtUp,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneState {
    Observed,
    Missed,
    Inapplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeKind {
    AssistantPreview,
    AssistantMessage,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishKind {
    Result,
    Event,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishOutcome {
    Accepted,
    Failed,
    Timeout,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStage {
    Admit,
    Receipt,
    Recover,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutcome {
    Ok,
    Error,
    Expired,
    Timeout,
    Late,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscribeOutcome {
    Subscribed,
    Replaced,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    Event,
    Result,
    History,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decode {
    Ok,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DequeueDisposition {
    Applied,
    Duplicate,
    Stale,
    WaitingGap,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Preview,
    Message,
    AttemptEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinMilestone {
    Subscribed,
    ChannelReady,
    WatchReady,
    FirstUpdate,
    FirstVisiblePrefix,
    CaughtUp,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Success,
    Partial,
    Failed,
    Reversed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Query,
    Command,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliRequestStage {
    Invoke,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRoute {
    Inline,
    Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessQueryStage {
    Admit,
    Forward,
    Reply,
    Expire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayStage {
    Stage,
    Upload,
    Finish,
    Fetch,
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessNoticeStage {
    Subscribe,
    Ready,
    Disconnect,
}

/// Direction of one Access transport delivery. The direct live route carries
/// no Access application stage, so its transit is named by endpoint role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportDirection {
    HostToBroker,
    BrokerToClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportOutcome {
    Ok,
    Failed,
    Dropped,
}

/// Tokenized stream/block/sequence facts of one decrypted live frame.
///
/// This is the identity a per-increment waterfall joins on: the same source
/// range reaches the native callback, Host queue/publish, Core receipt and
/// delivery, and the CLI write stamp. All identities are run-local HMAC
/// tokens; the batch token is derived from the raw
/// `<stream>:<seq_start>:<seq_end>` range so a coalesced or split frame keeps
/// one stable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRange {
    pub stream: String,
    /// Absent when the frame names no block (for example a started or ended
    /// attempt).
    pub block: Option<String>,
    pub seq_start: u64,
    pub seq_end: u64,
    pub batch: String,
}

impl FrameRange {
    /// Derive range facts from a decrypted session update. Only a live frame
    /// with a usable attempt identity and an inclusive sequence range yields
    /// facts; a result, history or command frame stays absent rather than
    /// inventing one.
    pub fn from_update(
        bench: &BenchContext,
        update: &whalerein_proto::SessionUpdate,
    ) -> Option<Self> {
        match update.payload.as_ref()? {
            whalerein_proto::session_update::Payload::Live(frame) => Self::from_frame(bench, frame),
            _ => None,
        }
    }

    /// Derive range facts from one decrypted live frame. A disabled context
    /// yields nothing, so `off` and `aggregate` never pay for a token.
    pub fn from_frame(bench: &BenchContext, frame: &whalerein_proto::LiveFrame) -> Option<Self> {
        if !bench.enabled() {
            return None;
        }
        let attempt = frame.attempt.as_ref()?;
        // `first_seq == 0` and a reversed range are not a usable increment:
        // Core rejects such a frame, so a fact must not claim one either.
        if attempt.attempt_id.is_empty() || frame.first_seq == 0 || frame.last_seq < frame.first_seq
        {
            return None;
        }
        Some(Self {
            stream: bench.token(TokenDomain::Stream, &attempt.attempt_id),
            block: (!frame.block_id.is_empty())
                .then(|| bench.token(TokenDomain::Block, &frame.block_id)),
            seq_start: frame.first_seq,
            seq_end: frame.last_seq,
            batch: bench.token(
                TokenDomain::Batch,
                &format!(
                    "{}:{}:{}",
                    attempt.attempt_id, frame.first_seq, frame.last_seq
                ),
            ),
        })
    }
}

/// Stable opaque frame identity derived from an authenticated event envelope.
/// The raw channel/nonce pair never leaves this function.
pub(crate) fn event_frame_identity(envelope: &whalerein_proto::EventEnvelope) -> String {
    format!(
        "{}:{}:{}:{}",
        envelope.host_id,
        envelope.device_id,
        envelope.channel_id,
        hex::encode(&envelope.nonce)
    )
}

#[cfg(test)]
mod tests;
