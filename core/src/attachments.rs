//! Authorized, verified attachment downloads.
//!
//! One transfer opens a Host-owned attachment occurrence, pulls contiguous
//! bounded parts through the existing encrypted query path, verifies the
//! declared length and SHA-256 outside the transcript cache, then publishes a
//! local file only after a successful atomic commit. A fresh transfer always
//! starts at offset zero; a deliberate retry is a new transfer.
//!
//! Nothing here reads or writes conversation history or the transcript cache.
//! Byte reservations, concurrency slots and timeouts are process-local and are
//! released when the transfer ends, including on cancellation or failure.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use whalerein_proto::{
    request_payload, response_payload, AttachmentPart, CloseAttachment, ErrorCode, IdKind,
    OpenAttachment, OpenAttachmentResult, PullAttachment, RequestPayload, ResponsePayload,
};

use crate::api::{Cancellation, Core, CoreError, CoreResult};
use crate::diagnostics::journal::DiagnosticKind;
use crate::session::SessionKey;

/// Fixed protojson envelope reserved by Access around an encrypted result.
pub(crate) const RESULT_ENVELOPE_OVERHEAD_BYTES: u64 = 2048;
/// Per-part framing reserved by the Host when it derives a raw allowance.
pub(crate) const RESULT_FRAMING_OVERHEAD_BYTES: u64 = 1024;
/// Local logical envelope ceiling (the packaged default profile).
const LOCAL_ENVELOPE_BYTES: u64 = crate::access::transport::MESSAGE_BYTES as u64;
/// Local encoded HTTPS result ceiling, including proto3 JSON/base64 growth.
const LOCAL_RESULT_MAX_BYTES: u64 = 24 * 1024 * 1024;
const DEFAULT_CONCURRENT: usize = 4;
const DEFAULT_RESERVED_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_OVERALL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_FILENAME_BYTES: usize = 255;
const MAX_SUGGESTION_BYTES: usize = 512;
const STAGING_ATTEMPTS: usize = 16;

/// How one attachment download is bounded locally.
///
/// Zero byte limits mean "no additional local cap"; the Host-declared
/// attachment policy still applies. Timeouts and the reservation ceiling are
/// always enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentLimits {
    pub max_total_bytes: u64,
    pub max_chunk_bytes: u32,
    pub max_concurrent: usize,
    pub max_reserved_bytes: u64,
    pub idle_timeout: Duration,
    pub overall_timeout: Duration,
}

impl Default for AttachmentLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 0,
            max_chunk_bytes: 0,
            max_concurrent: DEFAULT_CONCURRENT,
            max_reserved_bytes: DEFAULT_RESERVED_BYTES,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            overall_timeout: DEFAULT_OVERALL_TIMEOUT,
        }
    }
}

impl AttachmentLimits {
    fn validate(&self) -> CoreResult<()> {
        if self.max_concurrent == 0
            || self.max_reserved_bytes == 0
            || self.idle_timeout.is_zero()
            || self.overall_timeout.is_zero()
        {
            return Err(CoreError::InvalidInput(
                "attachment download limits must be positive".into(),
            ));
        }
        if self.idle_timeout > self.overall_timeout {
            return Err(CoreError::InvalidInput(
                "attachment idle timeout must not exceed the overall timeout".into(),
            ));
        }
        Ok(())
    }
}

/// One committed attachment occurrence in a Host-qualified session.
///
/// `attachment_ref` is opaque Source evidence from a history page or live
/// message. It is not a path, digest or authorization token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentReference {
    pub session: SessionKey,
    pub attachment_ref: String,
}

impl AttachmentReference {
    pub fn new(session: SessionKey, attachment_ref: impl Into<String>) -> Self {
        Self {
            session,
            attachment_ref: attachment_ref.into(),
        }
    }

    fn validate(&self) -> CoreResult<()> {
        if self.session.host_id.trim().is_empty() || self.session.session_id.is_empty() {
            return Err(CoreError::InvalidInput(
                "attachment session identity is missing".into(),
            ));
        }
        whalerein_proto::validate_id(IdKind::Attachment, &self.attachment_ref)
            .map_err(|_| CoreError::InvalidInput("attachment reference is invalid".into()))?;
        Ok(())
    }
}

/// Where and how to save one verified attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub reference: AttachmentReference,
    /// Existing directory that owns the final file and any staging data.
    pub destination: PathBuf,
    /// Explicit safe name. When absent the Source display name is validated as
    /// a suggestion.
    pub file_name: Option<String>,
    /// Replace an existing final file atomically. Without it a collision
    /// preserves the existing bytes.
    pub overwrite: bool,
}

impl DownloadRequest {
    pub fn new(reference: AttachmentReference, destination: impl Into<PathBuf>) -> Self {
        Self {
            reference,
            destination: destination.into(),
            file_name: None,
            overwrite: false,
        }
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    pub fn overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }
}

/// Bounded transfer progress for the CLI stderr stream.
///
/// The first event declares the authoritative total with `received == 0` and
/// the last event is emitted only after verification and a successful commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub received: u64,
    pub total: u64,
}

/// One verified local save. Presented only after the final name is visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadOutcome {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub media_type: String,
    pub verified: bool,
}

/// Effective bounds after combining the Host policy, local limits and the
/// directional encoded transport/result allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectivePolicy {
    total: u64,
    chunk: u32,
    concurrent: usize,
    idle: Duration,
    overall: Duration,
}

impl EffectivePolicy {
    fn resolve(
        policy: &whalerein_proto::AttachmentPolicy,
        limits: &AttachmentLimits,
    ) -> CoreResult<Self> {
        if policy.max_chunk_bytes == 0
            || policy.max_concurrent == 0
            || policy.idle_timeout_ms == 0
            || policy.overall_timeout_ms == 0
        {
            return Err(CoreError::Security(
                "attachment policy from the Host is invalid".into(),
            ));
        }
        if policy.max_total_bytes > 0 && u64::from(policy.max_chunk_bytes) > policy.max_total_bytes
        {
            return Err(CoreError::Security(
                "attachment policy chunk exceeds its total".into(),
            ));
        }
        if policy.idle_timeout_ms >= policy.overall_timeout_ms {
            return Err(CoreError::Security(
                "attachment policy idle timeout must be below its overall timeout".into(),
            ));
        }
        let total = min_positive(policy.max_total_bytes, limits.max_total_bytes).unwrap_or(0);
        let chunk_ceiling = min_positive(
            u64::from(policy.max_chunk_bytes),
            u64::from(limits.max_chunk_bytes),
        )
        .unwrap_or(u64::from(policy.max_chunk_bytes));
        let mut chunk =
            effective_chunk_bytes(chunk_ceiling, LOCAL_ENVELOPE_BYTES, LOCAL_RESULT_MAX_BYTES);
        if total > 0 {
            chunk = chunk.min(total);
        }
        if chunk == 0 {
            return Err(payload_too_large("attachment chunk budget is exhausted"));
        }
        let concurrent = usize::try_from(
            min_positive(
                u64::from(policy.max_concurrent),
                limits.max_concurrent as u64,
            )
            .unwrap_or(u64::from(policy.max_concurrent)),
        )
        .unwrap_or(1)
        .max(1);
        let idle = duration_from_ms(
            min_positive(
                u64::from(policy.idle_timeout_ms),
                duration_ms(limits.idle_timeout),
            )
            .unwrap_or(u64::from(policy.idle_timeout_ms)),
        );
        let overall = duration_from_ms(
            min_positive(
                u64::from(policy.overall_timeout_ms),
                duration_ms(limits.overall_timeout),
            )
            .unwrap_or(u64::from(policy.overall_timeout_ms)),
        );
        Ok(Self {
            total,
            chunk: u32::try_from(chunk).unwrap_or(u32::MAX),
            concurrent,
            idle,
            overall,
        })
    }
}

/// Raw-byte allowance for one part, mirroring the Host's encoded geometry:
/// base64 expansion for an encoded HTTPS result bound plus envelope/framing
/// reservations, and the logical envelope ceiling.
pub(crate) fn effective_chunk_bytes(max_chunk: u64, envelope: u64, result_max: u64) -> u64 {
    let mut allowance = max_chunk.min(u64::from(u32::MAX));
    if envelope > 0 {
        allowance =
            allowance
                .min(envelope.saturating_sub(
                    RESULT_ENVELOPE_OVERHEAD_BYTES + RESULT_FRAMING_OVERHEAD_BYTES,
                ));
    }
    if result_max > 0 {
        let raw = result_max.saturating_sub(RESULT_ENVELOPE_OVERHEAD_BYTES);
        let encoded = raw
            .saturating_mul(3)
            .saturating_div(4)
            .saturating_sub(RESULT_FRAMING_OVERHEAD_BYTES);
        allowance = allowance.min(encoded);
    }
    allowance.min(u64::from(u32::MAX))
}

fn min_positive(left: u64, right: u64) -> Option<u64> {
    match (left, right) {
        (0, 0) => None,
        (0, value) | (value, 0) => Some(value),
        (left, right) => Some(left.min(right)),
    }
}

fn duration_ms(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

fn duration_from_ms(value: u64) -> Duration {
    Duration::from_millis(value.min(24 * 60 * 60 * 1000))
}

fn payload_too_large(message: &str) -> CoreError {
    CoreError::Api {
        code: Some(ErrorCode::PayloadTooLarge),
        message: message.to_owned(),
    }
}

fn digest_mismatch(message: &str) -> CoreError {
    CoreError::Api {
        code: Some(ErrorCode::PayloadDigestMismatch),
        message: message.to_owned(),
    }
}

fn rate_limited(message: &str) -> CoreError {
    CoreError::Api {
        code: Some(ErrorCode::RateLimited),
        message: message.to_owned(),
    }
}

fn invalid_part(message: &str) -> CoreError {
    CoreError::InvalidInput(message.to_owned())
}

/// Process-local concurrency and byte reservation accounting. One instance is
/// owned by a Core client and never persisted.
pub(crate) struct Downloads {
    state: Mutex<DownloadState>,
    notify: Notify,
}

#[derive(Default)]
struct DownloadState {
    active: usize,
    reserved: u64,
}

impl Downloads {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(DownloadState::default()),
            notify: Notify::new(),
        }
    }

    fn active(&self) -> usize {
        self.state.lock().expect("download mutex").active
    }

    async fn acquire(
        self: &Arc<Self>,
        max_concurrent: usize,
        cancellation: &Cancellation,
    ) -> CoreResult<DownloadSlot> {
        let max_concurrent = max_concurrent.max(1);
        loop {
            {
                let mut state = self.state.lock().expect("download mutex");
                if state.active < max_concurrent {
                    state.active += 1;
                    return Ok(DownloadSlot {
                        manager: self.clone(),
                        reserved: 0,
                    });
                }
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(CoreError::Cancelled),
                _ = self.notify.notified() => {}
            }
        }
    }
}

struct DownloadSlot {
    manager: Arc<Downloads>,
    reserved: u64,
}

impl DownloadSlot {
    fn reserve(&mut self, bytes: u64, ceiling: u64) -> CoreResult<()> {
        if bytes == 0 {
            return Ok(());
        }
        if bytes > ceiling {
            return Err(rate_limited(
                "attachment exceeds the local transfer reservation budget",
            ));
        }
        let mut state = self.manager.state.lock().expect("download mutex");
        if state.reserved.saturating_add(bytes) > ceiling {
            return Err(rate_limited(
                "attachment download reservations are exhausted",
            ));
        }
        state.reserved += bytes;
        self.reserved = bytes;
        Ok(())
    }
}

impl Drop for DownloadSlot {
    fn drop(&mut self) {
        {
            let mut state = self.manager.state.lock().expect("download mutex");
            state.active = state.active.saturating_sub(1);
            state.reserved = state.reserved.saturating_sub(self.reserved);
        }
        self.manager.notify.notify_one();
    }
}

/// One download-owned staging file. Dropping it removes only its own bytes.
struct StagingFile {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl StagingFile {
    fn create(directory: &Path) -> CoreResult<Self> {
        for _ in 0..STAGING_ATTEMPTS {
            let path = directory.join(format!(
                ".whalerein-download-{}.part",
                crate::util::random_id(16)
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(save_error(&error)),
            }
        }
        Err(CoreError::Internal(
            "attachment staging file could not be created".into(),
        ))
    }

    fn write_all(&mut self, bytes: &[u8]) -> CoreResult<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| CoreError::Internal("attachment staging file is closed".into()))?;
        file.write_all(bytes).map_err(|error| save_error(&error))
    }

    fn sync_all(&mut self) -> CoreResult<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| CoreError::Internal("attachment staging file is closed".into()))?;
        file.sync_all().map_err(|error| save_error(&error))
    }

    fn commit(mut self, target: &Path, overwrite: bool) -> CoreResult<PathBuf> {
        self.file.take();
        if overwrite {
            replace_file(&self.path, target)?;
        } else {
            match std::fs::hard_link(&self.path, target) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&self.path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(CoreError::Conflict(
                        "destination already exists; choose another name or replace it".into(),
                    ))
                }
                Err(error) => return Err(save_error(&error)),
            }
        }
        sync_directory(target.parent().unwrap_or_else(|| Path::new(".")));
        Ok(target.to_path_buf())
    }
}

impl Drop for StagingFile {
    fn drop(&mut self) {
        self.file.take();
        // The staging path is download-owned. After a successful rename it is
        // already gone; after a hard-link commit this removes the extra name.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(not(windows))]
fn replace_file(staging: &Path, target: &Path) -> CoreResult<()> {
    std::fs::rename(staging, target).map_err(|error| save_error(&error))
}

#[cfg(windows)]
fn replace_file(staging: &Path, target: &Path) -> CoreResult<()> {
    if !target.exists() {
        return std::fs::rename(staging, target).map_err(|error| save_error(&error));
    }
    let backup = target.with_file_name(format!(
        ".whalerein-download-{}.replaced",
        crate::util::random_id(16)
    ));
    std::fs::rename(target, &backup).map_err(|error| save_error(&error))?;
    match std::fs::rename(staging, target) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::rename(&backup, target);
            Err(save_error(&error))
        }
    }
}

fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    {
        if let Ok(file) = std::fs::File::open(directory) {
            let _ = file.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
    }
}

/// Map a local save failure to a bounded diagnostic code and a stable error.
/// The operating-system message may contain a path, so it is never exposed.
fn save_error(error: &std::io::Error) -> CoreError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => {
            CoreError::Internal("attachment destination is not writable".into())
        }
        std::io::ErrorKind::StorageFull => {
            CoreError::Internal("attachment destination has no free space".into())
        }
        std::io::ErrorKind::AlreadyExists => CoreError::Conflict(
            "destination already exists; choose another name or replace it".into(),
        ),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
            CoreError::InvalidInput("attachment destination does not exist".into())
        }
        _ => CoreError::Internal("attachment could not be saved locally".into()),
    }
}

#[cfg(test)]
pub(crate) fn save_error_code(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::StorageFull => "disk_full",
        std::io::ErrorKind::AlreadyExists => "collision",
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => "destination_missing",
        _ => "save_failed",
    }
}

fn code_for(error: &CoreError) -> &'static str {
    error.code()
}

fn validate_destination(path: &Path) -> CoreResult<()> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CoreError::InvalidInput("attachment destination directory does not exist".into())
        } else {
            save_error(&error)
        }
    })?;
    if !metadata.is_dir() {
        return Err(CoreError::InvalidInput(
            "attachment destination must be a directory".into(),
        ));
    }
    Ok(())
}

/// Validate a Source display name or an explicit user-supplied file name.
///
/// Source suggestions may include path separators; only the last component is
/// used. Absolute paths, `..`, control characters and reserved device names
/// never become a local name. Explicit names are stricter: separators are
/// rejected rather than normalized.
pub(crate) fn safe_file_name(value: &str, explicit: bool) -> CoreResult<String> {
    if value.is_empty() || value.len() > MAX_SUGGESTION_BYTES {
        return Err(CoreError::InvalidInput(
            "attachment file name is invalid".into(),
        ));
    }
    if value
        .chars()
        .any(|character| character.is_control() || character == '\u{7f}')
    {
        return Err(CoreError::InvalidInput(
            "attachment file name contains control characters".into(),
        ));
    }
    let normalized = value.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.split('/').any(|component| component == "..")
        || normalized
            .split_once(':')
            .is_some_and(|(drive, _)| drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic())
    {
        return Err(CoreError::InvalidInput(
            "attachment file name is not safe".into(),
        ));
    }
    let candidate = if explicit {
        if value.contains('/') || value.contains('\\') {
            return Err(CoreError::InvalidInput(
                "attachment file name must not contain a path".into(),
            ));
        }
        value
    } else {
        // Normalize separators: only the last component of a Source suggestion
        // can ever name a local file.
        normalized.rsplit('/').next().unwrap_or("")
    };
    if candidate.is_empty()
        || candidate == "."
        || candidate == ".."
        || candidate.starts_with('/')
        || candidate.ends_with(' ')
        || candidate.ends_with('.')
        || candidate.len() > MAX_FILENAME_BYTES
        || candidate.contains(':')
    {
        return Err(CoreError::InvalidInput(
            "attachment file name is not safe".into(),
        ));
    }
    if is_reserved_name(candidate) {
        return Err(CoreError::InvalidInput(
            "attachment file name is reserved by the operating system".into(),
        ));
    }
    Ok(candidate.to_owned())
}

fn is_reserved_name(name: &str) -> bool {
    let stem = name
        .split_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(name)
        .trim()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn validate_metadata(
    metadata: &whalerein_proto::AttachmentMetadata,
    reference: &AttachmentReference,
) -> CoreResult<()> {
    let session = metadata
        .session
        .as_ref()
        .ok_or_else(|| CoreError::Security("attachment metadata is missing its session".into()))?;
    if session.host_id != reference.session.host_id
        || session.session_id != reference.session.session_id
        || metadata.attachment_ref != reference.attachment_ref
    {
        return Err(CoreError::Security(
            "attachment metadata does not match the requested scope".into(),
        ));
    }
    if metadata.media_type.is_empty() || metadata.media_type.len() > 256 {
        return Err(CoreError::Security(
            "attachment media type from the Host is invalid".into(),
        ));
    }
    if metadata.name.len() > MAX_SUGGESTION_BYTES {
        return Err(CoreError::Security(
            "attachment display name from the Host is invalid".into(),
        ));
    }
    if metadata.transfer_id.is_empty() {
        return Err(CoreError::Security(
            "attachment transfer handle is missing".into(),
        ));
    }
    whalerein_proto::validate_id(IdKind::Transfer, &metadata.transfer_id).map_err(|_| {
        CoreError::Security("attachment transfer handle from the Host is invalid".into())
    })?;
    if metadata.expires_at_unix_ms <= 0 {
        return Err(CoreError::Security(
            "attachment transfer expiry is missing".into(),
        ));
    }
    if metadata.expires_at_unix_ms <= crate::util::now_unix_ms() {
        return Err(CoreError::Api {
            code: Some(ErrorCode::Expired),
            message: "attachment transfer has expired".into(),
        });
    }
    let policy = metadata
        .policy
        .as_ref()
        .ok_or_else(|| CoreError::Security("attachment policy is missing".into()))?;
    if policy.max_chunk_bytes == 0
        || policy.max_concurrent == 0
        || policy.idle_timeout_ms == 0
        || policy.overall_timeout_ms == 0
    {
        return Err(CoreError::Security(
            "attachment policy from the Host is invalid".into(),
        ));
    }
    Ok(())
}

fn validate_part(
    part: &AttachmentPart,
    reference: &AttachmentReference,
    transfer_id: &str,
    expected_offset: u64,
    declared_size: u64,
    requested: u32,
) -> CoreResult<()> {
    let session = part
        .session
        .as_ref()
        .ok_or_else(|| invalid_part("attachment part is missing its session"))?;
    if session.host_id != reference.session.host_id
        || session.session_id != reference.session.session_id
        || part.attachment_ref != reference.attachment_ref
        || part.transfer_id != transfer_id
    {
        return Err(CoreError::Security(
            "attachment part does not match the transfer scope".into(),
        ));
    }
    if part.length as usize != part.data.len() {
        return Err(invalid_part(
            "attachment part length does not match its data",
        ));
    }
    if part.data.len() > requested as usize {
        return Err(payload_too_large(
            "attachment part exceeds the requested chunk budget",
        ));
    }
    if part.offset != expected_offset {
        return Err(CoreError::Conflict(
            "attachment part is not the contiguous next offset".into(),
        ));
    }
    let end = part
        .offset
        .checked_add(u64::from(part.length))
        .ok_or_else(|| invalid_part("attachment part offset overflows"))?;
    if end > declared_size {
        return Err(digest_mismatch(
            "attachment part exceeds the declared attachment size",
        ));
    }
    if part.r#final {
        if part.total_size != declared_size || end != declared_size {
            return Err(digest_mismatch(
                "attachment terminal size does not match the declared size",
            ));
        }
        if part.sha256.len() != 32 {
            return Err(digest_mismatch(
                "attachment terminal digest is missing or malformed",
            ));
        }
    } else if part.total_size != 0 || !part.sha256.is_empty() {
        return Err(invalid_part(
            "non-terminal attachment part carries terminal integrity metadata",
        ));
    }
    Ok(())
}

impl Core {
    /// Download one committed attachment to a local destination.
    ///
    /// The returned path only exists after the local file was fully verified
    /// and atomically committed. On any failure, including cancellation, all
    /// staging data owned by this download is removed and an existing final
    /// file is left untouched.
    pub async fn download_attachment<F>(
        &self,
        request: DownloadRequest,
        cancellation: &Cancellation,
        progress: F,
    ) -> CoreResult<DownloadOutcome>
    where
        F: FnMut(DownloadProgress) + Send,
    {
        self.download_attachment_with_limits(
            request,
            AttachmentLimits::default(),
            cancellation,
            progress,
        )
        .await
    }

    /// Download with explicit process-local resource limits.
    ///
    /// Operator/CLI code normally uses [`Self::download_attachment`]; this
    /// additive entry point lets a caller tighten concurrency, reservations or
    /// time budgets without changing the default request shape.
    pub async fn download_attachment_with_limits<F>(
        &self,
        request: DownloadRequest,
        limits: AttachmentLimits,
        cancellation: &Cancellation,
        mut progress: F,
    ) -> CoreResult<DownloadOutcome>
    where
        F: FnMut(DownloadProgress) + Send,
    {
        request.reference.validate()?;
        self.authorized_session(&request.reference.session)?;
        limits.validate()?;
        validate_destination(&request.destination)?;
        let explicit_name = match request.file_name.as_deref() {
            Some(name) => Some(safe_file_name(name, true)?),
            None => None,
        };
        if cancellation.is_cancelled() {
            self.attachment_diagnostic(
                "download_cancel",
                "cancelled",
                Some("cancelled"),
                0,
                None,
                None,
            );
            return Err(CoreError::Cancelled);
        }

        let mut slot = self
            .attachments
            .acquire(limits.max_concurrent, cancellation)
            .await?;
        let mut transfer_id: Option<String> = None;
        let result = self
            .download_inner(
                &request,
                &limits,
                explicit_name,
                cancellation,
                &mut slot,
                &mut transfer_id,
                &mut progress,
            )
            .await;
        if let Some(transfer_id) = transfer_id {
            let cancel = result.is_err();
            self.close_transfer(&request.reference, &transfer_id, cancel, cancellation)
                .await;
        }
        if let Err(CoreError::Cancelled) = &result {
            self.attachment_diagnostic(
                "download_cancel",
                "cancelled",
                Some("cancelled"),
                0,
                None,
                None,
            );
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_inner<F>(
        &self,
        request: &DownloadRequest,
        limits: &AttachmentLimits,
        explicit_name: Option<String>,
        cancellation: &Cancellation,
        slot: &mut DownloadSlot,
        transfer_id: &mut Option<String>,
        progress: &mut F,
    ) -> CoreResult<DownloadOutcome>
    where
        F: FnMut(DownloadProgress) + Send,
    {
        let started = Instant::now();
        let overall_deadline = started + limits.overall_timeout;
        let mut idle_deadline = started + limits.idle_timeout;

        self.attachment_diagnostic("download_open", "started", None, 0, None, None);
        let open_payload = RequestPayload {
            payload: Some(request_payload::Payload::OpenAttachment(OpenAttachment {
                session: Some((&request.reference.session).into()),
                attachment_ref: request.reference.attachment_ref.clone(),
            })),
        };
        let response = match self
            .bounded_query(
                &request.reference.session.host_id,
                open_payload,
                cancellation,
                overall_deadline,
                idle_deadline,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.attachment_diagnostic(
                    "download_open",
                    "failed",
                    Some(code_for(&error)),
                    0,
                    None,
                    None,
                );
                return Err(error);
            }
        };
        let metadata = match response.payload {
            Some(response_payload::Payload::OpenAttachmentResult(OpenAttachmentResult {
                attachment: Some(metadata),
            })) => metadata,
            _ => {
                let error =
                    CoreError::Security("unexpected attachment open response payload".into());
                self.attachment_diagnostic(
                    "download_open",
                    "failed",
                    Some(code_for(&error)),
                    0,
                    None,
                    None,
                );
                return Err(error);
            }
        };
        if let Err(error) = validate_metadata(&metadata, &request.reference) {
            self.attachment_diagnostic(
                "download_open",
                "failed",
                Some(code_for(&error)),
                0,
                None,
                None,
            );
            return Err(error);
        }
        *transfer_id = Some(metadata.transfer_id.clone());
        self.attachment_diagnostic(
            "download_open",
            "succeeded",
            None,
            metadata.size,
            None,
            None,
        );

        let policy = metadata
            .policy
            .as_ref()
            .ok_or_else(|| CoreError::Security("attachment policy is missing".into()))?;
        let policy = match EffectivePolicy::resolve(policy, limits) {
            Ok(policy) => policy,
            Err(error) => {
                self.attachment_diagnostic(
                    "download_admission",
                    "failed",
                    Some(code_for(&error)),
                    metadata.size,
                    None,
                    None,
                );
                return Err(error);
            }
        };
        if policy.total > 0 && metadata.size > policy.total {
            let error = payload_too_large("attachment exceeds the effective total policy");
            self.attachment_diagnostic(
                "download_admission",
                "failed",
                Some(code_for(&error)),
                metadata.size,
                Some(policy.total),
                None,
            );
            return Err(error);
        }
        if self.attachments.active() > policy.concurrent {
            let error = rate_limited("attachment transfer concurrency is exhausted");
            self.attachment_diagnostic(
                "download_admission",
                "failed",
                Some(code_for(&error)),
                metadata.size,
                Some(policy.concurrent as u64),
                None,
            );
            return Err(error);
        }
        if let Err(error) = slot.reserve(metadata.size, limits.max_reserved_bytes) {
            self.attachment_diagnostic(
                "download_admission",
                "failed",
                Some(code_for(&error)),
                metadata.size,
                Some(limits.max_reserved_bytes),
                None,
            );
            return Err(error);
        }
        let suggested_name = if metadata.name.is_empty() {
            None
        } else {
            Some(metadata.name.clone())
        };
        let name = match explicit_name {
            Some(name) => name,
            None => match suggested_name.as_deref() {
                Some(name) => match safe_file_name(name, false) {
                    Ok(name) => name,
                    Err(error) => {
                        self.attachment_diagnostic(
                            "download_commit",
                            "failed",
                            Some("invalid_name"),
                            0,
                            None,
                            None,
                        );
                        return Err(error);
                    }
                },
                None => {
                    self.attachment_diagnostic(
                        "download_commit",
                        "failed",
                        Some("invalid_name"),
                        0,
                        None,
                        None,
                    );
                    return Err(CoreError::InvalidInput(
                        "attachment has no safe display filename".into(),
                    ));
                }
            },
        };
        let target = request.destination.join(&name);
        self.attachment_diagnostic(
            "download_admission",
            "succeeded",
            None,
            metadata.size,
            if policy.total > 0 {
                Some(policy.total)
            } else {
                None
            },
            None,
        );

        let mut staging = match StagingFile::create(&request.destination) {
            Ok(staging) => staging,
            Err(error) => {
                self.attachment_diagnostic(
                    "download_commit",
                    "failed",
                    Some(save_code_for(&error)),
                    0,
                    None,
                    None,
                );
                return Err(error);
            }
        };
        progress(DownloadProgress {
            received: 0,
            total: metadata.size,
        });

        let mut hasher = Sha256::new();
        let mut received = 0u64;
        let mut parts = 0u32;
        let mut last_part: Option<AttachmentPart> = None;
        let mut last_retried = false;
        let expected_sha = loop {
            idle_deadline = Instant::now() + policy.idle;
            let pull_payload = RequestPayload {
                payload: Some(request_payload::Payload::PullAttachment(PullAttachment {
                    session: Some((&request.reference.session).into()),
                    attachment_ref: request.reference.attachment_ref.clone(),
                    transfer_id: metadata.transfer_id.clone(),
                    offset: received,
                    max_bytes: policy.chunk,
                })),
            };
            let response = self
                .bounded_query(
                    &request.reference.session.host_id,
                    pull_payload,
                    cancellation,
                    overall_deadline,
                    idle_deadline,
                )
                .await
                .inspect_err(|error| {
                    self.attachment_diagnostic(
                        "download_part",
                        "failed",
                        Some(code_for(error)),
                        received,
                        Some(policy.chunk as u64),
                        Some(parts),
                    );
                })?;
            let part = match response.payload {
                Some(response_payload::Payload::AttachmentPart(part)) => part,
                _ => {
                    let error =
                        CoreError::Security("unexpected attachment pull response payload".into());
                    self.attachment_diagnostic(
                        "download_part",
                        "failed",
                        Some(code_for(&error)),
                        received,
                        Some(policy.chunk as u64),
                        Some(parts),
                    );
                    return Err(error);
                }
            };
            if received > 0 && part.offset < received {
                // Only the last completed offset is an idempotent retry; every
                // other overlap is a conflict and never writes more bytes.
                if last_part
                    .as_ref()
                    .is_none_or(|previous| previous.offset != part.offset)
                {
                    let error = CoreError::Conflict(
                        "attachment part overlaps an already received range".into(),
                    );
                    self.attachment_diagnostic(
                        "download_part",
                        "failed",
                        Some(code_for(&error)),
                        received,
                        Some(policy.chunk as u64),
                        Some(parts),
                    );
                    return Err(error);
                }
                if last_retried {
                    let error =
                        CoreError::Conflict("attachment part was duplicated more than once".into());
                    self.attachment_diagnostic(
                        "download_part",
                        "failed",
                        Some(code_for(&error)),
                        received,
                        Some(policy.chunk as u64),
                        Some(parts),
                    );
                    return Err(error);
                }
                let previous = last_part.as_ref().expect("checked above");
                if previous.data != part.data
                    || previous.r#final != part.r#final
                    || previous.total_size != part.total_size
                    || previous.sha256 != part.sha256
                {
                    let error =
                        digest_mismatch("attachment retried part conflicts with the prior part");
                    self.attachment_diagnostic(
                        "download_part",
                        "failed",
                        Some(code_for(&error)),
                        received,
                        Some(policy.chunk as u64),
                        Some(parts),
                    );
                    return Err(error);
                }
                last_retried = true;
                continue;
            }
            validate_part(
                &part,
                &request.reference,
                &metadata.transfer_id,
                received,
                metadata.size,
                policy.chunk,
            )
            .inspect_err(|error| {
                self.attachment_diagnostic(
                    "download_part",
                    "failed",
                    Some(code_for(error)),
                    received,
                    Some(policy.chunk as u64),
                    Some(parts),
                );
            })?;
            if part.data.is_empty() && !part.r#final && received < metadata.size {
                let error = invalid_part("attachment part made no progress");
                self.attachment_diagnostic(
                    "download_part",
                    "failed",
                    Some(code_for(&error)),
                    received,
                    Some(policy.chunk as u64),
                    Some(parts),
                );
                return Err(error);
            }
            if !part.data.is_empty() {
                staging.write_all(&part.data).inspect_err(|error| {
                    self.attachment_diagnostic(
                        "download_part",
                        "failed",
                        Some(save_code_for(error)),
                        received,
                        Some(policy.chunk as u64),
                        Some(parts),
                    );
                })?;
                hasher.update(&part.data);
                received += u64::from(part.length);
            }
            parts += 1;
            progress(DownloadProgress {
                received,
                total: metadata.size,
            });
            self.attachment_diagnostic(
                "download_part",
                "succeeded",
                None,
                received,
                Some(policy.chunk as u64),
                Some(parts),
            );
            last_retried = false;
            last_part = Some(part.clone());
            if part.r#final {
                break part.sha256.clone();
            }
            if received == metadata.size {
                let error =
                    digest_mismatch("attachment source ended without terminal integrity metadata");
                self.attachment_diagnostic(
                    "download_part",
                    "failed",
                    Some(code_for(&error)),
                    received,
                    Some(policy.chunk as u64),
                    Some(parts),
                );
                return Err(error);
            }
        };

        let digest = hasher.finalize();
        if expected_sha.len() != 32 || expected_sha != digest.as_slice() {
            let error =
                digest_mismatch("attachment content digest does not match the declared digest");
            self.attachment_diagnostic(
                "download_verify",
                "failed",
                Some(code_for(&error)),
                received,
                None,
                Some(parts),
            );
            return Err(error);
        }
        if received != metadata.size {
            let error =
                digest_mismatch("attachment content length does not match the declared size");
            self.attachment_diagnostic(
                "download_verify",
                "failed",
                Some(code_for(&error)),
                received,
                None,
                Some(parts),
            );
            return Err(error);
        }
        staging.sync_all().inspect_err(|error| {
            self.attachment_diagnostic(
                "download_verify",
                "failed",
                Some(save_code_for(error)),
                received,
                None,
                Some(parts),
            );
        })?;
        self.attachment_diagnostic(
            "download_verify",
            "succeeded",
            None,
            received,
            None,
            Some(parts),
        );

        let committed = staging
            .commit(&target, request.overwrite)
            .inspect_err(|error| {
                self.attachment_diagnostic(
                    "download_commit",
                    "failed",
                    Some(save_code_for(error)),
                    received,
                    None,
                    Some(parts),
                );
            })?;
        let sha256 = hex::encode(digest);
        self.attachment_diagnostic(
            "download_commit",
            "succeeded",
            None,
            received,
            None,
            Some(parts),
        );
        progress(DownloadProgress {
            received,
            total: metadata.size,
        });
        Ok(DownloadOutcome {
            path: committed,
            name,
            bytes: received,
            sha256,
            media_type: metadata.media_type,
            verified: true,
        })
    }

    async fn bounded_query(
        &self,
        host_id: &str,
        payload: RequestPayload,
        cancellation: &Cancellation,
        overall_deadline: Instant,
        idle_deadline: Instant,
    ) -> CoreResult<ResponsePayload> {
        let deadline = overall_deadline.min(idle_deadline);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CoreError::Timeout);
        }
        tokio::select! {
            _ = cancellation.cancelled() => Err(CoreError::Cancelled),
            result = tokio::time::timeout(remaining, self.query(host_id, payload)) => {
                match result {
                    Ok(result) => result,
                    Err(_) => Err(CoreError::Timeout),
                }
            }
        }
    }

    async fn close_transfer(
        &self,
        reference: &AttachmentReference,
        transfer_id: &str,
        cancel: bool,
        cancellation: &Cancellation,
    ) {
        let payload = RequestPayload {
            payload: Some(request_payload::Payload::CloseAttachment(CloseAttachment {
                session: Some((&reference.session).into()),
                attachment_ref: reference.attachment_ref.clone(),
                transfer_id: transfer_id.to_owned(),
                cancel,
            })),
        };
        let close = self.query(&reference.session.host_id, payload);
        // Best effort: a cancelled caller must not wait for Host cleanup.
        let allowance = if cancellation.is_cancelled() && cancel {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(5)
        };
        let _ = tokio::time::timeout(allowance, close).await;
    }

    #[allow(clippy::too_many_arguments)]
    fn attachment_diagnostic(
        &self,
        stage: &'static str,
        outcome: &'static str,
        code: Option<&'static str>,
        bytes: u64,
        limit: Option<u64>,
        parts: Option<u32>,
    ) {
        self.diagnostics
            .record_stage("core", stage, outcome, code.unwrap_or("none"));
        self.diagnostics.record(DiagnosticKind::AttachmentTransfer {
            stage,
            outcome,
            code,
            bytes,
            limit,
            parts,
        });
    }
}

fn save_code_for(error: &CoreError) -> &'static str {
    match error {
        CoreError::InvalidInput(message) if message.contains("file name") => "invalid_name",
        CoreError::InvalidInput(_) => "destination_missing",
        CoreError::Conflict(_) => "collision",
        CoreError::Internal(message) if message.contains("not writable") => "permission_denied",
        CoreError::Internal(message) if message.contains("free space") => "disk_full",
        _ => "save_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_chunk_accounts_for_base64_envelope_and_framing() {
        assert_eq!(
            effective_chunk_bytes(60 * 1024, 16 * 1024 * 1024, 24 * 1024 * 1024),
            60 * 1024
        );
        // The envelope reserves the protojson envelope and per-part framing.
        assert_eq!(effective_chunk_bytes(u64::MAX, 4096, 0), 4096 - 3072);
        // Base64 expansion: an encoded bound of 4096 leaves 2048 raw minus framing.
        assert_eq!(
            effective_chunk_bytes(u64::MAX, 0, 4096),
            (4096 - 2048) * 3 / 4 - 1024
        );
        // The smallest applicable bound wins.
        assert_eq!(effective_chunk_bytes(500, 4096, 4096), 500);
    }

    #[test]
    fn safe_names_reject_paths_reserved_names_and_control_characters() {
        assert_eq!(safe_file_name("photo.png", false).unwrap(), "photo.png");
        assert_eq!(safe_file_name("a/b\\c.png", false).unwrap(), "c.png");
        for unsafe_name in [
            "/etc/passwd",
            "C:\\Windows\\x.txt",
            "..",
            ".",
            "a/../b",
            "CON",
            "con.txt",
            "LPT1.log",
            "a\u{7f}b",
            "a:b",
            "trailing.",
            "trailing ",
        ] {
            assert!(
                safe_file_name(unsafe_name, false).is_err(),
                "{unsafe_name:?} must not become a local name"
            );
        }
        assert!(safe_file_name("a/b", true).is_err());
        assert!(safe_file_name(".hidden", false).is_ok());
        assert!(safe_file_name(&"a".repeat(MAX_FILENAME_BYTES + 1), false).is_err());
    }

    #[test]
    fn save_error_codes_are_stable_and_path_free() {
        let cases = [
            (std::io::ErrorKind::PermissionDenied, "permission_denied"),
            (std::io::ErrorKind::StorageFull, "disk_full"),
            (std::io::ErrorKind::AlreadyExists, "collision"),
            (std::io::ErrorKind::NotFound, "destination_missing"),
            (std::io::ErrorKind::Other, "save_failed"),
        ];
        for (kind, code) in cases {
            let error = std::io::Error::from(kind);
            assert_eq!(save_error_code(&error), code);
            let mapped = save_error(&error);
            assert!(!mapped.to_string().contains("attachment path"));
        }
    }

    #[test]
    fn staging_failure_removes_only_its_own_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let sibling = directory.path().join("existing.bin");
        std::fs::write(&sibling, b"keep").unwrap();
        let mut staging = StagingFile::create(directory.path()).unwrap();
        staging.write_all(b"partial").unwrap();
        staging.file.take();
        let error = staging.write_all(b"more").unwrap_err();
        assert_eq!(error.code(), "internal");
        drop(staging);
        assert_eq!(std::fs::read(&sibling).unwrap(), b"keep");
        assert!(std::fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".whalerein-download-")
        }));
    }

    #[test]
    fn limits_reject_invalid_relations() {
        let limits = AttachmentLimits {
            max_concurrent: 0,
            ..AttachmentLimits::default()
        };
        assert!(limits.validate().is_err());
        let limits = AttachmentLimits::default();
        let limits = AttachmentLimits {
            idle_timeout: limits.overall_timeout + Duration::from_secs(1),
            ..limits
        };
        assert!(limits.validate().is_err());
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use crate::api::test_support::{FakeAccess, FakeAttachmentObject};
    use crate::api::{ClientConfig, ClientService, Core, MemorySecretStore, PairingDecision};
    use crate::diagnostics::journal::CapturingSink;
    use std::sync::Arc;

    fn attachment(
        session: &str,
        reference: &str,
        name: &str,
        bytes: Vec<u8>,
    ) -> FakeAttachmentObject {
        FakeAttachmentObject::new(session, reference, name, bytes)
    }

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

    async fn paired_with_sink() -> (tempfile::TempDir, Core, FakeAccess, Arc<CapturingSink>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        Core::write_config(
            &path,
            &ClientConfig::new(ClientService::Standalone, "instance"),
        )
        .unwrap();
        let sink = Arc::new(CapturingSink::default());
        let secrets = Arc::new(MemorySecretStore::new());
        let core = Core::open_with_diagnostic_sink(&path, secrets, sink.clone()).unwrap();
        let access = FakeAccess::start().await;
        core.pair(&access.pairing_uri(), |_| PairingDecision::approve())
            .await
            .unwrap();
        sink.0.lock().unwrap().clear();
        (directory, core, access, sink)
    }

    fn reference() -> AttachmentReference {
        AttachmentReference::new(
            SessionKey::new("host-1", "session-1").unwrap(),
            "attachment-1",
        )
    }

    fn request(destination: &Path) -> DownloadRequest {
        DownloadRequest::new(reference(), destination)
    }

    fn files(directory: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn no_staging(directory: &Path) {
        assert!(
            files(directory)
                .iter()
                .all(|name| !name.starts_with(".whalerein-download-")),
            "staging data must be removed: {:?}",
            files(directory)
        );
    }

    #[tokio::test]
    async fn downloads_verified_bytes_in_bounded_parts_and_closes_the_transfer() {
        let (directory, core, access) = paired().await;
        let bytes = (0..200 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut object = attachment("session-1", "attachment-1", "report.bin", bytes.clone());
        object.policy.max_chunk_bytes = 32 * 1024;
        object.policy.max_total_bytes = 1024 * 1024;
        let policy = object.policy;
        access.host.lock().unwrap().put_attachment(object);
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let mut events = Vec::new();
        let outcome = core
            .download_attachment(request(&destination), &Cancellation::new(), |event| {
                events.push(event)
            })
            .await
            .unwrap();
        assert_eq!(outcome.bytes, bytes.len() as u64);
        assert_eq!(outcome.sha256, crate::util::sha256_hex(&bytes));
        assert_eq!(outcome.name, "report.bin");
        assert_eq!(outcome.media_type, "application/octet-stream");
        assert!(outcome.verified);
        assert_eq!(outcome.path, destination.join("report.bin"));
        assert_eq!(std::fs::read(&outcome.path).unwrap(), bytes);
        assert_eq!(
            events.first(),
            Some(&DownloadProgress {
                received: 0,
                total: bytes.len() as u64
            })
        );
        assert_eq!(
            events.last(),
            Some(&DownloadProgress {
                received: bytes.len() as u64,
                total: bytes.len() as u64
            })
        );
        assert_eq!(
            events.iter().filter(|event| event.received > 0).count(),
            (bytes.len() as u64).div_ceil(policy.max_chunk_bytes as u64) as usize + 1
        );
        no_staging(&destination);
        let host = access.host.lock().unwrap();
        assert_eq!(host.attachment_open_calls, 1);
        assert_eq!(host.attachment_close_calls, 1);
        assert!(host.attachment_pull_calls >= 6);
        assert_eq!(host.history_reads, 0);
        assert_eq!(host.attachment_pull_offsets[0], 0);
    }

    #[tokio::test]
    async fn empty_file_completes_with_an_empty_verified_result() {
        let (directory, core, access) = paired().await;
        access.host.lock().unwrap().put_attachment(attachment(
            "session-1",
            "attachment-1",
            "empty.txt",
            Vec::new(),
        ));
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let outcome = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.bytes, 0);
        assert_eq!(outcome.sha256, crate::util::sha256_hex(&[]));
        assert_eq!(std::fs::read(outcome.path).unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn foreign_scope_and_missing_occurrences_fail_before_reading_bytes() {
        let (directory, core, access) = paired().await;
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();

        let foreign_host = AttachmentReference::new(
            SessionKey::new("host-2", "session-1").unwrap(),
            "attachment-1",
        );
        let error = core
            .download_attachment(
                DownloadRequest::new(foreign_host, &destination),
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
        assert_eq!(access.host.lock().unwrap().attachment_open_calls, 0);

        access.host.lock().unwrap().put_attachment(attachment(
            "session-2",
            "attachment-1",
            "private.bin",
            b"foreign".to_vec(),
        ));
        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
        assert_eq!(access.host.lock().unwrap().attachment_pull_calls, 0);

        let missing = AttachmentReference::new(
            SessionKey::new("host-1", "session-1").unwrap(),
            "attachment-missing",
        );
        let error = core
            .download_attachment(
                DownloadRequest::new(missing, &destination),
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
        assert!(files(&destination).is_empty());
        no_staging(&destination);
    }

    #[tokio::test]
    async fn a_rejected_open_and_forged_handle_fail_closed() {
        let (directory, core, access) = paired().await;
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        access.host.lock().unwrap().put_attachment(attachment(
            "session-1",
            "attachment-1",
            "device.bin",
            b"device".to_vec(),
        ));
        access.host.lock().unwrap().attachment_open_error = Some(ErrorCode::NotFound);
        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
        assert!(access.host.lock().unwrap().attachment_pull_calls == 0);
        assert!(files(&destination).is_empty());

        // A closed or forged transfer handle fails on the first pull.
        access.host.lock().unwrap().attachment_open_error = None;
        access.host.lock().unwrap().attachment_pull_error = Some(ErrorCode::NotFound);
        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
        assert!(files(&destination).is_empty());
        no_staging(&destination);
    }

    #[tokio::test]
    async fn unsafe_names_and_oversize_or_unsupported_reads_fail_before_bytes() {
        let (directory, core, access) = paired().await;
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        access.host.lock().unwrap().put_attachment(attachment(
            "session-1",
            "attachment-1",
            "../escape.bin",
            b"data".to_vec(),
        ));

        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "invalid_argument");
        assert!(access.host.lock().unwrap().attachment_pull_calls == 0);
        assert!(files(&destination).is_empty());
        no_staging(&destination);

        let error = core
            .download_attachment(
                request(&destination).file_name("../escape.bin"),
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "invalid_argument");

        // An object above the Host total policy is rejected by the Host.
        let mut object = attachment("session-1", "attachment-1", "huge.bin", vec![7; 16]);
        object.policy.max_total_bytes = 8;
        object.policy.max_chunk_bytes = 8;
        access.host.lock().unwrap().put_attachment(object);
        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "payload_too_large");
        assert_eq!(access.host.lock().unwrap().attachment_pull_calls, 0);

        // A local total policy rejects the declared size before any part read.
        let mut object = attachment("session-1", "attachment-1", "local.bin", vec![7; 16]);
        object.policy.max_total_bytes = 1024;
        object.policy.max_chunk_bytes = 1024;
        access.host.lock().unwrap().put_attachment(object);
        let limits = AttachmentLimits {
            max_total_bytes: 8,
            ..AttachmentLimits::default()
        };
        let error = core
            .download_attachment_with_limits(
                request(&destination),
                limits,
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "payload_too_large");
        assert_eq!(access.host.lock().unwrap().attachment_pull_calls, 0);

        // Unsupported storage is an explicit capability result.
        access.host.lock().unwrap().attachment_unsupported = true;
        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "unsupported_capability");
        assert!(files(&destination).is_empty());
        no_staging(&destination);
    }

    #[tokio::test]
    async fn part_faults_never_publish_a_file() {
        let (directory, core, access) = paired().await;
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let bytes = vec![3u8; 96 * 1024];

        for (configure, expected) in [
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_wrong_digest = true;
                }) as Box<dyn Fn(&mut crate::api::test_support::FakeHost)>,
                "payload_digest_mismatch",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_wrong_total = true;
                }),
                "payload_digest_mismatch",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_missing_final = true;
                }),
                "payload_digest_mismatch",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_gap_part_at = Some(1);
                }),
                "conflict",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_wrong_length_part_at = Some(1);
                }),
                "invalid_argument",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_overlap_part_at = Some(1);
                }),
                "conflict",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_stalled_part_at = Some(1);
                }),
                "invalid_argument",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_pull_error_after = Some((1, ErrorCode::Unavailable));
                }),
                "unavailable",
            ),
            (
                Box::new(|host: &mut crate::api::test_support::FakeHost| {
                    host.attachment_pull_error = Some(ErrorCode::NotFound);
                }),
                "not_found",
            ),
        ] {
            let mut object = attachment("session-1", "attachment-1", "fault.bin", bytes.clone());
            object.policy.max_chunk_bytes = 16 * 1024;
            {
                let mut host = access.host.lock().unwrap();
                host.put_attachment(object);
                host.attachment_wrong_digest = false;
                host.attachment_wrong_total = false;
                host.attachment_missing_final = false;
                host.attachment_gap_part_at = None;
                host.attachment_overlap_part_at = None;
                host.attachment_wrong_length_part_at = None;
                host.attachment_stalled_part_at = None;
                host.attachment_pull_error_after = None;
                host.attachment_pull_error = None;
                configure(&mut host);
            }
            let error = core
                .download_attachment(request(&destination), &Cancellation::new(), |_| {})
                .await
                .unwrap_err();
            assert_eq!(error.code(), expected);
            assert!(!destination.join("fault.bin").exists());
            no_staging(&destination);
        }
    }

    #[tokio::test]
    async fn a_forbidden_mid_read_fails_closed_without_publishing_bytes() {
        let (directory, core, access) = paired().await;
        let bytes = vec![6u8; 48 * 1024];
        let mut object = attachment("session-1", "attachment-1", "revoked.bin", bytes);
        object.policy.max_chunk_bytes = 16 * 1024;
        {
            let mut host = access.host.lock().unwrap();
            host.put_attachment(object);
            host.attachment_pull_error_after = Some((1, ErrorCode::Forbidden));
        }
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        // Core retries a read once after an authorization-shaped Host error and
        // then reports the retry's stable failure; bytes are never published.
        assert!(matches!(
            error,
            CoreError::Host { .. } | CoreError::Api { .. }
        ));
        assert!(!destination.join("revoked.bin").exists());
        no_staging(&destination);
    }

    #[tokio::test]
    async fn a_single_duplicate_part_is_idempotent_without_duplicating_bytes() {
        let (directory, core, access) = paired().await;
        let bytes = vec![9u8; 48 * 1024];
        let mut object = attachment("session-1", "attachment-1", "retry.bin", bytes.clone());
        object.policy.max_chunk_bytes = 16 * 1024;
        {
            let mut host = access.host.lock().unwrap();
            host.put_attachment(object);
            host.attachment_duplicate_part_at = Some(1);
        }
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let outcome = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.sha256, crate::util::sha256_hex(&bytes));
        assert_eq!(std::fs::read(outcome.path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn cancellation_cleans_up_and_a_fresh_retry_starts_at_zero() {
        let (directory, core, access) = paired().await;
        let bytes = vec![5u8; 64 * 1024];
        let mut object = attachment("session-1", "attachment-1", "cancel.bin", bytes.clone());
        object.policy.max_chunk_bytes = 16 * 1024;
        access.host.lock().unwrap().put_attachment(object);
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        access.host.lock().unwrap().attachment_pull_delay_ms = 300;

        let cancellation = Cancellation::new();
        let cancel = cancellation.clone();
        let task = {
            let core = core.clone();
            let destination = destination.clone();
            tokio::spawn(async move {
                core.download_attachment(request(&destination), &cancellation, |_| {})
                    .await
            })
        };
        let readiness = std::time::Instant::now();
        while access.host.lock().unwrap().attachment_open_calls == 0 {
            assert!(
                readiness.elapsed() < Duration::from_secs(5),
                "the transfer never opened"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code(), "cancelled");
        assert!(!destination.join("cancel.bin").exists());
        no_staging(&destination);

        access.host.lock().unwrap().attachment_pull_delay_ms = 0;
        let outcome = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.sha256, crate::util::sha256_hex(&bytes));
        let host = access.host.lock().unwrap();
        assert_eq!(host.attachment_open_calls, 2, "retry is a fresh transfer");
        assert!(host.attachment_pull_offsets.contains(&0));
        assert!(
            host.attachment_pull_offsets
                .iter()
                .filter(|offset| **offset == 0)
                .count()
                >= 2
        );
    }

    #[tokio::test]
    async fn overall_and_idle_timeouts_release_the_transfer_without_a_file() {
        let (directory, core, access) = paired().await;
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let mut object = attachment(
            "session-1",
            "attachment-1",
            "slow.bin",
            vec![1u8; 64 * 1024],
        );
        object.policy.max_chunk_bytes = 16 * 1024;
        access.host.lock().unwrap().put_attachment(object);
        access.host.lock().unwrap().attachment_open_delay_ms = 400;
        let limits = AttachmentLimits {
            overall_timeout: Duration::from_millis(60),
            idle_timeout: Duration::from_millis(60),
            ..AttachmentLimits::default()
        };
        let error = core
            .download_attachment_with_limits(
                request(&destination),
                limits,
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "timeout");
        assert!(!destination.join("slow.bin").exists());
        no_staging(&destination);

        access.host.lock().unwrap().attachment_open_delay_ms = 0;
        access.host.lock().unwrap().attachment_pull_delay_ms = 400;
        let limits = AttachmentLimits {
            idle_timeout: Duration::from_millis(60),
            overall_timeout: Duration::from_secs(5),
            ..AttachmentLimits::default()
        };
        let error = core
            .download_attachment_with_limits(
                request(&destination),
                limits,
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "timeout");
        assert!(!destination.join("slow.bin").exists());
        no_staging(&destination);
    }

    #[tokio::test]
    async fn local_concurrency_and_byte_reservations_are_bounded_and_released() {
        let manager = Arc::new(Downloads::new());
        let cancellation = Cancellation::new();
        let first = manager.acquire(1, &cancellation).await.unwrap();
        let waiting = {
            let manager = manager.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move { manager.acquire(1, &cancellation).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());
        drop(first);
        let second = waiting.await.unwrap().unwrap();
        assert_eq!(manager.active(), 1);
        drop(second);
        assert_eq!(manager.active(), 0);

        let mut slot = manager.acquire(1, &cancellation).await.unwrap();
        slot.reserve(10, 10).unwrap();
        assert!(slot.reserve(1, 10).is_err());
        drop(slot);
        let mut slot = manager.acquire(1, &cancellation).await.unwrap();
        slot.reserve(10, 10).unwrap();
    }

    #[tokio::test]
    async fn collision_preserves_existing_bytes_and_explicit_replacement_commits() {
        let (directory, core, access) = paired().await;
        access.host.lock().unwrap().put_attachment(attachment(
            "session-1",
            "attachment-1",
            "report.bin",
            b"new bytes".to_vec(),
        ));
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let target = destination.join("report.bin");
        std::fs::write(&target, b"existing").unwrap();

        let error = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "conflict");
        assert_eq!(std::fs::read(&target).unwrap(), b"existing");
        no_staging(&destination);

        let outcome = core
            .download_attachment(
                request(&destination).overwrite(true),
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(outcome.path, target);
        assert_eq!(std::fs::read(&target).unwrap(), b"new bytes");
        no_staging(&destination);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denied_destination_and_failed_finalization_preserve_existing_data() {
        if unsafe { libc::geteuid() } == 0 {
            // Root bypasses directory permissions; the platform job without
            // root executes this case.
            return;
        }
        let (directory, core, access) = paired().await;
        access.host.lock().unwrap().put_attachment(attachment(
            "session-1",
            "attachment-1",
            "report.bin",
            b"bytes".to_vec(),
        ));
        let denied = directory.path().join("denied");
        std::fs::create_dir(&denied).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = core
            .download_attachment(request(&denied), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.code(), "internal");
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(files(&denied).is_empty());

        // A directory at the final name is never replaced, even with overwrite.
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let target = destination.join("report.bin");
        std::fs::create_dir(&target).unwrap();
        let error = core
            .download_attachment(
                request(&destination).overwrite(true),
                &Cancellation::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "internal");
        assert!(target.is_dir());
        no_staging(&destination);
    }

    #[tokio::test]
    async fn diagnostics_report_counts_and_limits_without_names_paths_or_occurrences() {
        let (directory, core, access, sink) = paired_with_sink().await;
        let bytes = vec![4u8; 40 * 1024];
        let mut object = attachment(
            "session-1",
            "attachment-1",
            "secret-name.bin",
            bytes.clone(),
        );
        object.policy.max_chunk_bytes = 16 * 1024;
        object.policy.max_total_bytes = 1024 * 1024;
        access.host.lock().unwrap().put_attachment(object);
        let destination = directory.path().join("private-download-directory");
        std::fs::create_dir(&destination).unwrap();
        core.download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap();

        let events = sink.0.lock().unwrap().clone();
        let text = serde_json::to_string(&events).unwrap();
        assert!(text.contains("\"event\":\"attachment_transfer\""));
        assert!(text.contains("\"stage\":\"download_admission\""));
        assert!(text.contains("\"stage\":\"download_verify\""));
        assert!(text.contains("\"stage\":\"download_commit\""));
        assert!(text.contains("\"bytes\":40960"));
        assert!(!text.contains("secret-name.bin"));
        assert!(!text.contains("private-download-directory"));
        assert!(!text.contains("attachment-1"));
        assert!(!text.contains("session-1"));
        assert!(!text.contains(&crate::util::sha256_hex(&bytes)));
        assert!(!text.contains("report"));
        let stages = events
            .iter()
            .filter(|event| event["event"] == "operation_diagnostic")
            .map(|event| event["stage"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(stages.iter().any(|stage| stage == "download_open"));
        assert!(stages.iter().any(|stage| stage == "download_verify"));
    }

    #[tokio::test]
    async fn local_save_failure_diagnostics_carry_only_a_stable_code() {
        let (directory, core, access, sink) = paired_with_sink().await;
        access.host.lock().unwrap().put_attachment(attachment(
            "session-1",
            "attachment-1",
            "../escape.bin",
            b"data".to_vec(),
        ));
        let destination = directory.path().join("private-download-directory");
        std::fs::create_dir(&destination).unwrap();
        let _ = core
            .download_attachment(request(&destination), &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        let events = sink.0.lock().unwrap().clone();
        let text = serde_json::to_string(&events).unwrap();
        assert!(text.contains("invalid_name"));
        assert!(!text.contains("escape.bin"));
        assert!(!text.contains("private-download-directory"));
        assert!(!text.contains("attachment-1"));
        assert!(files(&destination).is_empty());
    }

    #[tokio::test]
    async fn a_file_larger_than_one_chunk_completes_over_multiple_bounded_parts() {
        let (directory, core, access) = paired().await;
        let bytes = (0..900 * 1024)
            .map(|index| (index % 253) as u8)
            .collect::<Vec<_>>();
        let mut object = attachment("session-1", "attachment-1", "large.bin", bytes.clone());
        object.policy.max_chunk_bytes = 64 * 1024;
        {
            let mut host = access.host.lock().unwrap();
            host.put_attachment(object);
            host.attachment_pull_offsets.clear();
        }
        let destination = directory.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let mut advanced = Vec::new();
        let outcome = core
            .download_attachment(request(&destination), &Cancellation::new(), |event| {
                advanced.push((event.received, event.total));
            })
            .await
            .unwrap();
        assert_eq!(outcome.bytes, bytes.len() as u64);
        assert_eq!(outcome.sha256, crate::util::sha256_hex(&bytes));
        assert!(advanced.len() >= 14);
        assert_eq!(advanced.last().unwrap().0, bytes.len() as u64);
        let host = access.host.lock().unwrap();
        assert!(host.attachment_pull_calls >= 14);
        assert_eq!(host.attachment_pull_offsets[0], 0);
    }
}
