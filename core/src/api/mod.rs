//! Public Core API.
//!
//! CLI and future bindings call only this module. HTTP DTOs, MQTT topics and
//! Noise messages stay behind it.

pub(crate) mod config;
mod core;
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
    ApprovalInfo, CommandDiagnostics, CommandHandle, DoctorReport, ExportManifest, HostInfo,
    OnlineStatus, PairingOutcome, PairingPrompt, PendingCommand, SessionCapabilities, SessionList,
    SessionSummary,
};
