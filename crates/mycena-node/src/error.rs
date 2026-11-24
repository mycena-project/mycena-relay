// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Unified errors for peer-controlled input — must never panic on malformed network data.

use crate::ingest_guard::IngestReject;
use crate::security::Sd030DropReason;
use mycena_core::transport::WireMessage;

/// Errors from decoding or validating data that originated from the mesh / a remote peer.
#[derive(Debug)]
pub enum NodeError {
    InvalidBleFrame(String),
    InvalidBleStream(String),
    InvalidWireMessage(String),
    InvalidGossipPayload(String),
    InvalidCoinPayload(String),
    InvalidInvite(String),
    Crypto(String),
    IngestRejected(IngestReject),
    SignatureRejected(Sd030DropReason),
    Internal(anyhow::Error),
}

/// Outbound send failed because the destination is not reachable on a live bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendPathError {
    NotConnected { to_peer: String },
    NoDeliveryPath { to_peer: String },
}

impl std::fmt::Display for SendPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected { to_peer } => write!(f, "not connected to peer {to_peer}"),
            Self::NoDeliveryPath { to_peer } => write!(
                f,
                "peer_not_connected: no delivery path to {to_peer} (connect via Wi‑Fi, relay, or BLE first)"
            ),
        }
    }
}

impl std::error::Error for SendPathError {}

pub type NodeResult<T> = Result<T, NodeError>;

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBleFrame(e) => write!(f, "invalid BLE frame from peer: {e}"),
            Self::InvalidBleStream(e) => write!(f, "invalid BLE stream frame from peer: {e}"),
            Self::InvalidWireMessage(e) => write!(f, "invalid wire message from peer: {e}"),
            Self::InvalidGossipPayload(e) => write!(f, "invalid gossip payload from peer: {e}"),
            Self::InvalidCoinPayload(e) => write!(f, "invalid coin payload from peer: {e}"),
            Self::InvalidInvite(e) => write!(f, "invalid invite/QR input: {e}"),
            Self::Crypto(e) => write!(f, "crypto error on peer input: {e}"),
            Self::IngestRejected(r) => write!(f, "ingest rejected: {r:?}"),
            Self::SignatureRejected(r) => write!(f, "signature rejected: {r:?}"),
            Self::Internal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for NodeError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

impl From<bincode::Error> for NodeError {
    fn from(value: bincode::Error) -> Self {
        Self::InvalidWireMessage(value.to_string())
    }
}

impl From<IngestReject> for NodeError {
    fn from(value: IngestReject) -> Self {
        Self::IngestRejected(value)
    }
}

impl From<Sd030DropReason> for NodeError {
    fn from(value: Sd030DropReason) -> Self {
        Self::SignatureRejected(value)
    }
}

impl NodeError {
    pub fn invalid_ble_frame(err: impl std::fmt::Display) -> Self {
        Self::InvalidBleFrame(err.to_string())
    }

    pub fn invalid_gossip(err: impl std::fmt::Display) -> Self {
        Self::InvalidGossipPayload(err.to_string())
    }

    pub fn invalid_coin(err: impl std::fmt::Display) -> Self {
        Self::InvalidCoinPayload(err.to_string())
    }
}

/// Decode any bearer frame (MYBR v1, MYBL, MYLP) from a peer.
pub fn decode_peer_bearer_frame(
    frame: &[u8],
) -> NodeResult<mycena_core::bearer_transport::BearerFrame> {
    mycena_core::bearer_transport::decode_bearer_frame(frame).map_err(NodeError::invalid_ble_frame)
}

/// Decode an F2 MYBL wire frame from a peer; never panics on malformed bytes.
pub fn decode_peer_ble_wire_frame(frame: &[u8]) -> NodeResult<WireMessage> {
    mycena_core::ble_mesh::decode_frame(frame).map_err(NodeError::invalid_ble_frame)
}

/// Decode an F3 MYLP stream frame from a peer; never panics on malformed bytes.
pub fn decode_peer_ble_stream_frame(frame: &[u8]) -> NodeResult<Vec<u8>> {
    mycena_core::ble_stream::decode_stream_frame(frame)
        .map_err(|e| NodeError::InvalidBleStream(e.to_string()))
}

/// Deserialize gossip / direct wire payloads from a peer.
pub fn decode_peer_wire_message(payload: &[u8]) -> NodeResult<WireMessage> {
    bincode::deserialize(payload).map_err(NodeError::from)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::data::Envelope;
    use mycena_core::transport::DirectMessage;

    #[test]
    fn malformed_ble_frame_returns_err_not_panic() {
        assert!(decode_peer_ble_wire_frame(b"").is_err());
        assert!(decode_peer_ble_wire_frame(b"MYBL\x00\x00\x00\x01").is_err());
    }

    #[test]
    fn malformed_stream_frame_returns_err_not_panic() {
        assert!(decode_peer_ble_stream_frame(b"MYLP").is_err());
    }

    #[test]
    fn valid_ble_roundtrip_via_node_error_path() {
        let msg = DirectMessage {
            envelope: Envelope::new("from".into(), None, b"hi".to_vec()),
            body: "hello".into(),
        };
        let frame = mycena_core::ble_mesh::encode_wire(&WireMessage::Data(msg)).expect("encode");
        let back = decode_peer_ble_wire_frame(&frame).expect("decode");
        match back {
            WireMessage::Data(m) => assert_eq!(m.body, "hello"),
            _ => panic!("wrong variant in test"),
        }
    }
}
