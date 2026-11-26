//! Process bench outlet for CLI and Core facts.
//!
//! The composition root enables capture only for
//! `WHALEREIN_BENCH_MODE=trace` with `WHALEREIN_BENCH_OUTLET` set. `off`,
//! `aggregate` and unset configurations create no writer. Records are appended
//! as NDJSON by one background writer over a bounded channel; overflow and
//! byte-cap drops are counted here, never by callers.
//!
//! Capture requires Linux `CLOCK_MONOTONIC` because all timed producers must
//! share one kernel-boot epoch. Other platforms keep the disabled path.

use std::env;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use whalerein_core::api::bench::{
    BenchContext, BenchEvent, BenchMode, BenchRecord, BenchSink, FrameRange, Producer, WriteOutcome,
};

pub(crate) const MODE_ENV: &str = "WHALEREIN_BENCH_MODE";
pub(crate) const OUTLET_ENV: &str = "WHALEREIN_BENCH_OUTLET";
pub(crate) const RUN_ID_ENV: &str = "WHALEREIN_BENCH_RUN_ID";
pub(crate) const CAPTURE_KEY_ENV: &str = "WHALEREIN_BENCH_CAPTURE_KEY";
pub(crate) const LIFETIME_ENV: &str = "WHALEREIN_BENCH_LIFETIME";
pub(crate) const CAPACITY_ENV: &str = "WHALEREIN_BENCH_CAPACITY";
pub(crate) const BYTE_CAP_ENV: &str = "WHALEREIN_BENCH_BYTE_CAP";

pub(crate) const DEFAULT_CAPACITY: usize = 4096;
pub(crate) const DEFAULT_BYTE_CAP: u64 = 32 * 1024 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

enum OutletMessage {
    // One record now carries the range fields of a delivered increment; a box
    // keeps the bounded channel slots small instead of one large enum.
    Record(Box<BenchRecord>),
    Shutdown(SyncSender<()>),
}

struct OutletState {
    dropped: AtomicU64,
    written_bytes: AtomicU64,
    failed: AtomicBool,
}

/// Bounded NDJSON file sink. `emit` never blocks: a full channel, a byte-cap
/// or a failed writer increments the drop counter and returns.
pub(crate) struct FileOutlet {
    sender: SyncSender<OutletMessage>,
    state: Arc<OutletState>,
    byte_cap: u64,
    path: PathBuf,
    writer: Mutex<Option<JoinHandle<()>>>,
}

#[allow(dead_code)]
impl FileOutlet {
    fn start(path: &Path, capacity: usize, byte_cap: u64) -> std::io::Result<Arc<Self>> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        let state = Arc::new(OutletState {
            dropped: AtomicU64::new(0),
            written_bytes: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        let (sender, receiver) = mpsc::sync_channel(capacity.max(1));
        let writer_state = state.clone();
        let writer = thread::Builder::new()
            .name("whalerein-bench-outlet".into())
            .spawn(move || write_records(file, receiver, writer_state, byte_cap))
            .map_err(|_| std::io::Error::other("bench writer thread"))?;
        Ok(Arc::new(Self {
            sender,
            state,
            byte_cap,
            path: path.to_path_buf(),
            writer: Mutex::new(Some(writer)),
        }))
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.state.dropped.load(Ordering::SeqCst)
    }

    pub(crate) fn written_bytes(&self) -> u64 {
        self.state.written_bytes.load(Ordering::SeqCst)
    }

    pub(crate) fn failed(&self) -> bool {
        self.state.failed.load(Ordering::SeqCst)
    }

    pub(crate) fn byte_cap(&self) -> u64 {
        self.byte_cap
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Flush and fsync the file with a bounded wait. A stalled writer never
    /// blocks process shutdown.
    pub(crate) fn shutdown(&self) {
        let (done, completed) = mpsc::sync_channel(1);
        let deadline = std::time::Instant::now() + SHUTDOWN_TIMEOUT;
        let mut message = OutletMessage::Shutdown(done);
        loop {
            match self.sender.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    if std::time::Instant::now() >= deadline {
                        // The writer is stalled. Nothing can make product
                        // shutdown wait for it; the capture stays incomplete.
                        self.state.failed.store(true, Ordering::SeqCst);
                        return;
                    }
                    message = returned;
                    thread::sleep(Duration::from_millis(2));
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.state.failed.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if completed.recv_timeout(remaining).is_err() {
            self.state.failed.store(true, Ordering::SeqCst);
            return;
        }
        if let Some(writer) = self.writer.lock().expect("bench writer mutex").take() {
            // The writer replied after fsync, so join is bounded in practice.
            let _ = writer.join();
        }
    }
}

impl BenchSink for FileOutlet {
    fn emit(&self, record: BenchRecord) {
        if self.state.failed.load(Ordering::SeqCst) {
            self.state.dropped.fetch_add(1, Ordering::SeqCst);
            return;
        }
        match self
            .sender
            .try_send(OutletMessage::Record(Box::new(record)))
        {
            Ok(()) => {}
            // A full or closed queue is a diagnostic loss, never a business
            // failure. The outlet owns the counter.
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.state.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

impl Drop for FileOutlet {
    fn drop(&mut self) {
        let (done, _completed) = mpsc::sync_channel(1);
        let _ = self.sender.try_send(OutletMessage::Shutdown(done));
    }
}

fn write_records(
    file: std::fs::File,
    receiver: Receiver<OutletMessage>,
    state: Arc<OutletState>,
    byte_cap: u64,
) {
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let mut bytes = 0u64;
    loop {
        let message = match receiver.recv() {
            Ok(message) => message,
            Err(_) => break,
        };
        let mut pending = vec![message];
        while let Ok(message) = receiver.try_recv() {
            pending.push(message);
        }
        for message in pending {
            match message {
                OutletMessage::Record(record) => {
                    let line = record.to_vec();
                    if line.is_empty() {
                        state.dropped.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    if bytes.saturating_add(line.len() as u64) > byte_cap {
                        // Preserve the cap; stop retaining detail but keep
                        // the process alive.
                        state.dropped.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    if writer
                        .write_all(&line)
                        .and_then(|()| writer.write_all(b"\n"))
                        .is_err()
                    {
                        state.failed.store(true, Ordering::SeqCst);
                        state.dropped.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    bytes += line.len() as u64 + 1;
                    state.written_bytes.store(bytes, Ordering::SeqCst);
                }
                OutletMessage::Shutdown(done) => {
                    let flushed = writer.flush().is_ok() && writer.get_ref().sync_all().is_ok();
                    if !flushed {
                        state.failed.store(true, Ordering::SeqCst);
                    }
                    let _ = writer.flush();
                    let _ = done.send(());
                    return;
                }
            }
        }
        // Flush each drained batch to the OS so a crash loses at most the
        // current channel window. fsync remains a shutdown operation.
        if writer.flush().is_err() {
            state.failed.store(true, Ordering::SeqCst);
            return;
        }
    }
    let _ = writer.flush();
}

/// Validated capture settings.
#[derive(Debug, Clone)]
struct BenchSpec {
    outlet: PathBuf,
    run_id: String,
    capture_key: String,
    lifetime: String,
    capacity: usize,
    byte_cap: u64,
}

/// Process bench composition. `core` and `cli` share one outlet, lifetime,
/// tokenizer, clock and sequence stream; only the producer role differs.
pub(crate) struct BenchSetup {
    core: Arc<BenchContext>,
    cli: Arc<BenchContext>,
    outlet: Option<Arc<FileOutlet>>,
    reason: Option<String>,
}

#[allow(dead_code)]
impl BenchSetup {
    /// Production composition: never fails the CLI. Malformed capture settings
    /// leave the disabled path and record a local reason.
    pub(crate) fn from_env() -> Self {
        Self::from_env_with(|name| env::var(name).ok(), cfg!(target_os = "linux"))
    }

    fn from_env_with(read: impl Fn(&str) -> Option<String>, linux: bool) -> Self {
        match BenchSpec::from_env(&read, linux) {
            Ok(Some(spec)) => match FileOutlet::start(&spec.outlet, spec.capacity, spec.byte_cap) {
                Ok(outlet) => {
                    let key =
                        match whalerein_core::api::bench::decode_capture_key(&spec.capture_key) {
                            Ok(key) => key,
                            Err(error) => return Self::disabled(format!("capture key: {error}")),
                        };
                    // One context owns the outlet, lifetime, tokenizer, clock
                    // and sequence stream; the CLI role shares its inner state.
                    let core = BenchContext::new(
                        spec.run_id,
                        spec.lifetime,
                        Producer::Core,
                        BenchMode::Trace,
                        &key,
                        outlet.clone(),
                    );
                    Self {
                        cli: core.for_producer(Producer::Cli),
                        core,
                        outlet: Some(outlet),
                        reason: None,
                    }
                }
                Err(error) => Self::disabled(format!("outlet: {error}")),
            },
            Ok(None) => Self::disabled("mode is not trace or outlet is unset".into()),
            Err(reason) => Self::disabled(reason),
        }
    }

    fn disabled(reason: String) -> Self {
        Self {
            core: BenchContext::disabled(Producer::Core),
            cli: BenchContext::disabled(Producer::Cli),
            outlet: None,
            reason: Some(reason),
        }
    }

    pub(crate) fn core_context(&self) -> Arc<BenchContext> {
        self.core.clone()
    }

    pub(crate) fn cli_context(&self) -> Arc<BenchContext> {
        self.cli.clone()
    }

    pub(crate) fn write_stamps(&self) -> WriteStamps {
        WriteStamps::new(self.cli.clone())
    }

    pub(crate) fn outlet(&self) -> Option<&Arc<FileOutlet>> {
        self.outlet.as_ref()
    }

    pub(crate) fn disabled_reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Flush and fsync the one process outlet. Idempotent and bounded.
    pub(crate) fn finish(&self) {
        if let Some(outlet) = &self.outlet {
            outlet.shutdown();
        }
    }
}

impl BenchSpec {
    fn from_env(
        read: &impl Fn(&str) -> Option<String>,
        linux: bool,
    ) -> Result<Option<Self>, String> {
        let mode = read(MODE_ENV).unwrap_or_default();
        let mode = BenchMode::parse(&mode)
            .ok_or_else(|| format!("{MODE_ENV} is not off, aggregate or trace"))?;
        if mode != BenchMode::Trace {
            return Ok(None);
        }
        let outlet = match read(OUTLET_ENV).filter(|value| !value.is_empty()) {
            Some(outlet) => PathBuf::from(outlet),
            None => return Ok(None),
        };
        if !linux {
            return Err("bench capture requires Linux CLOCK_MONOTONIC".into());
        }
        let run_id = read(RUN_ID_ENV)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{RUN_ID_ENV} is required in trace mode"))?;
        let capture_key = read(CAPTURE_KEY_ENV)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{CAPTURE_KEY_ENV} is required in trace mode"))?;
        let lifetime = read(LIFETIME_ENV)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(process_lifetime);
        let capacity = read(CAPACITY_ENV)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_CAPACITY)
            .max(1);
        let byte_cap = read(BYTE_CAP_ENV)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_BYTE_CAP)
            .max(1);
        Ok(Some(Self {
            outlet,
            run_id,
            capture_key,
            lifetime,
            capacity,
            byte_cap,
        }))
    }
}

fn process_lifetime() -> String {
    // Run-local opaque lifetime. The raw PID is never exported; the kernel
    // time reading plus the address of a stack local distinguishes processes
    // without disclosing a machine identifier.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let local = 0u8;
    format!("lt-{nanos}-{:x}", &local as *const u8 as usize)
}

/// Stamps a public CLI record write after `stream::write_all` and its flush.
/// A record that rendered no Core delivery has no join key and is not stamped.
#[derive(Clone)]
pub(crate) struct WriteStamps {
    bench: Arc<BenchContext>,
}

#[allow(dead_code)]
impl WriteStamps {
    pub(crate) fn new(bench: Arc<BenchContext>) -> Self {
        Self { bench }
    }

    /// Stamp one completed public record write. `range` is the preview
    /// increment the record rendered; another delivery kind or a record with
    /// no range keeps those fields absent. The stream/block tokens come from
    /// the shared Core context, so receipt, delivery and write join exactly.
    pub(crate) fn stamp(
        &self,
        delivery: Option<u64>,
        range: Option<&FrameRange>,
        bytes: u64,
        outcome: WriteOutcome,
    ) {
        let Some(delivery) = delivery else {
            return;
        };
        if !self.bench.enabled() {
            return;
        }
        let ordinal = self.bench.next_write_ordinal();
        self.bench.emit(BenchEvent::CliWrite {
            delivery,
            ordinal,
            bytes,
            outcome,
            flush: true,
            stream: range.map(|range| range.stream.clone()),
            block: range.and_then(|range| range.block.clone()),
            seq_start: range.map(|range| range.seq_start),
            seq_end: range.map(|range| range.seq_end),
        });
    }
}

/// Classify a counted write. `written == requested` with a success result is
/// `success`; an error after some bytes were accepted is `partial`; an error
/// before any byte is `failed`.
pub(crate) fn outcome_for(
    result: &Result<(), crate::stream::WriteFailure>,
    written: u64,
    requested: usize,
) -> WriteOutcome {
    match result {
        Ok(()) if written as usize == requested => WriteOutcome::Success,
        Ok(()) => WriteOutcome::Partial,
        Err(_) if written > 0 => WriteOutcome::Partial,
        Err(_) => WriteOutcome::Failed,
    }
}

#[cfg(test)]
mod tests;
