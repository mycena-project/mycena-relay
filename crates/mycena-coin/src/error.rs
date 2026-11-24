// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Errors for mesh coin payloads from peers — never panic on malformed gossip.

use crate::ledger::ApplyResult;
use crate::payload::CoinPayload;

#[derive(Debug)]
pub enum CoinError {
    InvalidPayload(String),
    LedgerRejected(ApplyResult),
    Internal(anyhow::Error),
}

pub type CoinResult<T> = Result<T, CoinError>;

impl std::fmt::Display for CoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPayload(e) => write!(f, "invalid coin payload from peer: {e}"),
            Self::LedgerRejected(r) => write!(f, "coin ledger rejected tx: {r:?}"),
            Self::Internal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CoinError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for CoinError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

impl From<bincode::Error> for CoinError {
    fn from(value: bincode::Error) -> Self {
        Self::InvalidPayload(value.to_string())
    }
}

impl From<sled::Error> for CoinError {
    fn from(value: sled::Error) -> Self {
        Self::Internal(value.into())
    }
}

/// Decode coin gossip bytes from a peer; returns `Err` on malformed input.
pub fn decode_peer_coin_payload(payload: &[u8]) -> CoinResult<CoinPayload> {
    bincode::deserialize(payload).map_err(CoinError::from)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn malformed_coin_payload_returns_err_not_panic() {
        assert!(decode_peer_coin_payload(b"not-bincode").is_err());
        assert!(decode_peer_coin_payload(&[]).is_err());
    }
}
