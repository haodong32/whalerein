//! Bounded receiver for opaque logical MQTT envelopes. AEAD verification stays
//! in security and must succeed before a reconstructed message reaches business.
use super::AccessError;
use prost::Message;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use whalerein_proto::TransportFrame;

pub const VERSION: u32 = 2;
pub const CHUNK_BYTES: usize = 60 * 1024;
pub const PACKET_BYTES: usize = 64 * 1024;
pub const MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const PREFIX: &[u8] = b"WRMQTT2";
struct State {
    total: usize,
    count: usize,
    digest: Vec<u8>,
    parts: HashMap<u32, Vec<u8>>,
    until: Instant,
}
#[derive(Default)]
pub struct Receiver {
    incoming: HashMap<String, State>,
    completed: HashMap<String, State>,
}
pub struct Receipt {
    pub logical: Option<Vec<u8>>,
    pub ack: Vec<u8>,
}
fn invalid() -> AccessError {
    AccessError::protocol("invalid logical MQTT transport frame")
}

impl Receiver {
    pub fn clear(&mut self) {
        self.incoming.clear();
        self.completed.clear();
    }
    pub fn receive(&mut self, topic: &str, bytes: &[u8]) -> Result<Option<Receipt>, AccessError> {
        if !bytes.starts_with(PREFIX) {
            return Ok(None);
        }
        if bytes.len() > PACKET_BYTES {
            return Err(invalid());
        }
        let frame = TransportFrame::decode(&bytes[PREFIX.len()..]).map_err(|_| invalid())?;
        let total = frame.total_bytes as usize;
        let count = frame.count as usize;
        let index = frame.index as usize;
        if frame.version != VERSION
            || frame.ack
            || frame.topic != topic
            || frame.transfer_id.len() != 32
            || !frame
                .transfer_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || frame.digest.len() != 32
            || total == 0
            || total > MESSAGE_BYTES
            || count != total.div_ceil(CHUNK_BYTES)
            || index >= count
            || frame.payload.len() != CHUNK_BYTES.min(total - index * CHUNK_BYTES)
        {
            return Err(invalid());
        }
        let now = Instant::now();
        self.incoming.retain(|_, state| state.until > now);
        self.completed.retain(|_, state| state.until > now);
        let key = frame.transfer_id.clone();
        let mut logical = None;
        if !self.completed.contains_key(&key) {
            if !self.incoming.contains_key(&key) {
                let reserved: usize = self.incoming.values().map(|state| state.total).sum();
                if self.completed.len() >= 16384
                    || (total > CHUNK_BYTES
                        && (self.incoming.len() >= 4 || reserved + total > 32 * 1024 * 1024))
                {
                    return Err(AccessError::protocol(
                        "logical MQTT receive budget exhausted",
                    ));
                }
                self.incoming.insert(
                    key.clone(),
                    State {
                        total,
                        count,
                        digest: frame.digest.clone(),
                        parts: HashMap::new(),
                        until: now + Duration::from_secs(30),
                    },
                );
            }
            let state = self.incoming.get_mut(&key).expect("inserted transport");
            if state.total != total || state.count != count || state.digest != frame.digest {
                return Err(invalid());
            }
            if let Some(existing) = state.parts.get(&frame.index) {
                if existing != &frame.payload {
                    return Err(invalid());
                }
            }
            state.parts.insert(frame.index, frame.payload);
            if state.parts.len() == count {
                let mut body = Vec::with_capacity(total);
                for index in 0..count {
                    body.extend_from_slice(&state.parts[&(index as u32)]);
                }
                if crate::util::sha256(&body).as_slice() != state.digest {
                    self.incoming.remove(&key);
                    return Err(invalid());
                }
                let mut state = self.incoming.remove(&key).expect("complete transport");
                state.parts.clear();
                state.until = now + Duration::from_secs(30);
                self.completed.insert(key, state);
                logical = Some(body);
            }
        } else {
            let state = &self.completed[&key];
            if state.total != total || state.count != count || state.digest != frame.digest {
                return Err(invalid());
            }
        }
        let complete = logical.is_some() || self.completed.contains_key(&frame.transfer_id);
        let ack = TransportFrame {
            version: VERSION,
            transfer_id: frame.transfer_id,
            topic: frame.topic,
            index: frame.index,
            digest: frame.digest,
            ack: true,
            complete,
            ..Default::default()
        };
        let mut bytes = PREFIX.to_vec();
        ack.encode(&mut bytes).map_err(|_| invalid())?;
        Ok(Some(Receipt {
            logical,
            ack: bytes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frames(body: &[u8]) -> Vec<Vec<u8>> {
        body.chunks(CHUNK_BYTES)
            .enumerate()
            .map(|(index, part)| {
                let frame = TransportFrame {
                    version: VERSION,
                    transfer_id: "a".repeat(32),
                    topic: "event".into(),
                    index: index as u32,
                    count: body.len().div_ceil(CHUNK_BYTES) as u32,
                    total_bytes: body.len() as u32,
                    digest: crate::util::sha256(body).to_vec(),
                    payload: part.to_vec(),
                    ..Default::default()
                };
                let mut bytes = PREFIX.to_vec();
                frame.encode(&mut bytes).unwrap();
                bytes
            })
            .collect()
    }
    #[test]
    fn reordered_and_duplicate_parts_deliver_once() {
        let body = vec![0u8; 2 * 1024 * 1024];
        let frames = frames(&body);
        let mut receiver = Receiver::default();
        let mut delivered = 0;
        for bytes in frames.iter().rev().chain(frames.iter()) {
            assert!(bytes.len() <= PACKET_BYTES);
            let receipt = receiver.receive("event", bytes).unwrap().unwrap();
            let ack = TransportFrame::decode(&receipt.ack[PREFIX.len()..]).unwrap();
            assert!(ack.ack);
            if let Some(logical) = receipt.logical {
                assert_eq!(logical.len(), body.len());
                delivered += 1;
            }
        }
        assert_eq!(delivered, 1);
        receiver.clear();
        assert!(receiver.incoming.is_empty());
    }
    #[test]
    fn rejects_metadata_tampering_and_incomplete_messages() {
        let mut receiver = Receiver::default();
        assert!(receiver.receive("event", b"legacy").unwrap().is_none());
        assert!(receiver.receive("event", PREFIX).is_err());
        let body = vec![0; CHUNK_BYTES + 10];
        let frames = frames(&body);
        assert!(receiver.receive("other", &frames[0]).is_err());
        assert!(receiver
            .receive("event", &frames[0])
            .unwrap()
            .unwrap()
            .logical
            .is_none());
        let mut frame = TransportFrame::decode(&frames[1][PREFIX.len()..]).unwrap();
        frame.payload[0] = 1;
        let mut corrupt = PREFIX.to_vec();
        frame.encode(&mut corrupt).unwrap();
        assert!(receiver.receive("event", &corrupt).is_err());
        for index in 0..4 {
            let mut frame = TransportFrame::decode(&frames[0][PREFIX.len()..]).unwrap();
            frame.transfer_id = format!("{index:032x}");
            let mut bytes = PREFIX.to_vec();
            frame.encode(&mut bytes).unwrap();
            receiver.receive("event", &bytes).unwrap();
        }
        let mut frame = TransportFrame::decode(&frames[0][PREFIX.len()..]).unwrap();
        frame.transfer_id = "f".repeat(32);
        let mut bytes = PREFIX.to_vec();
        frame.encode(&mut bytes).unwrap();
        assert!(receiver.receive("event", &bytes).is_err());
    }
}
