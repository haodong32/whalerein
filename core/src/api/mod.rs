//! Public Core API.
//!
//! CLI and future bindings call only this module. HTTP DTOs, MQTT topics and
//! Noise messages stay behind it.
//!
//! Apps open an instance with [`Core::open`] and submit sends through
//! [`Core::submit_message_operation`]. Previous client entry points are
//! rejected by the public API.
//!
//! ```compile_fail,E0432
//! use whalerein_core::CoreOptions;
//! ```
//!
//! ```compile_fail,E0432
//! use whalerein_core::Profiles;
//! ```
//!
//! ```compile_fail,E0599
//! use whalerein_core::api::{Core, SessionKey};
//! fn submit(core: &Core, target: &SessionKey) {
//!     let _ = core.send_message(target, "fixture");
//! }
//! ```
//!
//! ```compile_fail,E0599
//! use whalerein_core::api::Core;
//! fn recover(core: &Core) {
//!     let _ = core.resume_pairing();
//! }
//! ```

pub(crate) mod config;
pub(crate) mod core;
mod error;
pub(crate) mod instance;
mod proto_compat;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod types;

pub use crate::access::transport::{
    resolve_transport_settings, TransportBudget, TransportOverrides, TransportProfile,
    TransportSettings,
};
pub use crate::account::native::{
    android_redirect_uri, AccountHost, AccountIdentity, AccountLoginPrompt, AccountSessionInfo,
    AccountSessionState, AccountWorkspace, CloudAuthAttempt, CloudAuthPlatform, SignedInAccount,
    ANDROID_REDIRECT_URI,
};
pub use crate::account::Environment;
#[cfg(test)]
pub use crate::account::{
    AccountBinding, CustomServiceInput, ProfileMode, ServiceDescriptor, ServiceKind,
};
pub use crate::attachments::{
    AttachmentLimits, AttachmentReference, DownloadOutcome, DownloadProgress, DownloadRequest,
};
pub use crate::connection::ConnectionSnapshot;
pub use crate::diagnostics::journal::{
    DiagnosticContext, DiagnosticEvent, DiagnosticKind, DiagnosticLevel, DiagnosticSink,
};
pub use crate::diagnostics::MetricSample;
pub use crate::observation::SessionObservation;
pub use crate::pairing::{PairingDecision, PairingError};
#[cfg(test)]
pub use crate::profiles::{
    ActivationOutcome, ProfileContext, ProfileSelection, ProfileSummary, Profiles, RemovalOutcome,
};
pub use crate::runtime::ConnectionState;
#[cfg(unix)]
pub use crate::security::FileSecretStore;
#[cfg(feature = "keyring")]
pub use crate::security::KeyringSecretStore;
pub use crate::security::{MemorySecretStore, SecretError, SecretStore};
pub use crate::session::SessionKey;
#[cfg(test)]
pub use config::default_data_dir;
pub use core::cloud_pairing::{CloudRevocationOutcome, CloudRevocationState};
#[cfg(test)]
pub use core::CoreOptions;
pub use core::{
    AccountState, ClientStatus, Core, DeviceAccess, ForkOutcome, SendFollowUp, SendOperation,
    SendReply,
};
pub use error::{error_code_action, error_code_exit_code, error_code_name, CoreError, CoreResult};
pub use instance::{ClientConfig, ClientService};
pub use proto_compat::{
    decode_json, protocol_incompatible, require_payload, require_protocol_version, wire_error,
};
pub use types::{
    ApprovalInfo, Cancellation, CommandDiagnostics, CommandHandle, CreateSessionInput,
    DoctorReport, ExportManifest, HistoryContinuity, HistorySnapshot, HostCapabilities, HostInfo,
    InputKind, LifecycleReceipt, ModelCatalog, ModelChoice, ModelSelectionReceipt,
    ObservationInterests, OnlineStatus, PairingHistoryRecord, PairingOutcome, PairingPrompt,
    PendingCommand, PendingQueue, PendingQueueAction, PendingQueueBlock, PendingQueueItem,
    QuestionAnswer, QuestionBatchInfo, QuestionFreshness, QuestionItemInfo, QuestionOptionInfo,
    QuestionPageInfo, QuestionReceiptInfo, QuestionStamp, QuestionView, RevocationOutcome,
    RevocationRetention, SessionCapabilities, SessionEvent, SessionEventPayload, SessionList,
    SessionModel, SessionSummary, WaitInput, WaitObservation, WaitReason, WaitState,
};

// Delivery choices are Core-owned; wire conversion stays internal.
pub use types::{DeliveryMode, QuestionPhase};

/// Existing Host control request lifetime used for bounded cancellation.
pub const CONTROL_REQUEST_TTL_MS: u64 = crate::commands::REQUEST_TTL_MS as u64;
