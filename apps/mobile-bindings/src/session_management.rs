//! Typed session-management projections for the iOS client.
//!
//! These values wrap Core's existing session creation, fork, details and model
//! methods. They add no native behavior: every disposition, warning, reason code
//! and fallback value is Core's, and a missing lifecycle receipt stays explicit
//! instead of being presented as a successful create or fork.

use whalerein_core::api::{
    CommandHandle, Core, CoreError, CreateSessionInput, ForkOutcome, LifecycleReceipt,
    ModelCatalog, ModelSelectionReceipt, SessionKey, SessionModel, SessionSummary,
};

use crate::error::MobileError;
use crate::types::{CommandView, SessionCapabilitiesView};

/// Matches the binding's general identifier budget.
const MAX_ID_BYTES: usize = 512;
/// Host paths may be longer than identifiers.
const MAX_PATH_BYTES: usize = 4096;

/// One session summary with its Host-qualified identity.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionDetailView {
    pub host_id: String,
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub running: bool,
    pub agent_available: bool,
    pub blank: bool,
    pub parent_session_id: String,
    pub origin: String,
    pub history_cursor: String,
    pub queued_prompts: u32,
    /// The Source capabilities reported on this summary. `None` means the
    /// Source did not report them; callers must not synthesize defaults.
    pub capabilities: Option<SessionCapabilitiesView>,
}

impl SessionDetailView {
    fn from_summary(host_id: &str, summary: SessionSummary) -> Self {
        Self {
            host_id: host_id.to_owned(),
            session_id: summary.session_id,
            title: summary.title,
            cwd: summary.cwd,
            created_at_unix_ms: summary.created_at_unix_ms,
            updated_at_unix_ms: summary.updated_at_unix_ms,
            running: summary.running,
            agent_available: summary.agent_available,
            blank: summary.blank,
            parent_session_id: summary.parent_session_id,
            origin: summary.origin,
            history_cursor: summary.history_cursor,
            queued_prompts: summary.queued_prompts,
            capabilities: summary.source_capabilities.map(Into::into),
        }
    }
}

/// Optional Host-owned inputs for session creation. Every value is passed to
/// Core unchanged; Core and the Host decide default location and identity.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionCreateInputView {
    pub cwd: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_preset: Option<String>,
    pub preferred_session_id: Option<String>,
}

impl From<SessionCreateInputView> for CreateSessionInput {
    fn from(value: SessionCreateInputView) -> Self {
        Self {
            cwd: value.cwd,
            workspace_id: value.workspace_id,
            agent_preset: value.agent_preset,
            preferred_session_id: value.preferred_session_id,
        }
    }
}

/// Lifecycle fields shared by create and fork receipts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct LifecycleFields {
    session_id: Option<String>,
    parent_session_id: Option<String>,
    disposition: String,
    warning_code: Option<String>,
    operation_id: Option<String>,
    phase: Option<String>,
    reason_code: Option<String>,
    retryable: bool,
    reconcile_mode: Option<String>,
    reconcile_key: Option<String>,
}

impl LifecycleFields {
    fn from_receipt(receipt: Option<&LifecycleReceipt>) -> Self {
        let Some(receipt) = receipt else {
            return Self {
                disposition: "UNKNOWN".into(),
                ..Self::default()
            };
        };
        Self {
            session_id: (!receipt.session_id.is_empty()).then(|| receipt.session_id.clone()),
            parent_session_id: receipt.parent_session_id.clone(),
            disposition: receipt.disposition.clone(),
            warning_code: receipt.warning_code.clone(),
            operation_id: receipt.operation_id.clone(),
            phase: receipt.phase.clone(),
            reason_code: receipt.reason_code.clone(),
            retryable: receipt.retryable,
            reconcile_mode: receipt.reconcile_mode.clone(),
            reconcile_key: receipt.reconcile_key.clone(),
        }
    }
}

/// Outcome of one create submission, including the typed command receipt.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionCreateResultView {
    pub receipt: CommandView,
    /// The session the Host returned. `None` for a proven failure or an
    /// unreconciled result; the caller must not invent an identity.
    pub session_id: Option<String>,
    pub disposition: String,
    pub warning_code: Option<String>,
    pub operation_id: Option<String>,
    pub phase: Option<String>,
    pub reason_code: Option<String>,
    pub retryable: bool,
    pub reconcile_mode: Option<String>,
    pub reconcile_key: Option<String>,
}

impl SessionCreateResultView {
    fn from_handle(handle: CommandHandle) -> Self {
        let fields = LifecycleFields::from_receipt(handle.lifecycle_receipt.as_ref());
        Self {
            receipt: CommandView::from(handle),
            session_id: fields.session_id,
            disposition: fields.disposition,
            warning_code: fields.warning_code,
            operation_id: fields.operation_id,
            phase: fields.phase,
            reason_code: fields.reason_code,
            retryable: fields.retryable,
            reconcile_mode: fields.reconcile_mode,
            reconcile_key: fields.reconcile_key,
        }
    }
}

/// Outcome of one fork, including the child identity and working directory.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ForkResultView {
    pub receipt: CommandView,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub disposition: String,
    pub warning_code: Option<String>,
    pub operation_id: Option<String>,
    pub phase: Option<String>,
    pub reason_code: Option<String>,
    pub retryable: bool,
    pub reconcile_mode: Option<String>,
    pub reconcile_key: Option<String>,
    /// Confirmed child working directory. Fork shares the parent directory;
    /// `None` means the second read did not supply it.
    pub working_directory: Option<String>,
    /// Why the directory could not be resolved. The fork itself still
    /// succeeded and must not be retried.
    pub working_directory_error: Option<String>,
}

impl ForkResultView {
    fn from_outcome(outcome: ForkOutcome) -> Self {
        let fields = LifecycleFields::from_receipt(outcome.handle.lifecycle_receipt.as_ref());
        Self {
            receipt: CommandView::from(outcome.handle),
            session_id: fields.session_id,
            parent_session_id: fields.parent_session_id,
            disposition: fields.disposition,
            warning_code: fields.warning_code,
            operation_id: fields.operation_id,
            phase: fields.phase,
            reason_code: fields.reason_code,
            retryable: fields.retryable,
            reconcile_mode: fields.reconcile_mode,
            reconcile_key: fields.reconcile_key,
            working_directory: outcome.working_directory,
            working_directory_error: outcome.working_directory_error,
        }
    }
}

/// One concrete provider/model/effort choice.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelChoiceView {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
}

impl From<whalerein_core::api::ModelChoice> for ModelChoiceView {
    fn from(value: whalerein_core::api::ModelChoice) -> Self {
        Self {
            provider: value.provider,
            model: value.model,
            effort: value.effort,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelEffortView {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelEntryView {
    pub id: String,
    pub name: String,
    pub efforts: Vec<ModelEffortView>,
    pub default_effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelProviderView {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelEntryView>,
}

/// A provider the Source could not read. The remaining providers stay usable.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelProviderFailureView {
    pub id: String,
    pub name: String,
    pub code: String,
}

/// Authoritative source model catalog. `complete` is false when any provider
/// failed; successful providers are still listed.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelCatalogView {
    pub default_selection: Option<ModelChoiceView>,
    pub providers: Vec<ModelProviderView>,
    pub failures: Vec<ModelProviderFailureView>,
    pub complete: bool,
    pub effect_scope: String,
}

impl From<ModelCatalog> for ModelCatalogView {
    fn from(value: ModelCatalog) -> Self {
        Self {
            default_selection: value.default_selection.map(Into::into),
            providers: value
                .groups
                .into_iter()
                .map(|provider| ModelProviderView {
                    id: provider.id,
                    name: provider.name,
                    models: provider
                        .models
                        .into_iter()
                        .map(|model| ModelEntryView {
                            id: model.id,
                            name: model.name,
                            efforts: model
                                .efforts
                                .into_iter()
                                .map(|effort| ModelEffortView {
                                    id: effort.id,
                                    name: effort.name,
                                })
                                .collect(),
                            default_effort: model.default_effort,
                        })
                        .collect(),
                })
                .collect(),
            failures: value
                .failures
                .into_iter()
                .map(|failure| ModelProviderFailureView {
                    id: failure.id,
                    name: failure.name,
                    code: failure.code,
                })
                .collect(),
            complete: value.complete,
            effect_scope: value.effect_scope,
        }
    }
}

/// Passive session model observation. `next`, `last_used`, `pending` and the
/// source default stay distinct; `pending_known` is false when the Source did
/// not report a pending choice.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionModelView {
    pub pending: Option<ModelChoiceView>,
    pub last_used: Option<ModelChoiceView>,
    pub default_selection: Option<ModelChoiceView>,
    pub default_is_fallback: bool,
    pub next: Option<ModelChoiceView>,
    pub pending_known: bool,
}

impl From<SessionModel> for SessionModelView {
    fn from(value: SessionModel) -> Self {
        Self {
            pending: value.pending.map(Into::into),
            last_used: value.last_used.map(Into::into),
            default_selection: value.default_selection.map(Into::into),
            default_is_fallback: value.default_is_fallback,
            next: value.next.map(Into::into),
            pending_known: value.pending_known,
        }
    }
}

/// Native selection result. Session acceptance and source default persistence
/// are separate fields; an empty `receipt.command_id` means Core treated the
/// selection as a no-op and journaled no command.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelSelectionResultView {
    pub receipt: CommandView,
    pub selected: Option<ModelChoiceView>,
    pub effect_scope: Option<String>,
    pub default_persistence: Option<String>,
}

impl ModelSelectionResultView {
    fn from_receipt(receipt: ModelSelectionReceipt, handle: CommandHandle) -> Self {
        Self {
            receipt: CommandView::from(handle),
            selected: receipt.selected.map(Into::into),
            effect_scope: Some(receipt.effect_scope),
            default_persistence: Some(receipt.default_persistence),
        }
    }

    fn without_receipt(handle: CommandHandle) -> Self {
        Self {
            receipt: CommandView::from(handle),
            selected: None,
            effect_scope: None,
            default_persistence: None,
        }
    }
}

#[cfg(test)]
fn validate_id(value: &str) -> Result<(), MobileError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.chars().any(|character| character.is_control())
    {
        return Err(MobileError::invalid_input());
    }
    Ok(())
}

fn validate_optional(value: &Option<String>, limit: usize) -> Result<(), MobileError> {
    if let Some(value) = value {
        if value.len() > limit || value.chars().any(|character| character.is_control()) {
            return Err(MobileError::invalid_input());
        }
    }
    Ok(())
}

/// Validate optional create values against the transport budget and convert
/// them to Core's typed input.
///
/// Blank values, relative paths and a working directory combined with a
/// workspace are rejected by Core's own preflight; do not copy those rules here.
pub(crate) fn create_input(
    input: SessionCreateInputView,
) -> Result<CreateSessionInput, MobileError> {
    validate_optional(&input.cwd, MAX_PATH_BYTES)?;
    validate_optional(&input.workspace_id, MAX_ID_BYTES)?;
    validate_optional(&input.agent_preset, MAX_ID_BYTES)?;
    validate_optional(&input.preferred_session_id, MAX_ID_BYTES)?;
    Ok(input.into())
}

pub(crate) fn key(host_id: &str, session_id: &str) -> SessionKey {
    SessionKey {
        host_id: host_id.to_owned(),
        session_id: session_id.to_owned(),
    }
}

pub(crate) async fn create_session(
    core: &Core,
    host_id: &str,
    input: CreateSessionInput,
) -> Result<SessionCreateResultView, CoreError> {
    let handle = core.create_session(host_id, input).await?;
    Ok(SessionCreateResultView::from_handle(handle))
}

pub(crate) async fn session_details(
    core: &Core,
    host_id: &str,
    key: &SessionKey,
) -> Result<SessionDetailView, CoreError> {
    let summary = core.get_session(key).await?;
    Ok(SessionDetailView::from_summary(host_id, summary))
}

pub(crate) async fn fork_session_outcome(
    core: &Core,
    key: &SessionKey,
) -> Result<ForkResultView, CoreError> {
    let outcome = core.fork_session_outcome(key).await?;
    Ok(ForkResultView::from_outcome(outcome))
}

pub(crate) async fn model_catalog(
    core: &Core,
    host_id: &str,
) -> Result<ModelCatalogView, CoreError> {
    let catalog = core.list_models(host_id).await?;
    Ok(ModelCatalogView::from(catalog))
}

pub(crate) async fn read_session_model(
    core: &Core,
    key: &SessionKey,
) -> Result<SessionModelView, CoreError> {
    let model = core.read_session_model(key).await?;
    Ok(SessionModelView::from(model))
}

pub(crate) async fn select_session_model(
    core: &Core,
    key: &SessionKey,
    provider: &str,
    model: &str,
    effort: Option<String>,
) -> Result<ModelSelectionResultView, CoreError> {
    let handle = core
        .select_session_model(key, provider, model, effort.as_deref())
        .await?;
    Ok(match handle.model_selection.clone() {
        Some(receipt) => ModelSelectionResultView::from_receipt(receipt, handle),
        None => ModelSelectionResultView::without_receipt(handle),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_core::api::{ModelChoice, OnlineStatus};

    fn handle(lifecycle: Option<LifecycleReceipt>) -> CommandHandle {
        CommandHandle {
            request_id: "request-1".into(),
            command_id: "command-1".into(),
            status: OnlineStatus::Succeeded,
            message_id: None,
            delivery_id: None,
            model_selection: None,
            lifecycle_receipt: lifecycle,
            question_receipt: None,
        }
    }

    fn lifecycle(session_id: &str, disposition: &str) -> LifecycleReceipt {
        LifecycleReceipt {
            session_id: session_id.into(),
            parent_session_id: None,
            operation: "create".into(),
            disposition: disposition.into(),
            warning_code: None,
            operation_id: Some("operation-1".into()),
            phase: Some("COMPLETED".into()),
            reason_code: None,
            retryable: false,
            reconcile_mode: None,
            reconcile_key: None,
            warning_detail: None,
        }
    }

    // AC-01: ready, warning, failure and unknown stay distinct.
    #[test]
    fn create_result_maps_every_lifecycle_disposition() {
        let ready =
            SessionCreateResultView::from_handle(handle(Some(lifecycle("session-new", "READY"))));
        assert_eq!(ready.session_id.as_deref(), Some("session-new"));
        assert_eq!(ready.disposition, "READY");
        assert_eq!(ready.operation_id.as_deref(), Some("operation-1"));
        assert_eq!(ready.phase.as_deref(), Some("COMPLETED"));
        assert!(!ready.retryable);
        assert_eq!(ready.receipt.command_id, "command-1");

        let mut warning = lifecycle("session-new", "READY_WITH_WARNING");
        warning.warning_code = Some("PRESET_IGNORED".into());
        let warned = SessionCreateResultView::from_handle(handle(Some(warning)));
        assert_eq!(warned.disposition, "READY_WITH_WARNING");
        assert_eq!(warned.warning_code.as_deref(), Some("PRESET_IGNORED"));
        assert_eq!(warned.session_id.as_deref(), Some("session-new"));

        let mut failed = lifecycle("", "FAILED");
        failed.reason_code = Some("WORKSPACE_NOT_FOUND".into());
        failed.retryable = true;
        let rejected = SessionCreateResultView::from_handle(handle(Some(failed)));
        assert_eq!(rejected.session_id, None);
        assert_eq!(rejected.disposition, "FAILED");
        assert_eq!(rejected.reason_code.as_deref(), Some("WORKSPACE_NOT_FOUND"));
        assert!(rejected.retryable);

        let mut unknown = lifecycle("", "UNKNOWN");
        unknown.reason_code = Some("NATIVE_INTERRUPTED".into());
        unknown.reconcile_mode = Some("PREFERRED_SESSION_ID".into());
        unknown.reconcile_key = Some("session-x".into());
        let unreconciled = SessionCreateResultView::from_handle(handle(Some(unknown)));
        assert_eq!(unreconciled.session_id, None);
        assert_eq!(unreconciled.disposition, "UNKNOWN");
        assert_eq!(
            unreconciled.reconcile_mode.as_deref(),
            Some("PREFERRED_SESSION_ID")
        );
        assert_eq!(unreconciled.reconcile_key.as_deref(), Some("session-x"));
    }

    #[test]
    fn create_without_a_lifecycle_receipt_reports_no_identity() {
        let result = SessionCreateResultView::from_handle(handle(None));
        assert_eq!(result.session_id, None);
        assert_eq!(result.disposition, "UNKNOWN");
        assert_eq!(result.warning_code, None);
        assert_eq!(result.receipt.status, "succeeded");
    }

    // AC-03: the child identity survives an unreadable directory.
    #[test]
    fn fork_result_keeps_the_confirmed_child_when_the_directory_is_unavailable() {
        let mut receipt = lifecycle("child-1", "READY");
        receipt.operation = "fork".into();
        receipt.parent_session_id = Some("parent-1".into());
        let outcome = ForkOutcome {
            handle: handle(Some(receipt)),
            working_directory: None,
            working_directory_error: Some("not_found".into()),
        };
        let view = ForkResultView::from_outcome(outcome);
        assert_eq!(view.session_id.as_deref(), Some("child-1"));
        assert_eq!(view.parent_session_id.as_deref(), Some("parent-1"));
        assert_eq!(view.disposition, "READY");
        assert_eq!(view.working_directory, None);
        assert_eq!(view.working_directory_error.as_deref(), Some("not_found"));
        assert_eq!(view.warning_code, None);
    }

    #[test]
    fn fork_result_reports_shared_directory_and_uncertain_reconcile_fields() {
        let mut receipt = lifecycle("child-2", "UNKNOWN");
        receipt.operation = "fork".into();
        receipt.reconcile_mode = Some("PARENT_SESSION_ID".into());
        receipt.reconcile_key = Some("parent-1".into());
        let outcome = ForkOutcome {
            handle: handle(Some(receipt)),
            working_directory: Some("/work/child".into()),
            working_directory_error: None,
        };
        let view = ForkResultView::from_outcome(outcome);
        assert_eq!(view.working_directory.as_deref(), Some("/work/child"));
        assert_eq!(view.working_directory_error, None);
        assert_eq!(view.disposition, "UNKNOWN");
        assert_eq!(view.reconcile_mode.as_deref(), Some("PARENT_SESSION_ID"));
        assert_eq!(view.reconcile_key.as_deref(), Some("parent-1"));
    }

    #[test]
    fn catalog_preserves_partial_providers_and_failures() {
        let catalog = ModelCatalog::from(whalerein_proto::ModelCatalog {
            default_selection: Some(whalerein_proto::ModelChoice {
                provider: "anthropic".into(),
                model: "sonnet".into(),
                effort: Some("high".into()),
            }),
            groups: vec![whalerein_proto::ModelProvider {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                models: vec![whalerein_proto::ModelEntry {
                    id: "sonnet".into(),
                    name: "Sonnet".into(),
                    efforts: vec![whalerein_proto::ModelEffort {
                        id: "high".into(),
                        name: "High".into(),
                    }],
                    default_effort: "high".into(),
                }],
            }],
            failures: vec![whalerein_proto::ModelProviderFailure {
                id: "openai".into(),
                name: "OpenAI".into(),
                code: whalerein_proto::ErrorCode::Unavailable as i32,
            }],
            effect_scope: "session_and_source_default_attempt".into(),
            complete: false,
        });
        let view = ModelCatalogView::from(catalog);
        assert_eq!(view.providers.len(), 1);
        assert_eq!(view.providers[0].models[0].efforts[0].id, "high");
        assert_eq!(view.failures[0].code, "unavailable");
        assert!(!view.complete);
        assert_eq!(
            view.default_selection.unwrap().effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn session_model_keeps_recorded_choice_and_fallback_separate() {
        fn choice(model: &str, effort: Option<&str>) -> whalerein_proto::ModelChoice {
            whalerein_proto::ModelChoice {
                provider: "p".into(),
                model: model.into(),
                effort: effort.map(str::to_owned),
            }
        }
        let model = SessionModel::from(whalerein_proto::SessionModel {
            pending: Some(choice("m", None)),
            last_used: Some(choice("last", None)),
            default_selection: Some(choice("default", None)),
            default_is_fallback: true,
            next: Some(choice("m", Some("low"))),
            pending_known: true,
        });
        let view = SessionModelView::from(model);
        assert_eq!(view.next.unwrap().model, "m");
        assert_eq!(view.last_used.unwrap().model, "last");
        assert_eq!(view.default_selection.unwrap().model, "default");
        assert!(view.default_is_fallback);
        assert!(view.pending_known);
        assert_eq!(view.pending.unwrap().model, "m");
    }

    #[test]
    fn model_selection_keeps_receipt_and_default_persistence_separate() {
        let mut command = handle(None);
        command.status = OnlineStatus::Succeeded;
        let receipt = ModelSelectionReceipt {
            selected: Some(ModelChoice {
                provider: "p".into(),
                model: "m".into(),
                effort: Some("high".into()),
            }),
            effect_scope: "session_and_source_default_attempt".into(),
            default_persistence: "unconfirmed".into(),
        };
        let view = ModelSelectionResultView::from_receipt(receipt, command);
        assert_eq!(view.receipt.command_id, "command-1");
        assert_eq!(view.receipt.status, "succeeded");
        assert_eq!(view.selected.unwrap().effort.as_deref(), Some("high"));
        assert_eq!(
            view.effect_scope.as_deref(),
            Some("session_and_source_default_attempt")
        );
        assert_eq!(view.default_persistence.as_deref(), Some("unconfirmed"));
    }

    #[test]
    fn no_op_selection_has_no_journaled_command_identity() {
        let mut command = handle(None);
        command.command_id = String::new();
        command.request_id = String::new();
        let receipt = ModelSelectionReceipt {
            selected: Some(ModelChoice {
                provider: "p".into(),
                model: "m".into(),
                effort: None,
            }),
            effect_scope: "session_and_source_default_attempt".into(),
            default_persistence: "unconfirmed".into(),
        };
        let view = ModelSelectionResultView::from_receipt(receipt, command);
        assert!(view.receipt.command_id.is_empty());
        assert_eq!(view.receipt.status, "succeeded");
        assert!(view.selected.is_some());
    }

    #[test]
    fn selection_without_a_source_receipt_does_not_fabricate_persistence() {
        let view = ModelSelectionResultView::without_receipt(handle(None));
        assert_eq!(view.selected, None);
        assert_eq!(view.effect_scope, None);
        assert_eq!(view.default_persistence, None);
    }

    #[test]
    fn validation_rejects_unusable_ids_and_oversized_values() {
        assert!(validate_id("").is_err());
        assert!(validate_id(&"x".repeat(MAX_ID_BYTES + 1)).is_err());
        assert!(validate_id("bad\nid").is_err());
        assert!(validate_optional(&Some("x".repeat(MAX_PATH_BYTES + 1)), MAX_PATH_BYTES).is_err());
        assert!(create_input(SessionCreateInputView {
            cwd: Some("/work".into()),
            workspace_id: None,
            agent_preset: None,
            preferred_session_id: Some("session-1".into()),
        })
        .is_ok());
    }

    #[test]
    fn detail_view_carries_host_and_session_identity() {
        let summary = SessionSummary {
            session_id: "session-1".into(),
            title: "Release".into(),
            cwd: "/work".into(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            running: true,
            agent_available: true,
            blank: false,
            parent_session_id: String::new(),
            origin: "native".into(),
            history_cursor: "cursor-1".into(),
            source_capabilities: None,
            queued_prompts: 0,
            archived: false,
        };
        let view = SessionDetailView::from_summary("host-1", summary);
        assert_eq!(view.host_id, "host-1");
        assert_eq!(view.session_id, "session-1");
        assert_eq!(view.title, "Release");
        assert_eq!(view.cwd, "/work");
        assert_eq!(view.created_at_unix_ms, 1);
        assert!(view.running);
        assert!(view.agent_available);
        assert_eq!(view.capabilities, None);
    }
}
