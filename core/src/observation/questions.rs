//! Same-stamp question reconciliation.
//!
//! Question pages are staged privately and only one complete collection with a
//! single stamp replaces the accepted view. This reducer has no history
//! dependency and never acquires a native answer claim.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use whalerein_proto::QuestionDescriptor;

use crate::access::transport::MESSAGE_BYTES;
use crate::api::{CoreResult, QuestionBatchInfo, QuestionPageInfo, QuestionStamp, QuestionView};

/// One complete question collection may not exceed the existing logical
/// envelope budget. This reuses the protocol's only content ceiling instead of
/// introducing a second product-specific number.
pub(crate) const QUESTION_COLLECTION_BYTES: usize = MESSAGE_BYTES;

/// Boxed authoritative page read. The production implementation is
/// `Core::read_questions`; tests supply controlled pages at the same seam.
pub(crate) type PageFuture<'a> =
    Pin<Box<dyn Future<Output = CoreResult<QuestionPageInfo>> + Send + 'a>>;

pub(crate) trait QuestionPageSource: Send + Sync {
    fn page<'a>(&'a self, cursor: &'a str) -> PageFuture<'a>;
}

/// A descriptor projected into the three states the reducer understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DescriptorState {
    Available(QuestionStamp),
    Unavailable(String),
    Unsupported(String),
}

pub(crate) fn descriptor_state(descriptor: &QuestionDescriptor) -> DescriptorState {
    let code = if descriptor.unavailable_code.is_empty() {
        "questions_unavailable"
    } else {
        descriptor.unavailable_code.as_str()
    };
    match whalerein_proto::QuestionAvailability::try_from(descriptor.availability) {
        Ok(whalerein_proto::QuestionAvailability::Available) => match &descriptor.stamp {
            Some(stamp) => DescriptorState::Available(stamp.clone().into()),
            // Available metadata without a stamp is not an empty collection.
            None => DescriptorState::Unavailable("missing_question_stamp".into()),
        },
        Ok(whalerein_proto::QuestionAvailability::Unsupported) => {
            DescriptorState::Unsupported(code.into())
        }
        Ok(whalerein_proto::QuestionAvailability::Unavailable) => {
            DescriptorState::Unavailable(code.into())
        }
        // UNSPECIFIED and unknown values are wire errors, never empty views.
        _ => DescriptorState::Unavailable("invalid_question_availability".into()),
    }
}

/// What the driver should do after a descriptor or transport signal.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// Nothing changed; no content read is needed.
    Idle,
    /// Emit this view without reading pages.
    Emit(QuestionView),
    /// Read the authoritative first page against the current target.
    Reconcile,
}

#[derive(Debug)]
struct Stage {
    target: QuestionStamp,
    cursor: String,
    batches: Vec<QuestionBatchInfo>,
    encoded_bytes: usize,
    seen_cursors: HashSet<String>,
    seen_batches: HashSet<String>,
    first_page: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PageStep {
    /// Read the same collection at this continuation cursor.
    Read,
    /// The staged collection is complete and has replaced the accepted view.
    Installed(QuestionView),
    /// Discard the stage and start again from the first page.
    Restart(&'static str),
    /// The stage cannot become a complete collection.
    Failed(String),
}

struct Accepted {
    stamp: QuestionStamp,
    batches: Vec<QuestionBatchInfo>,
}

pub(crate) struct QuestionReconciler {
    accepted: Option<Accepted>,
    descriptor: Option<DescriptorState>,
    stage: Option<Stage>,
    /// Reconnect/queue loss requires an authoritative read even when the next
    /// descriptor carries the previously accepted stamp.
    force: bool,
}

impl QuestionReconciler {
    pub(crate) fn new() -> Self {
        Self {
            accepted: None,
            descriptor: None,
            stage: None,
            force: false,
        }
    }

    pub(crate) fn accepted_view(&self) -> Option<QuestionView> {
        self.accepted
            .as_ref()
            .map(|accepted| QuestionView::current(accepted.stamp.clone(), accepted.batches.clone()))
    }

    /// Apply an authoritative descriptor update.
    pub(crate) fn observe_descriptor(&mut self, descriptor: &QuestionDescriptor) -> Step {
        match descriptor_state(descriptor) {
            DescriptorState::Unsupported(code) => {
                self.stage = None;
                self.descriptor = None;
                self.accepted = None;
                // A later available descriptor must rebuild from authority.
                self.force = true;
                Step::Emit(QuestionView::unsupported(code))
            }
            DescriptorState::Unavailable(code) => {
                self.stage = None;
                self.descriptor = None;
                self.force = true;
                let (stamp, batches) = self.accepted_parts();
                Step::Emit(QuestionView::unavailable(stamp, batches, code))
            }
            DescriptorState::Available(stamp) => {
                self.descriptor = Some(DescriptorState::Available(stamp.clone()));
                if !self.force
                    && self
                        .accepted
                        .as_ref()
                        .is_some_and(|accepted| accepted.stamp == stamp)
                {
                    Step::Idle
                } else {
                    Step::Reconcile
                }
            }
        }
    }

    /// Mark the facet stale after observation loss and require a rebuild.
    pub(crate) fn transport_lost(&mut self, code: &str) -> QuestionView {
        self.stage = None;
        self.force = true;
        let (stamp, batches) = self.accepted_parts();
        if stamp.is_some() {
            QuestionView::stale(stamp, batches, code)
        } else {
            QuestionView::unavailable(None, Vec::new(), code)
        }
    }

    pub(crate) fn abort_stage(&mut self) {
        self.stage = None;
    }

    /// Start a fresh stage against the current target, unless it is already
    /// satisfied. `None` means no read is needed.
    pub(crate) fn begin(&mut self) -> Result<Option<String>, QuestionView> {
        let Some(DescriptorState::Available(target)) = self.descriptor.clone() else {
            return Err(match self.descriptor.clone() {
                Some(DescriptorState::Unsupported(code)) => QuestionView::unsupported(code),
                Some(DescriptorState::Unavailable(code)) => {
                    QuestionView::unavailable(None, Vec::new(), code)
                }
                None => QuestionView::unavailable(None, Vec::new(), "questions_loading"),
                Some(DescriptorState::Available(_)) => unreachable!(),
            });
        };
        if !self.force
            && self
                .accepted
                .as_ref()
                .is_some_and(|accepted| accepted.stamp == target)
        {
            return Ok(None);
        }
        self.stage = Some(Stage {
            target,
            cursor: String::new(),
            batches: Vec::new(),
            encoded_bytes: 0,
            seen_cursors: HashSet::new(),
            seen_batches: HashSet::new(),
            first_page: true,
        });
        Ok(Some(String::new()))
    }

    pub(crate) fn read_cursor(&self) -> Option<String> {
        self.stage.as_ref().map(|stage| stage.cursor.clone())
    }

    /// Validate one page against the staged target and the accepted view.
    pub(crate) fn accept_page(&mut self, page: QuestionPageInfo) -> PageStep {
        let Some(stage) = self.stage.as_mut() else {
            return PageStep::Failed("question stage missing".into());
        };
        let Some(stamp) = page.stamp else {
            return PageStep::Failed("missing_question_stamp".into());
        };
        // Charge the encoded page, including its cursor/stamp overhead, before
        // retaining any of its content. Repeated page overhead is counted
        // conservatively; a complete collection must fit one envelope budget.
        if stage.encoded_bytes.saturating_add(page.encoded_bytes) > QUESTION_COLLECTION_BYTES {
            return PageStep::Failed("question_collection_too_large".into());
        }
        stage.encoded_bytes += page.encoded_bytes;
        if stamp.observation_epoch != stage.target.observation_epoch {
            // An opaque epoch is never ordered; a different epoch needs a
            // current authoritative descriptor, not a lexical comparison.
            return PageStep::Restart("question epoch changed");
        }
        if stage.first_page {
            if stamp.revision < stage.target.revision {
                return PageStep::Restart("older first page");
            }
            // A read may sample inventory newer than the descriptor. Its stamp
            // becomes the target and every later page must match it exactly.
            stage.target = stamp;
        } else if stamp != stage.target {
            return PageStep::Restart("mixed question stamps");
        }
        for batch in page.batches {
            if !stage.seen_batches.insert(batch.batch_id.clone()) {
                return PageStep::Failed("duplicate_question_batch".into());
            }
            stage.batches.push(batch);
        }
        stage.first_page = false;
        if page.complete {
            if !page.next_cursor.is_empty() {
                return PageStep::Failed("complete_question_page_has_cursor".into());
            }
            let stage = self.stage.take().expect("stage present");
            let stamp = stage.target.clone();
            let view = QuestionView::current(stamp.clone(), stage.batches.clone());
            self.accepted = Some(Accepted {
                stamp,
                batches: stage.batches,
            });
            self.force = false;
            return PageStep::Installed(view);
        }
        if page.next_cursor.is_empty() {
            return PageStep::Failed("incomplete_question_page_without_cursor".into());
        }
        if page.next_cursor == stage.cursor || !stage.seen_cursors.insert(page.next_cursor.clone())
        {
            return PageStep::Failed("non_progressing_question_cursor".into());
        }
        stage.cursor = page.next_cursor;
        PageStep::Read
    }

    fn accepted_parts(&self) -> (Option<QuestionStamp>, Vec<QuestionBatchInfo>) {
        self.accepted
            .as_ref()
            .map(|accepted| (Some(accepted.stamp.clone()), accepted.batches.clone()))
            .unwrap_or((None, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::QuestionFreshness;

    fn stamp(epoch: &str, revision: u64) -> QuestionStamp {
        QuestionStamp {
            observation_epoch: epoch.into(),
            revision,
        }
    }

    fn descriptor(epoch: &str, revision: u64) -> QuestionDescriptor {
        QuestionDescriptor {
            availability: whalerein_proto::QuestionAvailability::Available as i32,
            unavailable_code: String::new(),
            stamp: Some(whalerein_proto::QuestionStamp {
                observation_epoch: epoch.into(),
                revision,
            }),
        }
    }

    fn batch(id: &str) -> QuestionBatchInfo {
        QuestionBatchInfo {
            session_id: "session-1".into(),
            batch_id: id.into(),
            source_epoch: "native".into(),
            version: 1,
            digest: vec![1; 32],
            phase: "QUESTION_OPEN".into(),
            questions: Vec::new(),
            answerable: true,
            content_unavailable: false,
        }
    }

    fn page(
        stamp: Option<QuestionStamp>,
        next_cursor: &str,
        complete: bool,
        batches: Vec<QuestionBatchInfo>,
    ) -> QuestionPageInfo {
        QuestionPageInfo {
            batches,
            next_cursor: next_cursor.into(),
            stamp,
            complete,
            encoded_bytes: 0,
        }
    }

    fn install(reconciler: &mut QuestionReconciler, page: QuestionPageInfo) -> QuestionView {
        reconciler.begin().unwrap().unwrap();
        match reconciler.accept_page(page) {
            PageStep::Installed(view) => view,
            other => panic!("expected install, got {other:?}"),
        }
    }

    #[test]
    fn session_observation_ac_06_stable_stamp_does_not_requery_content() {
        let mut reconciler = QuestionReconciler::new();
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 1)),
            Step::Reconcile
        );
        let view = install(
            &mut reconciler,
            page(Some(stamp("epoch-1", 1)), "", true, vec![batch("b1")]),
        );
        assert_eq!(view.freshness, QuestionFreshness::Current);
        // The unchanged descriptor never asks for another content read.
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 1)),
            Step::Idle
        );
        assert_eq!(reconciler.read_cursor(), None);
    }

    #[test]
    fn session_observation_ac_07_older_first_page_is_discarded() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        install(
            &mut reconciler,
            page(Some(stamp("epoch-1", 1)), "", true, vec![batch("b1")]),
        );
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 3)),
            Step::Reconcile
        );
        assert_eq!(reconciler.begin().unwrap(), Some(String::new()));
        assert_eq!(
            reconciler.accept_page(page(Some(stamp("epoch-1", 1)), "", true, vec![])),
            PageStep::Restart("older first page")
        );
        assert_eq!(
            reconciler.accepted_view().unwrap().stamp,
            Some(stamp("epoch-1", 1))
        );
        // The same stage then accepts the current revision.
        reconciler.begin().unwrap().unwrap();
        let view = match reconciler.accept_page(page(
            Some(stamp("epoch-1", 3)),
            "",
            true,
            vec![batch("b3")],
        )) {
            PageStep::Installed(view) => view,
            other => panic!("{other:?}"),
        };
        assert_eq!(view.stamp, Some(stamp("epoch-1", 3)));
        assert_eq!(view.batches[0].batch_id, "b3");
    }

    #[test]
    fn session_observation_ac_07_different_epoch_is_never_ordered() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-2", 1));
        reconciler.begin().unwrap().unwrap();
        assert_eq!(
            reconciler.accept_page(page(
                Some(stamp("epoch-1", 99)),
                "",
                true,
                vec![batch("old")],
            )),
            PageStep::Restart("question epoch changed")
        );
        assert_eq!(reconciler.accepted_view(), None);
    }

    #[test]
    fn session_observation_ac_07_mixed_pages_never_become_current() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        reconciler.begin().unwrap().unwrap();
        assert_eq!(
            reconciler.accept_page(page(
                Some(stamp("epoch-1", 1)),
                "next",
                false,
                vec![batch("b1")],
            )),
            PageStep::Read
        );
        assert_eq!(
            reconciler.accept_page(page(Some(stamp("epoch-1", 2)), "", true, vec![batch("b2")],)),
            PageStep::Restart("mixed question stamps")
        );
        assert_eq!(reconciler.accepted_view(), None);
    }

    #[test]
    fn session_observation_ac_07_duplicate_and_nonprogressing_cursors_fail() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        reconciler.begin().unwrap().unwrap();
        assert_eq!(
            reconciler.accept_page(page(Some(stamp("epoch-1", 1)), "same", false, vec![])),
            PageStep::Read
        );
        assert_eq!(
            reconciler.accept_page(page(Some(stamp("epoch-1", 1)), "same", false, vec![])),
            PageStep::Failed("non_progressing_question_cursor".into())
        );

        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        reconciler.begin().unwrap().unwrap();
        assert_eq!(
            reconciler.accept_page(page(Some(stamp("epoch-1", 1)), "", false, vec![])),
            PageStep::Failed("incomplete_question_page_without_cursor".into())
        );
    }

    #[test]
    fn session_observation_ac_06_content_change_requires_a_new_read() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        install(
            &mut reconciler,
            page(Some(stamp("epoch-1", 1)), "", true, vec![batch("b1")]),
        );
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 2)),
            Step::Reconcile
        );
    }

    #[test]
    fn session_observation_ac_04_change_during_read_restarts_with_new_target() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        reconciler.begin().unwrap().unwrap();
        // A newer descriptor arrives while the r1 page is in flight.
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 2)),
            Step::Reconcile
        );
        reconciler.abort_stage();
        reconciler.begin().unwrap().unwrap();
        // The late r1 page cannot overwrite the newer accepted target.
        assert_eq!(
            reconciler.accept_page(page(Some(stamp("epoch-1", 1)), "", true, vec![])),
            PageStep::Restart("older first page")
        );
        reconciler.begin().unwrap().unwrap();
        let view = match reconciler.accept_page(page(
            Some(stamp("epoch-1", 2)),
            "",
            true,
            vec![batch("b2")],
        )) {
            PageStep::Installed(view) => view,
            other => panic!("{other:?}"),
        };
        assert_eq!(view.stamp, Some(stamp("epoch-1", 2)));
    }

    #[test]
    fn session_observation_ac_10_transport_loss_marks_stale_and_forces_rebuild() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        install(
            &mut reconciler,
            page(Some(stamp("epoch-1", 1)), "", true, vec![batch("b1")]),
        );
        let stale = reconciler.transport_lost("connection_lost");
        assert_eq!(stale.freshness, QuestionFreshness::Stale);
        assert_eq!(stale.stamp, Some(stamp("epoch-1", 1)));
        assert_eq!(stale.batches.len(), 1);
        // The same stamp after reconnect still requires an authoritative read.
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 1)),
            Step::Reconcile
        );
    }

    #[test]
    fn session_observation_ac_09_refresh_detects_a_lost_final_hint() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        install(
            &mut reconciler,
            page(Some(stamp("epoch-1", 1)), "", true, vec![batch("b1")]),
        );
        // No question hint arrived; the refresh descriptor alone advances it.
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 2)),
            Step::Reconcile
        );
    }

    fn page_sized(
        encoded_bytes: usize,
        stamp: Option<QuestionStamp>,
        next_cursor: &str,
        complete: bool,
        batches: Vec<QuestionBatchInfo>,
    ) -> QuestionPageInfo {
        QuestionPageInfo {
            encoded_bytes,
            ..page(stamp, next_cursor, complete, batches)
        }
    }

    #[test]
    fn session_observation_ac_16_aggregate_below_and_at_the_envelope_budget_install() {
        let mut below = QuestionReconciler::new();
        below.observe_descriptor(&descriptor("epoch-1", 1));
        let view = install(
            &mut below,
            page_sized(
                QUESTION_COLLECTION_BYTES - 1,
                Some(stamp("epoch-1", 1)),
                "",
                true,
                vec![batch("b1")],
            ),
        );
        assert_eq!(view.freshness, QuestionFreshness::Current);

        let mut at = QuestionReconciler::new();
        at.observe_descriptor(&descriptor("epoch-1", 1));
        let view = install(
            &mut at,
            page_sized(
                QUESTION_COLLECTION_BYTES,
                Some(stamp("epoch-1", 1)),
                "",
                true,
                vec![batch("b1")],
            ),
        );
        assert_eq!(view.freshness, QuestionFreshness::Current);
    }

    #[test]
    fn session_observation_ac_16_aggregate_above_the_budget_fails_without_partial_current() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        reconciler.begin().unwrap().unwrap();
        assert_eq!(
            reconciler.accept_page(page_sized(
                QUESTION_COLLECTION_BYTES,
                Some(stamp("epoch-1", 1)),
                "next",
                false,
                vec![batch("b1")],
            )),
            PageStep::Read
        );
        // One more byte in the same stamped collection exceeds the budget.
        assert_eq!(
            reconciler.accept_page(page_sized(
                1,
                Some(stamp("epoch-1", 1)),
                "",
                true,
                vec![batch("b2")],
            )),
            PageStep::Failed("question_collection_too_large".into())
        );
        assert_eq!(reconciler.accepted_view(), None);
        reconciler.abort_stage();

        // A later smaller complete collection can still install.
        reconciler.begin().unwrap().unwrap();
        let view = match reconciler.accept_page(page_sized(
            128,
            Some(stamp("epoch-1", 1)),
            "",
            true,
            vec![batch("b3")],
        )) {
            PageStep::Installed(view) => view,
            other => panic!("{other:?}"),
        };
        assert_eq!(view.freshness, QuestionFreshness::Current);
        assert_eq!(view.batches[0].batch_id, "b3");
    }

    #[test]
    fn session_observation_ac_08_aggregate_overflow_keeps_the_previous_view_stale() {
        let mut reconciler = QuestionReconciler::new();
        reconciler.observe_descriptor(&descriptor("epoch-1", 1));
        install(
            &mut reconciler,
            page_sized(128, Some(stamp("epoch-1", 1)), "", true, vec![batch("b1")]),
        );
        reconciler.observe_descriptor(&descriptor("epoch-1", 2));
        reconciler.begin().unwrap().unwrap();
        assert_eq!(
            reconciler.accept_page(page_sized(
                QUESTION_COLLECTION_BYTES + 1,
                Some(stamp("epoch-1", 2)),
                "",
                true,
                vec![batch("b2")],
            )),
            PageStep::Failed("question_collection_too_large".into())
        );
        // The caller aborts the stage and derives Stale from the accepted view.
        reconciler.abort_stage();
        let stale = reconciler.transport_lost("question_collection_too_large");
        assert_eq!(stale.freshness, QuestionFreshness::Stale);
        assert_eq!(stale.stamp, Some(stamp("epoch-1", 1)));
        assert_eq!(stale.batches[0].batch_id, "b1");
        assert_eq!(
            stale.error.as_deref(),
            Some("question_collection_too_large")
        );
    }

    #[test]
    fn session_observation_ac_03_unsupported_and_unavailable_are_not_empty_current() {
        let mut reconciler = QuestionReconciler::new();
        let unsupported = QuestionDescriptor {
            availability: whalerein_proto::QuestionAvailability::Unsupported as i32,
            unavailable_code: "questions_unsupported".into(),
            stamp: None,
        };
        let Step::Emit(view) = reconciler.observe_descriptor(&unsupported) else {
            panic!("expected a direct view")
        };
        assert_eq!(view.freshness, QuestionFreshness::Unsupported);
        assert_ne!(view.freshness, QuestionFreshness::Current);

        let unspecified = QuestionDescriptor::default();
        let Step::Emit(view) = reconciler.observe_descriptor(&unspecified) else {
            panic!("expected a direct view")
        };
        assert_eq!(view.freshness, QuestionFreshness::Unavailable);
        assert_eq!(view.error.as_deref(), Some("invalid_question_availability"));
        // Recovery to the same stamp still requires an authoritative rebuild.
        assert_eq!(
            reconciler.observe_descriptor(&descriptor("epoch-1", 1)),
            Step::Reconcile
        );
    }
}
