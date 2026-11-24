// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! WebRTC SDP/ICE signaling carried over the existing libp2p relay path.
//!
//! After ICE completes, MYBR mesh frames flow on an RTCDataChannel ([`super::bearer_transport::BearerMedium::WebRtc`]).

/// Signaling message kind (wire `WireMessage::WebRtcSignal::kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum WebRtcSignalKind {
    Offer = 0,
    Answer = 1,
    IceCandidate = 2,
    /// Host reports data channel open (optional fast-path; link-ready FFI is authoritative).
    DataChannelOpen = 3,
}

impl WebRtcSignalKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Offer),
            1 => Some(Self::Answer),
            2 => Some(Self::IceCandidate),
            3 => Some(Self::DataChannelOpen),
            _ => None,
        }
    }
}

/// Default STUN servers for ICE (no TURN — relay path remains fallback).
pub fn default_stun_servers() -> Vec<String> {
    vec![
        "stun:stun.l.google.com:19302".to_string(),
        "stun:stun1.l.google.com:19302".to_string(),
    ]
}

/// RTCDataChannel label negotiated by Mycena hosts.
pub const DATA_CHANNEL_LABEL: &str = "mycena-mesh";

/// Max signaling payload (SDP or ICE JSON) over libp2p direct.
pub const MAX_SIGNALING_PAYLOAD: usize = 32_768;

/// Inbound WebRTC signaling delivered to the host (from libp2p relay).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebRtcSignalInbound {
    pub from_peer: String,
    pub session_id: String,
    pub kind: WebRtcSignalKind,
    pub payload: String,
    pub ice_mline_index: Option<u32>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn signal_kinds_roundtrip() {
        for k in [
            WebRtcSignalKind::Offer,
            WebRtcSignalKind::Answer,
            WebRtcSignalKind::IceCandidate,
            WebRtcSignalKind::DataChannelOpen,
        ] {
            assert_eq!(WebRtcSignalKind::from_u8(k as u8), Some(k));
        }
    }

    #[test]
    fn default_stun_non_empty() {
        assert!(!default_stun_servers().is_empty());
    }
}
