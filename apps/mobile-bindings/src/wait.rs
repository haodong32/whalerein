//! Background native wait attempts with independent cancellation.
//!
//! Core's `wait_session` is a blocking observation. The binding runs each
//! attempt on its own Tokio task, keeps a stable attempt identity, and exposes
//! a bounded take call. Cancelling one attempt aborts only that task; Core's
//! observation guard then sends the scoped `CancelSessionWait` and the
//! conversation watch is untouched.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::task::AbortHandle;
use whalerein_core::api::{Core, InputKind, SessionKey, WaitObservation, WaitReason, WaitState};
use whalerein_core::CoreError;

use crate::error::MobileError;

/// Core rejects a wait timeout outside this timer range; validate before a
/// background task exists so a bad request never leaves an attempt behind.
pub(crate) const MAX_WAIT_TIMEOUT_MS: u64 = 2_147_483_647;
/// Bound one `takeWaitResult` block. The native attempt keeps its own deadline.
const MAX_TAKE_WAIT_MS: u64 = 3_600_000;

/// State-only session observation returned by a native wait.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WaitStateView {
    pub active: bool,
    pub running: bool,
    pub queue_available: bool,
    /// `approval`, `question` or `other`, in Core's evidenced order.
    pub pending_input: Vec<String>,
}

impl From<WaitState> for WaitStateView {
    fn from(value: WaitState) -> Self {
        Self {
            active: value.active,
            running: value.running,
            queue_available: value.queue_available,
            pending_input: value.pending_input.into_iter().map(input_name).collect(),
        }
    }
}

fn input_name(value: InputKind) -> String {
    match value {
        InputKind::Approval => "approval",
        InputKind::Question => "question",
        InputKind::Other => "other",
    }
    .to_owned()
}

/// Typed stopping condition of one wait attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WaitOutcome {
    Idle,
    InputRequired,
    /// The attempt's own deadline expired. `state` carries the last observation
    /// when Core had one.
    Timeout,
    /// The caller cancelled the attempt before it settled.
    Cancelled,
    Failed,
}

/// One settled wait attempt. `state` is absent only when no state was observed
/// (a cancelled attempt or a failure before the first slice).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WaitResultView {
    pub wait_id: String,
    pub host_id: String,
    pub session_id: String,
    pub outcome: WaitOutcome,
    pub state: Option<WaitStateView>,
    /// Stable binding/Core error code for `Failed`; `cancelled` for a cancelled
    /// attempt; absent otherwise.
    pub error_code: Option<String>,
}

fn result_for(
    wait_id: &str,
    key: &SessionKey,
    outcome: WaitOutcome,
    state: Option<WaitStateView>,
    error_code: Option<String>,
) -> WaitResultView {
    WaitResultView {
        wait_id: wait_id.to_owned(),
        host_id: key.host_id.clone(),
        session_id: key.session_id.clone(),
        outcome,
        state,
        error_code,
    }
}

fn map_error(error: CoreError) -> WaitResultViewOutcome {
    match error {
        CoreError::Timeout => WaitResultViewOutcome::timeout(),
        other => WaitResultViewOutcome::failed(MobileError::from(other).code().to_owned()),
    }
}

struct WaitResultViewOutcome {
    outcome: WaitOutcome,
    state: Option<WaitStateView>,
    error_code: Option<String>,
}

impl WaitResultViewOutcome {
    fn timeout() -> Self {
        Self {
            outcome: WaitOutcome::Timeout,
            state: None,
            error_code: None,
        }
    }

    fn failed(code: String) -> Self {
        Self {
            outcome: WaitOutcome::Failed,
            state: None,
            error_code: Some(code),
        }
    }
}

fn map_observation(observation: WaitObservation) -> WaitResultViewOutcome {
    let state = Some(WaitStateView::from(observation.state));
    let outcome = match observation.reason {
        WaitReason::Idle => WaitOutcome::Idle,
        WaitReason::InputRequired => WaitOutcome::InputRequired,
        WaitReason::Timeout => WaitOutcome::Timeout,
    };
    WaitResultViewOutcome {
        outcome,
        state,
        error_code: None,
    }
}

/// Sends a `Cancelled` result if the task is dropped before it settles. The
/// guard owns the sender so taking the sender for a real result disarms it.
struct ResultSender {
    sender: Option<Sender<WaitResultView>>,
    cancelled: WaitResultView,
}

impl Drop for ResultSender {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(self.cancelled.clone());
        }
    }
}

struct WaitAttempt {
    receiver: Mutex<Receiver<WaitResultView>>,
    abort: AbortHandle,
    finished: Arc<AtomicBool>,
}

/// In-process registry of background wait attempts.
#[derive(Default)]
pub(crate) struct WaitAttempts {
    attempts: Mutex<HashMap<String, Arc<WaitAttempt>>>,
}

impl WaitAttempts {
    /// Start one background wait and return its stable identity.
    pub(crate) fn begin(
        &self,
        core: Core,
        handle: &Handle,
        key: SessionKey,
        timeout_ms: u64,
    ) -> String {
        let wait_id = Core::new_command_id();
        let (sender, receiver) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let task_finished = finished.clone();
        let task_key = key.clone();
        let task_id = wait_id.clone();
        let guard = ResultSender {
            sender: Some(sender),
            cancelled: result_for(
                &task_id,
                &task_key,
                WaitOutcome::Cancelled,
                None,
                Some("cancelled".into()),
            ),
        };
        let join = handle.spawn(async move {
            let mut guard = guard;
            let outcome = match core.wait_session(&task_key, timeout_ms).await {
                Ok(observation) => map_observation(observation),
                Err(error) => map_error(error),
            };
            let result = result_for(
                &task_id,
                &task_key,
                outcome.outcome,
                outcome.state,
                outcome.error_code,
            );
            task_finished.store(true, Ordering::SeqCst);
            if let Some(sender) = guard.sender.take() {
                let _ = sender.send(result);
            }
        });
        let attempt = Arc::new(WaitAttempt {
            receiver: Mutex::new(receiver),
            abort: join.abort_handle(),
            finished,
        });
        let mut attempts = self
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Bound abandoned attempts: an app that starts a wait per redraw must
        // not grow this registry without limit. Old settled entries have no
        // observer left; an in-flight one is aborted and drops its guard.
        if attempts.len() >= 32 {
            let stale: Vec<String> = attempts.keys().take(attempts.len() - 31).cloned().collect();
            for id in stale {
                if let Some(old) = attempts.remove(&id) {
                    old.abort.abort();
                }
            }
        }
        attempts.insert(wait_id.clone(), attempt);
        wait_id
    }

    /// Block up to `wait_ms` for a settled result. `None` means the native
    /// attempt is still running; its identity stays valid for a later take.
    pub(crate) fn take(
        &self,
        wait_id: &str,
        wait_ms: u64,
    ) -> Result<Option<WaitResultView>, MobileError> {
        let attempt = self
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(wait_id)
            .cloned()
            .ok_or_else(MobileError::not_found)?;
        let receiver = attempt
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match receiver.recv_timeout(Duration::from_millis(wait_ms.min(MAX_TAKE_WAIT_MS))) {
            Ok(result) => {
                self.remove(wait_id);
                Ok(Some(result))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                self.remove(wait_id);
                Err(MobileError::internal())
            }
        }
    }

    /// Abort only this attempt. Returns whether an unsettled attempt existed.
    pub(crate) fn cancel(&self, wait_id: &str) -> bool {
        let attempt = self
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(wait_id)
            .cloned();
        let Some(attempt) = attempt else {
            return false;
        };
        if attempt.finished.load(Ordering::SeqCst) {
            return false;
        }
        attempt.abort.abort();
        true
    }

    /// Abort every attempt when the owning MobileCore is dropped.
    pub(crate) fn cancel_all(&self) {
        let attempts = self
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for attempt in attempts.values() {
            attempt.abort.abort();
        }
    }

    fn remove(&self, wait_id: &str) {
        self.attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(wait_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(active: bool, running: bool, queue: bool, input: Vec<InputKind>) -> WaitState {
        WaitState {
            active,
            running,
            queue_available: queue,
            pending_input: input,
        }
    }

    #[test]
    fn maps_state_and_pending_input() {
        let view = WaitStateView::from(state(
            true,
            true,
            true,
            vec![InputKind::Question, InputKind::Approval, InputKind::Other],
        ));
        assert!(view.active);
        assert!(view.running);
        assert!(view.queue_available);
        assert_eq!(view.pending_input, vec!["question", "approval", "other"]);
    }

    #[test]
    fn maps_each_wait_reason_to_a_typed_outcome() {
        for (reason, expected) in [
            (WaitReason::Idle, WaitOutcome::Idle),
            (WaitReason::InputRequired, WaitOutcome::InputRequired),
            (WaitReason::Timeout, WaitOutcome::Timeout),
        ] {
            let mapped = map_observation(WaitObservation {
                session_id: "session-1".into(),
                reason,
                state: state(false, false, false, Vec::new()),
            });
            assert_eq!(mapped.outcome, expected);
            assert!(mapped.state.is_some());
            assert!(mapped.error_code.is_none());
        }
    }

    #[test]
    fn a_bare_core_timeout_is_a_timeout_not_a_failure() {
        let mapped = map_error(CoreError::Timeout);
        assert_eq!(mapped.outcome, WaitOutcome::Timeout);
        assert!(mapped.state.is_none());
        assert!(mapped.error_code.is_none());
    }

    #[test]
    fn transport_errors_keep_their_stable_code() {
        let mapped = map_error(CoreError::ChannelMismatch);
        assert_eq!(mapped.outcome, WaitOutcome::Failed);
        assert!(mapped.error_code.as_deref().is_some_and(|code| !code.is_empty()));
        assert!(mapped.state.is_none());
    }

    #[test]
    fn wait_timeout_bounds_match_core() {
        assert_eq!(MAX_WAIT_TIMEOUT_MS, 2_147_483_647);
        assert!(MAX_TAKE_WAIT_MS >= 1_000);
    }
}
