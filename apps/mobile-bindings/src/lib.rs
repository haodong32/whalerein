//! UniFFI mobile binding for WhaleRein Core.
//!
//! Swift imports the generated `WhaleReinMobile` module. The facade is
//! [`MobileCore`]; [`SecretStore`] is implemented by the app with the iOS
//! Keychain. All exported operations are synchronous and safe to call from the
//! main thread; background work runs on the binding's own Tokio runtime.

uniffi::setup_scaffolding!("WhaleReinMobile");

mod client;
mod error;
mod secret;
mod types;

pub use client::{core_revision, MobileCore};
pub use error::MobileError;
pub use secret::{MemorySecretStore, SecretStore, SecretStoreError};
pub use types::{
    ApprovalView, BlockView, CommandView, ConversationEvent, DeliveryChoice, DiagnosticsReportView,
    HostSummary, MessageView, PairingPromptView, QuestionAnswerView, QuestionBatchView,
    QuestionIntentView, QuestionItemView, QuestionOptionView, QuestionPageView, QuestionStampView,
    QuestionSubjectView, QuestionsView, SessionCapabilitiesView, SessionSummaryView,
};
