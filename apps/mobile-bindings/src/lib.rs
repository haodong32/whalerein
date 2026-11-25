//! UniFFI mobile binding for WhaleRein Core.
//!
//! Swift imports the generated `WhaleReinMobile` module. The facade is
//! [`MobileCore`]; [`SecretStore`] is implemented by the app with the iOS
//! Keychain. All exported operations are synchronous and safe to call from the
//! main thread; background work runs on the binding's own Tokio runtime.

uniffi::setup_scaffolding!("WhaleReinMobile");

mod app_config;
mod client;
mod diagnostics;
mod error;
mod queue;
mod secret;
mod session_management;
mod types;
mod wait;

pub use app_config::{
    app_config_bootstrap, app_config_read, app_config_remove_ca, app_config_remove_instance,
    app_config_save_instance, app_config_set_active, app_config_set_reasoning_visible,
    app_config_validate_instance, AppConfigView, AppSettingsView, InstanceInput, InstanceView,
};
pub use client::{
    android_redirect_uri, core_revision, new_operation_id, write_client_config,
    MobileClientService, MobileCloudEnvironment, MobileCore,
};
pub use diagnostics::{CoreDiagnosticSink, DiagnosticContext, DiagnosticEventView};
pub use error::MobileError;
pub use queue::{
    PendingQueueBlockView, PendingQueueItemView, PendingQueueView, QueueAction, QueueMutationView,
};
pub use secret::{MemorySecretStore, SecretStore, SecretStoreError};
pub use session_management::{
    ForkResultView, ModelCatalogView, ModelChoiceView, ModelEffortView, ModelEntryView,
    ModelProviderFailureView, ModelProviderView, ModelSelectionResultView, SessionCreateInputView,
    SessionCreateResultView, SessionDetailView, SessionModelView,
};
pub use types::{
    ApprovalView, AttachmentDownloadRequestView, AttachmentDownloadView, AttachmentProgressView,
    AttachmentReferenceView, AttachmentTransferView, AttachmentView, AttemptRefView, BlockView,
    ClientStatusView, CloudAccountSessionView, CloudAccountStatusView, CloudAccountView,
    CloudAuthAttemptView, CloudConnectResultView, CloudDirectoryHostView, CommandView,
    ConversationEvent, ConversationEventEnvelope, DeliveryChoice, DiagnosticsReportView,
    HistoryContinuityView, HistoryPageView, HostSummary, MessageReferenceView, MessageReplyView,
    MessageView, NoticeView, PairingPromptView, PreviewPrefixView, QuestionAnswerView,
    QuestionBatchView, QuestionCancelReceiptView, QuestionIntentView, QuestionItemView,
    QuestionOptionView, QuestionPageView, QuestionStampView, QuestionSubjectView, QuestionsView,
    RetainedOperationView, SendOperationView, SessionCapabilitiesView, SessionKeyView,
    SessionSummaryView, ToolActivityView, ToolResultPartView, WatchEventScope,
    WatchHistoryBaseline,
};
pub use wait::{WaitOutcome, WaitResultView, WaitStateView};
