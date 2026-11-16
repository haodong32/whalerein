//! Shared, transport-independent provisional conversation state.
//! Sync owns durable writes; consumers receive typed display-neutral updates.
use crate::api::{CoreError, CoreResult};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
pub use whalerein_proto::{
    content_block, live_frame, ApprovalState, AttemptOutcome, AttemptRef, ConversationMessage,
    LiveFrame, MessageRole, MessageUpsert, RunPhase, SessionKey,
};

const ATTEMPTS: usize = 8;
const PREVIEW_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: usize = 8 * PREVIEW_BYTES;
const SETTLED: usize = 4096;
/// Bound on the delivered committed-message identities kept for reconnect
/// dedupe. It covers a large replayed tail without growing without bound.
const DELIVERED: usize = 16_384;

#[derive(Debug, Clone, PartialEq)]
pub enum ViewUpdate {
    State(whalerein_proto::SessionState),
    Preview {
        attempt: AttemptRef,
        block_id: String,
        text: String,
        replace: bool,
        incomplete: bool,
    },
    Message {
        message: ConversationMessage,
        origin: Option<AttemptRef>,
        preview_prefix_bytes: Option<usize>,
    },
    AttemptEnded {
        attempt: AttemptRef,
        outcome: i32,
    },
    Reset,
}

#[derive(Default)]
struct Attempt {
    last: u64,
    /// Preview text per block, in first-seen order. A live frame addresses one
    /// block, so a block-level reset replaces only that block instead of
    /// invalidating the whole message preview.
    blocks: Vec<(String, String)>,
    intact: bool,
}

impl Attempt {
    fn block_mut(&mut self, block_id: &str) -> &mut String {
        if let Some(index) = self.blocks.iter().position(|(id, _)| id == block_id) {
            return &mut self.blocks[index].1;
        }
        self.blocks.push((block_id.to_owned(), String::new()));
        &mut self.blocks.last_mut().expect("just pushed").1
    }
    fn text_len(&self) -> usize {
        self.blocks.iter().map(|(_, text)| text.len()).sum()
    }
    /// Concatenate preview text in committed message order. `None` means a
    /// committed block has no preview (for example a tool call), so only the
    /// leading run can be skipped.
    fn leading_text(&self, message: &ConversationMessage) -> Option<String> {
        let mut text = String::new();
        for block in &message.blocks {
            let block_text = match &block.content {
                Some(content_block::Content::Text(content)) => &content.text,
                Some(content_block::Content::ToolResult(content)) => &content.text,
                Some(content_block::Content::Notice(content)) => &content.text,
                _ => continue,
            };
            let _ = block_text;
            let preview = self
                .blocks
                .iter()
                .find(|(id, _)| id == &block.block_id)
                .map(|(_, preview)| preview)?;
            text.push_str(preview);
        }
        Some(text)
    }
}

#[derive(Default)]
pub struct Conversation {
    epoch: String,
    active: HashMap<String, Attempt>,
    completed: VecDeque<(String, Instant)>,
    // Committed message identities already delivered to this viewer. This set
    // survives `reset` so a reconnecting watch or a gap recovery cannot print
    // the same committed message twice, while a cold attach baseline can mark
    // history as seen without emitting it.
    delivered: HashSet<String>,
    delivered_order: VecDeque<String>,
}

impl Conversation {
    /// Reset provisional attempt state for a new live epoch.
    ///
    /// Delivered committed identities are intentionally retained: a reconnect
    /// or a snapshot recovery replaces the transport, not what the viewer has
    /// already seen. Explicit `sync` uses a fresh Conversation and therefore
    /// still reports the whole committed history.
    pub fn reset(&mut self, epoch: &str) -> ViewUpdate {
        self.epoch = epoch.into();
        self.active.clear();
        self.completed.clear();
        ViewUpdate::Reset
    }

    /// Mark a committed message as already visible without emitting it.
    ///
    /// A watch attach replays history to rebuild the local cache; that baseline
    /// is not new output. Later gap recovery can then deliver only messages
    /// that were never observed.
    pub fn mark_delivered(&mut self, upsert: &MessageUpsert) {
        if let Some(message) = &upsert.message {
            self.mark_delivered_id(&message.message_id);
        }
    }

    /// Mark one committed identity as already visible to this viewer.
    pub fn mark_delivered_id(&mut self, message_id: &str) {
        if !message_id.is_empty() {
            self.note_delivered(message_id);
        }
    }

    fn note_delivered(&mut self, message_id: &str) -> bool {
        if !self.delivered.insert(message_id.to_owned()) {
            return false;
        }
        self.delivered_order.push_back(message_id.to_owned());
        while self.delivered_order.len() > DELIVERED {
            if let Some(evicted) = self.delivered_order.pop_front() {
                self.delivered.remove(&evicted);
            }
        }
        true
    }

    pub fn live(&mut self, frame: &LiveFrame) -> CoreResult<Option<ViewUpdate>> {
        let reference = frame
            .attempt
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing attempt identity".into()))?;
        if reference.live_epoch.is_empty()
            || reference.attempt_id.is_empty()
            || reference.attempt_id.len() > 512
            || frame.first_seq == 0
            || frame.last_seq < frame.first_seq
            || frame.payload.is_none()
        {
            return Err(CoreError::Security("invalid live frame".into()));
        }
        if matches!(
            frame.payload,
            Some(live_frame::Payload::Delta(_) | live_frame::Payload::Reset(_))
        ) && frame.block_id.is_empty()
        {
            return Err(CoreError::Security("missing text block identity".into()));
        }
        if reference.live_epoch != self.epoch {
            return Ok(None);
        }
        self.expire();
        if self
            .completed
            .iter()
            .any(|(id, _)| id == &reference.attempt_id)
        {
            return Ok(None);
        }
        if !self.active.contains_key(&reference.attempt_id) && self.active.len() >= ATTEMPTS {
            return Err(CoreError::Unavailable(
                "preview admission budget exceeded".into(),
            ));
        }
        let total: usize = self.active.values().map(Attempt::text_len).sum();
        let attempt = self.active.entry(reference.attempt_id.clone()).or_default();
        if frame.last_seq <= attempt.last {
            return Ok(None);
        }
        let contiguous = frame.first_seq == attempt.last + 1;
        let started = matches!(frame.payload, Some(live_frame::Payload::Started(_)));
        if attempt.last == 0 {
            attempt.intact = started && contiguous;
        } else {
            attempt.intact &= contiguous;
        }
        attempt.last = frame.last_seq;
        match frame.payload.as_ref().expect("validated payload") {
            live_frame::Payload::Started(_) => Ok(None),
            live_frame::Payload::Delta(text) | live_frame::Payload::Reset(text) => {
                let replace = matches!(frame.payload, Some(live_frame::Payload::Reset(_)));
                let current = attempt.block_mut(&frame.block_id);
                let replacement = if replace {
                    text.text.len()
                } else {
                    current.len() + text.text.len()
                };
                let total_after = total - current.len() + replacement;
                if replacement > PREVIEW_BYTES || total_after > TOTAL_BYTES {
                    // A truncated preview cannot be reconciled with its commit.
                    attempt.intact = false;
                } else if replace {
                    // The reset carries the full authoritative content of this
                    // block; it does not invalidate other blocks.
                    *current = text.text.clone();
                } else {
                    current.push_str(&text.text);
                }
                Ok(Some(ViewUpdate::Preview {
                    attempt: reference.clone(),
                    block_id: frame.block_id.clone(),
                    text: text.text.clone(),
                    replace,
                    incomplete: !attempt.intact,
                }))
            }
            live_frame::Payload::Ended(end) => {
                if end.outcome != whalerein_proto::AttemptOutcome::Committed as i32 {
                    self.settle(&reference.attempt_id);
                }
                Ok(Some(ViewUpdate::AttemptEnded {
                    attempt: reference.clone(),
                    outcome: end.outcome,
                }))
            }
        }
    }

    pub fn committed(&mut self, upsert: &MessageUpsert) -> CoreResult<ViewUpdate> {
        let message = upsert
            .message
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing committed message".into()))?;
        if message.message_id.is_empty()
            || message
                .blocks
                .iter()
                .any(|b| b.block_id.is_empty() || b.content.is_none())
        {
            return Err(CoreError::Security("invalid committed message".into()));
        }
        let mut prefix = None;
        if let Some(origin) = &upsert.origin {
            if origin.live_epoch == self.epoch {
                if let Some(attempt) = self.active.get(&origin.attempt_id) {
                    let text = message_text(message);
                    // Skip only the leading bytes that were already streamed
                    // for the same blocks. A block reset elsewhere or a tool
                    // block after the text does not force a full reprint.
                    if let Some(leading) = attempt.leading_text(message) {
                        if attempt.intact && text.starts_with(&leading) {
                            prefix = Some(leading.len());
                        }
                    }
                }
                self.settle(&origin.attempt_id);
            }
        }
        Ok(ViewUpdate::Message {
            message: message.clone(),
            origin: upsert.origin.clone(),
            preview_prefix_bytes: prefix,
        })
    }

    /// Format one committed message that this viewer has not seen before.
    ///
    /// Returns `None` for a duplicate identity (a replayed batch, a reconnect
    /// snapshot that overlaps live output, or a gap snapshot that repeats
    /// already printed history), so each committed message is emitted exactly
    /// once per watch.
    pub fn committed_once(&mut self, upsert: &MessageUpsert) -> CoreResult<Option<ViewUpdate>> {
        let message = upsert
            .message
            .as_ref()
            .ok_or_else(|| CoreError::Security("missing committed message".into()))?;
        let message_id = message.message_id.clone();
        let update = self.committed(upsert)?;
        if !self.note_delivered(&message_id) {
            return Ok(None);
        }
        Ok(Some(update))
    }

    fn settle(&mut self, id: &str) {
        self.active.remove(id);
        if !self.completed.iter().any(|(known, _)| known == id) {
            self.completed.push_back((id.into(), Instant::now()));
        }
        while self.completed.len() > SETTLED {
            self.completed.pop_front();
        }
    }
    fn expire(&mut self) {
        while self
            .completed
            .front()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(60))
        {
            self.completed.pop_front();
        }
    }
}

pub fn message_text(message: &ConversationMessage) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            Some(content_block::Content::Text(text)) => Some(text.text.as_str()),
            Some(content_block::Content::ToolResult(result)) => Some(result.text.as_str()),
            Some(content_block::Content::Notice(notice)) => Some(notice.text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use whalerein_proto::{
        content_block::Content, live_frame::Payload, AttemptEnded, AttemptOutcome, AttemptStarted,
        ContentBlock, TextContent,
    };
    fn frame(seq: u64, payload: Payload) -> LiveFrame {
        LiveFrame {
            attempt: Some(AttemptRef {
                live_epoch: "epoch".into(),
                attempt_id: "a".into(),
            }),
            block_id: "text".into(),
            first_seq: seq,
            last_seq: seq,
            payload: Some(payload),
        }
    }
    fn message() -> MessageUpsert {
        MessageUpsert {
            message: Some(ConversationMessage {
                message_id: "m".into(),
                blocks: vec![ContentBlock {
                    block_id: "text".into(),
                    content: Some(Content::Text(TextContent { text: "xy".into() })),
                }],
                ..Default::default()
            }),
            origin: frame(1, Payload::Started(AttemptStarted {})).attempt,
        }
    }
    #[test]
    fn streamed_text_and_final_commit_share_one_bounded_reducer() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        let delta = frame(2, Payload::Delta(TextContent { text: "x".into() }));
        assert!(matches!(
            state.live(&delta).unwrap(),
            Some(ViewUpdate::Preview {
                incomplete: false,
                ..
            })
        ));
        assert!(state.live(&delta).unwrap().is_none());
        let committed = message();
        assert!(matches!(
            state.committed(&committed).unwrap(),
            ViewUpdate::Message {
                message,
                preview_prefix_bytes: Some(1),
                ..
            } if Some(&message) == committed.message.as_ref()
        ));
        assert!(state
            .live(&frame(3, Payload::Delta(TextContent { text: "y".into() })))
            .unwrap()
            .is_none());
        assert!(state.active.is_empty());
    }
    #[test]
    fn rejected_delta_does_not_consume_the_sequence_of_a_corrected_frame() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        let valid = frame(2, Payload::Delta(TextContent { text: "x".into() }));
        let mut invalid = valid.clone();
        invalid.block_id.clear();
        assert!(state.live(&invalid).is_err());
        assert!(
            matches!(state.live(&valid).unwrap(), Some(ViewUpdate::Preview { text, incomplete: false, .. }) if text == "x")
        );
        assert!(matches!(
            state.committed(&message()).unwrap(),
            ViewUpdate::Message {
                preview_prefix_bytes: Some(1),
                ..
            }
        ));
    }

    #[test]
    fn gaps_resets_and_commit_before_start_do_not_create_duplicate_authoritative_text() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state.committed(&message()).unwrap();
        assert!(state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap()
            .is_none());
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        assert!(matches!(
            state
                .live(&frame(3, Payload::Reset(TextContent { text: "x".into() })))
                .unwrap(),
            Some(ViewUpdate::Preview {
                replace: true,
                incomplete: true,
                ..
            })
        ));
        assert!(matches!(
            state.committed(&message()).unwrap(),
            ViewUpdate::Message {
                preview_prefix_bytes: None,
                ..
            }
        ));
        state.reset("new");
        assert!(state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap()
            .is_none());
    }
    #[test]
    fn invalid_frames_and_message_content_fail_closed() {
        let mut state = Conversation::default();
        state.reset("epoch");
        assert!(state.live(&LiveFrame::default()).is_err());
        let mut invalid = frame(0, Payload::Started(AttemptStarted {}));
        assert!(state.live(&invalid).is_err());
        invalid.first_seq = 1;
        invalid.last_seq = 1;
        invalid.block_id.clear();
        invalid.payload = Some(Payload::Delta(TextContent { text: "x".into() }));
        assert!(state.live(&invalid).is_err());
        assert!(state.committed(&MessageUpsert::default()).is_err());
        let mut invalid = message();
        invalid.message.as_mut().unwrap().message_id.clear();
        assert!(state.committed(&invalid).is_err());
    }
    #[test]
    fn block_level_resets_keep_other_blocks_and_the_commit_prefix() {
        use whalerein_proto::{ContentBlock, MessageRole, ToolCallContent};
        fn block_frame(seq: u64, block: &str, payload: Payload) -> LiveFrame {
            let mut frame = frame(seq, payload);
            frame.block_id = block.into();
            frame
        }
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&block_frame(1, "a", Payload::Started(AttemptStarted {})))
            .unwrap();
        state
            .live(&block_frame(
                2,
                "a",
                Payload::Delta(TextContent {
                    text: "hello ".into(),
                }),
            ))
            .unwrap();
        assert!(matches!(
            state
                .live(&block_frame(
                    3,
                    "b",
                    Payload::Delta(TextContent {
                        text: "world".into(),
                    }),
                ))
                .unwrap(),
            Some(ViewUpdate::Preview {
                incomplete: false,
                ..
            })
        ));
        // A contiguous block reset replaces only block b; block a and the
        // attempt's integrity survive.
        let reset = state
            .live(&block_frame(
                4,
                "b",
                Payload::Reset(TextContent {
                    text: "there".into(),
                }),
            ))
            .unwrap()
            .unwrap();
        assert!(matches!(
            reset,
            ViewUpdate::Preview {
                replace: true,
                incomplete: false,
                ..
            }
        ));
        let committed = MessageUpsert {
            message: Some(ConversationMessage {
                message_id: "m".into(),
                role: MessageRole::Assistant as i32,
                blocks: vec![
                    ContentBlock {
                        block_id: "a".into(),
                        content: Some(Content::Text(TextContent {
                            text: "hello ".into(),
                        })),
                    },
                    ContentBlock {
                        block_id: "b".into(),
                        content: Some(Content::Text(TextContent {
                            text: "there".into(),
                        })),
                    },
                    ContentBlock {
                        block_id: "tool".into(),
                        content: Some(Content::ToolCall(ToolCallContent {
                            call_id: "c".into(),
                            name: "Read".into(),
                            summary: "f".into(),
                        })),
                    },
                ],
                ..Default::default()
            }),
            origin: frame(1, Payload::Started(AttemptStarted {})).attempt,
        };
        assert!(matches!(
            state.committed(&committed).unwrap(),
            ViewUpdate::Message {
                preview_prefix_bytes: Some(11),
                ..
            }
        ));
    }

    #[test]
    fn preview_memory_settlement_and_admission_have_explicit_limits() {
        let mut state = Conversation::default();
        state.reset("epoch");
        state
            .live(&frame(1, Payload::Started(AttemptStarted {})))
            .unwrap();
        state
            .live(&frame(
                2,
                Payload::Delta(TextContent {
                    text: "x".repeat(PREVIEW_BYTES + 1),
                }),
            ))
            .unwrap();
        assert_eq!(state.active["a"].text_len(), 0);
        assert!(!state.active["a"].intact);
        state
            .live(&frame(
                3,
                Payload::Ended(AttemptEnded {
                    outcome: AttemptOutcome::Abandoned as i32,
                }),
            ))
            .unwrap();
        assert!(state.active.is_empty());
        for index in 0..ATTEMPTS {
            let mut f = frame(1, Payload::Started(AttemptStarted {}));
            f.attempt.as_mut().unwrap().attempt_id = index.to_string();
            state.live(&f).unwrap();
        }
        let mut extra = frame(1, Payload::Started(AttemptStarted {}));
        extra.attempt.as_mut().unwrap().attempt_id = "extra".into();
        assert!(state.live(&extra).is_err());
        state.completed.clear();
        for index in 0..=SETTLED {
            state.settle(&index.to_string());
        }
        assert_eq!(state.completed.len(), SETTLED);
        state.completed.front_mut().unwrap().1 = Instant::now() - Duration::from_secs(61);
        state.expire();
        assert_eq!(state.completed.len(), SETTLED - 1);
    }
}
