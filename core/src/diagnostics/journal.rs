//! Bounded metadata-only history for the most recent failed operation.
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::api::{CoreError, CoreResult};
use crate::util::{now_unix_ms, random_id};

pub const FAILURE_FILE: &str = "last-failure.json";
const MAX_EVENTS: usize = 32;
const MAX_BYTES: u64 = 32 * 1024;
const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticEvent {
    pub time_unix_ms: i64,
    pub layer: String,
    pub stage: String,
    pub outcome: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureRecord {
    pub trace_id: String,
    pub recorded_at_unix_ms: i64,
    pub events: Vec<DiagnosticEvent>,
}

pub struct Journal {
    trace_id: String,
    events: Mutex<VecDeque<DiagnosticEvent>>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            trace_id: random_id(16),
            events: Mutex::new(VecDeque::new()),
        }
    }
}

impl Journal {
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    // Callers pass stable vocabulary only; never errors' Display strings.
    pub fn record(&self, layer: &str, stage: &str, outcome: &str, code: &str) {
        if ![layer, stage, outcome, code]
            .iter()
            .all(|value| token(value))
        {
            return;
        }
        // The CLI renders terminal errors once. Journal stages are opt-in
        // console detail, while the bounded history is always recorded below.
        tracing::info!(event = "operation_diagnostic", trace_id = %self.trace_id, layer, stage, outcome, code);
        let mut events = self.events.lock().expect("diagnostic journal mutex");
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(DiagnosticEvent {
            time_unix_ms: now_unix_ms(),
            layer: layer.into(),
            stage: stage.into(),
            outcome: outcome.into(),
            code: code.into(),
        });
    }

    pub fn save_failure(&self, directory: &Path, code: &str) -> CoreResult<()> {
        self.record("core", "operation", "failed", code);
        let record = FailureRecord {
            trace_id: self.trace_id.clone(),
            recorded_at_unix_ms: now_unix_ms(),
            events: self
                .events
                .lock()
                .expect("diagnostic journal mutex")
                .iter()
                .cloned()
                .collect(),
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|_| CoreError::Internal("diagnostic encoding failed".into()))?;
        let path = directory.join(FAILURE_FILE);
        // Create privately before writing; never expose the record during chmod.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options
            .open(&path)
            .map_err(|_| CoreError::Internal("diagnostic file could not be opened".into()))?;
        crate::api::config::set_private_permissions(&path)?;
        file.write_all(&bytes)
            .map_err(|_| CoreError::Internal("diagnostic file could not be written".into()))
    }
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Missing, corrupt, oversized and expired records do not break doctor/export.
pub fn load_failure(directory: &Path) -> Option<FailureRecord> {
    let path = directory.join(FAILURE_FILE);
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_BYTES {
        return None;
    }
    let record: FailureRecord = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let age = now_unix_ms().checked_sub(record.recorded_at_unix_ms)?;
    if !(0..=RETENTION_MS).contains(&age) {
        let _ = std::fs::remove_file(path);
        return None;
    }
    if record.trace_id.len() != 32
        || !record.trace_id.bytes().all(|b| b.is_ascii_hexdigit())
        || record.events.len() > MAX_EVENTS
        || record.events.iter().any(|event| {
            ![&event.layer, &event.stage, &event.outcome, &event.code]
                .iter()
                .all(|value| token(value))
        })
    {
        return None;
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_bounded_diagnostic_history_without_sensitive_details() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::default();
        for _ in 0..100 {
            journal.record("host", "query", "retrying", "unauthorized");
        }
        journal.record("core", "token=secret", "failed", "secret message");
        journal.save_failure(dir.path(), "timeout").unwrap();
        let record = load_failure(dir.path()).unwrap();
        assert_eq!(record.trace_id, journal.trace_id());
        assert_eq!(record.events.len(), MAX_EVENTS);
        assert_eq!(record.events.last().unwrap().code, "timeout");
        assert!(!std::fs::read_to_string(dir.path().join(FAILURE_FILE))
            .unwrap()
            .contains("secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join(FAILURE_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let export = dir.path().join("not-a-directory");
        std::fs::write(&export, b"x").unwrap();
        assert!(journal.save_failure(&export, "timeout").is_err());
    }

    #[test]
    fn ignores_untrusted_or_expired_history() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_failure(dir.path()).is_none());
        let path = dir.path().join(FAILURE_FILE);
        for bytes in [b"bad".to_vec(), vec![b'x'; MAX_BYTES as usize + 1]] {
            std::fs::write(&path, bytes).unwrap();
            assert!(load_failure(dir.path()).is_none());
        }
        let journal = Journal::default();
        journal.save_failure(dir.path(), "timeout").unwrap();
        let mut record = load_failure(dir.path()).unwrap();
        record.trace_id = "invalid".into();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(load_failure(dir.path()).is_none());
        record.recorded_at_unix_ms = 0;
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(load_failure(dir.path()).is_none());
        assert!(!path.exists());
    }
}
