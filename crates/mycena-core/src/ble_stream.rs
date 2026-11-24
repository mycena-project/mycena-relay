// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Length-prefixed stream frames for libp2p direct protocol over BLE (ADR 002 F3).
//!
//! Public API preserved; implementation delegates to [`crate::bearer_transport`].

use crate::bearer_transport::{self, BearerPayloadKind, ChunkAssembler};

pub use bearer_transport::chunk_frame;

pub const STREAM_MAGIC: [u8; 4] = bearer_transport::LEGACY_STREAM_MAGIC;
pub const MAX_STREAM_PAYLOAD: usize = bearer_transport::MAX_FRAME_LEN;

/// Encode stream payload (legacy `MYLP` on wire by default).
pub fn encode_stream_frame(payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    encode_stream_frame_with_version(payload, bearer_transport::default_wire_version())
}

/// Encode stream payload using a negotiated bearer wire version.
pub fn encode_stream_frame_with_version(
    payload: &[u8],
    style: bearer_transport::BearerWireVersion,
) -> anyhow::Result<Vec<u8>> {
    bearer_transport::encode_bearer_frame(BearerPayloadKind::StreamDirect, payload, style)
}

pub fn decode_stream_frame(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let frame = bearer_transport::decode_bearer_frame(bytes)?;
    match frame.kind {
        BearerPayloadKind::StreamDirect | BearerPayloadKind::LegacyStream => Ok(frame.payload),
        BearerPayloadKind::SideChannelWire | BearerPayloadKind::LegacyWire => {
            anyhow::bail!("wire bearer frame — use ble_mesh::decode_frame")
        }
    }
}

pub fn chunk_stream_frame(frame: &[u8]) -> Vec<Vec<u8>> {
    chunk_frame(frame)
}

pub type StreamAssembler = ChunkAssembler;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn stream_roundtrip_and_chunk() {
        let payload = b"libp2p-cbor-direct-request".repeat(40);
        let frame = encode_stream_frame(&payload).unwrap();
        assert_eq!(decode_stream_frame(&frame).unwrap(), payload);
        let chunks = chunk_stream_frame(&frame);
        assert!(chunks.len() > 1);
        let mut asm = StreamAssembler::default();
        let mut done = None;
        for c in chunks {
            if let Some(f) = asm.push_chunk(&c).unwrap() {
                done = Some(f);
            }
        }
        assert_eq!(done.as_deref(), Some(frame.as_slice()));
    }
}
