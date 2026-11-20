//! UniFFI mobile binding for WhaleRein Core.
//!
//! Swift imports the generated `WhaleReinMobile` module. The facade is
//! [`MobileCore`]; [`SecretStore`] is implemented by the app with the iOS
//! Keychain. All exported operations are synchronous and safe to call from the
//! main thread; background work runs on the binding's own Tokio runtime.

uniffi::setup_scaffolding!("WhaleReinMobile");

mod app_config;
mod client;
mod error;
mod secret;
mod types;

pub use app_config::{
    app_config_bootstrap, app_config_read, app_config_remove_ca, app_config_remove_instance,
    app_config_save_instance, app_config_set_active, app_config_set_reasoning_visible,
    app_config_validate_instance, AppConfigView, AppSettingsView, InstanceInput, InstanceView,
};
pub use client::{
    core_revision, write_client_config, MobileClientService, MobileCloudEnvironment, MobileCore,
};
pub use error::MobileError;
pub use secret::{MemorySecretStore, SecretStore, SecretStoreError};
pub use types::{
    ApprovalView, AttachmentDownloadRequestView, AttachmentDownloadView, AttachmentProgressView,
    AttachmentReferenceView, AttachmentTransferView, AttachmentView, BlockView, ClientStatusView,
    CommandView, ConversationEvent, DeliveryChoice, DiagnosticsReportView, HistoryPageView,
    HostSummary, MessageReferenceView, MessageReplyView, MessageView, NoticeView,
    PairingPromptView, QuestionAnswerView, QuestionBatchView, QuestionIntentView, QuestionItemView,
    QuestionOptionView, QuestionPageView, QuestionStampView, QuestionSubjectView, QuestionsView,
    RetainedOperationView, SendOperationView, SessionCapabilitiesView, SessionKeyView,
    SessionSummaryView, ToolActivityView, ToolResultPartView,
};
