//! Ordered pending-queue projection and targeted mutation receipts.
//!
//! Core owns the queue contents and the actions the Source advertises for each
//! item. This module only normalizes labels for display and revalidates an
//! action against the fresh advertisement before a mutation is submitted.

use whalerein_core::api::{PendingQueue, PendingQueueAction, PendingQueueBlock, PendingQueueItem};

use crate::types::CommandView;

/// One ordered pending item as displayed by the client.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PendingQueueItemView {
    pub item_id: String,
    /// Stable lowercase placement (`queued`, `steering`, `context`) or an
    /// explicit unknown label.
    pub placement: String,
    pub delivery_id: Option<String>,
    pub blocks: Vec<PendingQueueBlockView>,
    /// Lowercase actions advertised by the Source (`edit`, `remove`, `promote`).
    pub actions: Vec<String>,
}

impl PendingQueueItemView {
    /// Whether the Source currently advertises this action for the item.
    pub fn advertises(&self, action: QueueAction) -> bool {
        let label = action.as_str();
        self.actions
            .iter()
            .any(|advertised| advertised.eq_ignore_ascii_case(label))
    }
}

/// Typed content of one pending item. Attachment presence is explicit so the
/// client can warn before a whole-content edit replaces it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum PendingQueueBlockView {
    Text {
        text: String,
    },
    Attachment {
        media_type: String,
        name: String,
        size: u64,
        available: bool,
    },
    Unavailable,
}

/// The authoritative ordered queue. `available == false` means the Source did
/// not confirm a readable queue; an empty `items` list is then not an
/// authoritative empty queue.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PendingQueueView {
    pub session_id: String,
    pub available: bool,
    pub running: bool,
    pub items: Vec<PendingQueueItemView>,
}

/// One of the three targeted actions the CLI exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum QueueAction {
    Edit,
    Remove,
    Promote,
}

impl QueueAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Remove => "remove",
            Self::Promote => "promote",
        }
    }

    pub(crate) fn to_core(self) -> PendingQueueAction {
        match self {
            Self::Edit => PendingQueueAction::Edit,
            Self::Remove => PendingQueueAction::Remove,
            Self::Promote => PendingQueueAction::Promote,
        }
    }
}

/// A submitted targeted mutation plus the native terminal receipt.
///
/// `operation_id` is the command identity Core allocated for this mutation. It
/// is the only identity a caller should use to reconcile a late result; it is
/// not a second operation journal.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QueueMutationView {
    pub operation_id: String,
    pub host_id: String,
    pub session_id: String,
    pub item_id: String,
    pub action: String,
    pub receipt: CommandView,
}

impl From<PendingQueue> for PendingQueueView {
    fn from(value: PendingQueue) -> Self {
        Self {
            session_id: value.session_id,
            available: value.available,
            running: value.running,
            items: value.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<PendingQueueItem> for PendingQueueItemView {
    fn from(value: PendingQueueItem) -> Self {
        Self {
            item_id: value.item_id,
            placement: normalize_label(&value.placement, "unknown_placement"),
            delivery_id: value.delivery_id,
            blocks: value
                .blocks
                .into_iter()
                .map(|block| match block {
                    PendingQueueBlock::Text { text } => PendingQueueBlockView::Text { text },
                    PendingQueueBlock::Attachment {
                        media_type,
                        name,
                        size,
                        available,
                    } => PendingQueueBlockView::Attachment {
                        media_type,
                        name,
                        size,
                        available,
                    },
                    PendingQueueBlock::Unavailable => PendingQueueBlockView::Unavailable,
                })
                .collect(),
            actions: value
                .actions
                .into_iter()
                .map(|action| normalize_label(&action, "unknown_action"))
                .collect(),
        }
    }
}

/// Lowercase stable label. Unknown enum labels stay explicit instead of being
/// dropped or mistaken for a supported value.
fn normalize_label(value: &str, unknown: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return unknown.to_owned();
    }
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["queue_placement_", "queue_action_", "placement_", "action_"] {
        if let Some(stripped) = lower.strip_prefix(prefix) {
            if !stripped.is_empty() {
                return stripped.to_owned();
            }
        }
    }
    lower
}

/// Validate a requested action against the fresh advertisement for one item.
///
/// The caller still submits the mutation when the item disappeared in the same
/// window: the Host's native receipt, not a client-side assumption, owns the
/// consumed-before-mutation outcome.
pub(crate) fn action_is_advertised(item: &PendingQueueItemView, action: QueueAction) -> bool {
    item.advertises(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(actions: &[&str]) -> PendingQueueItemView {
        PendingQueueItemView {
            item_id: "item-1".into(),
            placement: "QUEUED".into(),
            delivery_id: Some("delivery-1".into()),
            blocks: Vec::new(),
            actions: actions.iter().map(|action| (*action).to_owned()).collect(),
        }
    }

    #[test]
    fn maps_blocks_and_normalizes_actions_and_placement() {
        let view = PendingQueueView::from(PendingQueue {
            session_id: "session-1".into(),
            available: true,
            running: true,
            items: vec![PendingQueueItem {
                item_id: "item-1".into(),
                placement: "QUEUE_PLACEMENT_QUEUED".into(),
                delivery_id: None,
                blocks: vec![
                    PendingQueueBlock::Text {
                        text: "first".into(),
                    },
                    PendingQueueBlock::Attachment {
                        media_type: "image/png".into(),
                        name: "shot.png".into(),
                        size: 12,
                        available: true,
                    },
                    PendingQueueBlock::Unavailable,
                ],
                actions: vec!["QUEUE_ACTION_EDIT".into(), "remove".into()],
            }],
        });
        assert!(view.available);
        assert!(view.running);
        assert_eq!(view.items[0].placement, "queued");
        assert_eq!(view.items[0].actions, vec!["edit", "remove"]);
        assert_eq!(
            view.items[0].blocks,
            vec![
                PendingQueueBlockView::Text {
                    text: "first".into()
                },
                PendingQueueBlockView::Attachment {
                    media_type: "image/png".into(),
                    name: "shot.png".into(),
                    size: 12,
                    available: true,
                },
                PendingQueueBlockView::Unavailable,
            ]
        );
    }

    #[test]
    fn unavailable_and_unknown_labels_stay_explicit() {
        let view = PendingQueueView::from(PendingQueue {
            session_id: "session-1".into(),
            available: false,
            running: false,
            items: vec![PendingQueueItem {
                item_id: "item-1".into(),
                placement: String::new(),
                delivery_id: None,
                blocks: Vec::new(),
                actions: vec![String::new(), "MYSTERY".into()],
            }],
        });
        assert!(!view.available);
        assert!(view.items[0].blocks.is_empty());
        assert_eq!(view.items[0].placement, "unknown_placement");
        assert_eq!(view.items[0].actions, vec!["unknown_action", "mystery"]);
    }

    #[test]
    fn only_advertised_actions_validate() {
        let item = item(&["EDIT", "PUBLISH"]);
        assert!(action_is_advertised(&item, QueueAction::Edit));
        assert!(!action_is_advertised(&item, QueueAction::Remove));
        assert!(!action_is_advertised(&item, QueueAction::Promote));
        assert_eq!(QueueAction::Promote.as_str(), "promote");
    }
}
