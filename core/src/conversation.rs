//! Shared, transport-independent provisional conversation state.
//! Sync owns durable writes; consumers receive typed display-neutral updates.
use crate::api::{CoreError, CoreResult};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
pub use whalerein_proto::{
    content_block, live_frame, ApprovalState, AttemptOutcome, AttemptRef, ConversationMessage,
    LiveFrame, MessageRole, MessageUpsert, RunPhase, SessionKey,
};

const ATTEMPTS: usize = 8;
const PREVIEW_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: usize = 8 * PREVIEW_BYTES;
const SETTLED: usize = 4096;

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
    text: String,
    intact: bool,
}

#[derive(Default)]
pub struct Conversation {
    epoch: String,
    active: HashMap<String, Attempt>,
    completed: VecDeque<(String, Instant)>,
}

impl Conversation {
    pub fn reset(&mut self, epoch: &str) -> ViewUpdate {
        self.epoch = epoch.into();
        self.active.clear();
        self.completed.clear();
        ViewUpdate::Reset
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
        let total: usize = self.active.values().map(|a| a.text.len()).sum();
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
                if replace {
                    attempt.intact = false;
                }
                if attempt.text.len() + text.text.len() > PREVIEW_BYTES
                    || total + text.text.len() > TOTAL_BYTES
                {
                    attempt.intact = false;
                } else {
                    attempt.text.push_str(&text.text);
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
                    if attempt.intact && text.starts_with(&attempt.text) {
                        prefix = Some(attempt.text.len());
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
        assert!(state.active["a"].text.is_empty());
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
