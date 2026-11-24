// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! BLE side-channel framing for wire messages (ADR 002 F2).
//!
//! Public API preserved; implementation delegates to [`crate::bearer_transport`].

use crate::bearer_transport::{self, BearerPayloadKind};
use crate::contact_hello::ContactHello;
use crate::transport::WireMessage;

pub use bearer_transport::chunk_frame;
pub use bearer_transport::ChunkAssembler;

pub const FRAME_MAGIC: [u8; 4] = bearer_transport::LEGACY_WIRE_MAGIC;
pub const CHUNK_MAGIC: [u8; 4] = bearer_transport::CHUNK_MAGIC;
pub const MAX_FRAME_LEN: usize = bearer_transport::MAX_FRAME_LEN;
pub const GATT_CHUNK_PAYLOAD: usize = bearer_transport::GATT_CHUNK_PAYLOAD;

/// Encode a [`WireMessage`] for BLE transfer (legacy `MYBL` on wire by default).
pub fn encode_wire(wire: &WireMessage) -> anyhow::Result<Vec<u8>> {
    let payload = bincode::serialize(wire)?;
    bearer_transport::encode_bearer_frame(
        BearerPayloadKind::SideChannelWire,
        &payload,
        bearer_transport::default_wire_version(),
    )
}

/// Sender libp2p peer id from a reassembled frame (contact hello or envelope).
pub fn sender_peer_from_frame(bytes: &[u8]) -> Option<String> {
    let wire = decode_frame(bytes).ok()?;
    match wire {
        WireMessage::Data(dm) => {
            if dm.body == "[contact:hello]" {
                ContactHello::decode(&dm.envelope.payload)
                    .ok()
                    .filter(|h| h.verify())
                    .map(|h| h.peer_id)
            } else if !dm.envelope.from_peer.is_empty() {
                Some(dm.envelope.from_peer)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Decode a full F2 wire frame (MYBR v1 or MYBL legacy).
pub fn decode_frame(bytes: &[u8]) -> anyhow::Result<WireMessage> {
    let frame = bearer_transport::decode_bearer_frame(bytes)?;
    match frame.kind {
        BearerPayloadKind::SideChannelWire | BearerPayloadKind::LegacyWire => {
            Ok(bincode::deserialize(&frame.payload)?)
        }
        BearerPayloadKind::StreamDirect | BearerPayloadKind::LegacyStream => {
            anyhow::bail!("stream bearer frame — use ble_stream::decode_stream_frame")
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::data::Envelope;
    use crate::transport::DirectMessage;

    #[test]
    fn roundtrip_wire_frame() {
        let msg = DirectMessage {
            envelope: Envelope::new("from".into(), Some("to".into()), b"hi".to_vec()),
            body: "hello".into(),
        };
        let wire = WireMessage::Data(msg);
        let frame = encode_wire(&wire).expect("encode");
        let back = decode_frame(&frame).expect("decode");
        match back {
            WireMessage::Data(m) => assert_eq!(m.body, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn chunk_reassembly() {
        let frame = encode_wire(&WireMessage::Data(DirectMessage {
            envelope: Envelope::new("a".into(), None, vec![0u8; 900]),
            body: "x".into(),
        }))
        .unwrap();
        let chunks = chunk_frame(&frame);
        assert!(chunks.len() > 1);
        let mut asm = ChunkAssembler::default();
        let mut done = None;
        for c in chunks {
            if let Some(f) = asm.push_chunk(&c).unwrap() {
                done = Some(f);
            }
        }
        assert_eq!(done.as_deref(), Some(frame.as_slice()));
    }
}
