//! Public Core API.
//!
//! CLI and future bindings call only this module. HTTP DTOs, MQTT topics and
//! Noise messages stay behind it.

pub(crate) mod config;
pub(crate) mod core;
mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod types;

pub use crate::diagnostics::MetricSample;
pub use crate::pairing::{PairingDecision, PairingError};
pub use crate::runtime::ConnectionState;
#[cfg(feature = "keyring")]
pub use crate::security::KeyringSecretStore;
pub use crate::security::{FileSecretStore, MemorySecretStore, SecretError, SecretStore};
pub use config::default_data_dir;
pub use core::{Core, CoreOptions};
pub use error::{error_code_exit_code, error_code_name, CoreError, CoreResult};
pub use types::{
    ApprovalInfo, CommandDiagnostics, CommandHandle, CreateSessionInput, DoctorReport,
    ExportManifest, HostCapabilities, HostInfo, LifecycleReceipt, ModelCatalog, ModelChoice,
    ModelSelectionReceipt, OnlineStatus, PairingHistoryRecord, PairingOutcome, PairingPrompt,
    PendingCommand, PendingQueue, PendingQueueAction, QuestionAnswer, QuestionBatchInfo,
    QuestionItemInfo, QuestionOptionInfo, QuestionPageInfo, QuestionReceiptInfo, RevocationOutcome,
    RevocationRetention, SessionCapabilities, SessionList, SessionModel, SessionSummary, WaitInput,
    WaitObservation, WaitReason, WaitState,
};

// Delivery choices are shared with the versioned wire contract.
pub use whalerein_proto::DeliveryMode;

/// Existing Host control request lifetime used for bounded cancellation.
pub const CONTROL_REQUEST_TTL_MS: u64 = crate::commands::REQUEST_TTL_MS as u64;
