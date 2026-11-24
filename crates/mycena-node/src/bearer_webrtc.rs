// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! WebRTC RTCDataChannel bearer — MYBR frames after ICE via libp2p-relay signaling.

use mycena_core::bearer_transport::{
    webrtc_mtu, BearerMedium, BearerMtu, BearerTransport, ReliabilityClass,
};
use mycena_core::data::now_ms;
use mycena_core::transport_score::{TransportBearer, TransportScoreSample, TransportScoreStore};
use mycena_core::webrtc_signaling::{WebRtcSignalInbound, MAX_SIGNALING_PAYLOAD};
use std::collections::{HashMap, HashSet, VecDeque};

const OUTBOUND_QUEUE_CAP: usize = 64;
const INBOUND_SIGNAL_CAP: usize = 32;
const NEGOTIATION_CAP: usize = 16;

/// Per-peer RTCDataChannel MYBR queue + signaling state.
#[derive(Debug, Default)]
pub struct BearerWebRtcRouter {
    host_available: bool,
    links_ready: HashSet<String>,
    outbound: HashMap<String, VecDeque<Vec<u8>>>,
    inbound_signals: VecDeque<WebRtcSignalInbound>,
    negotiation_queue: VecDeque<String>,
    scores: TransportScoreStore,
}

impl BearerWebRtcRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_host_available(&mut self, available: bool) {
        self.host_available = available;
        if !available {
            self.links_ready.clear();
        }
    }

    pub fn host_available(&self) -> bool {
        self.host_available
    }

    pub fn set_link_ready(&mut self, peer: &str, ready: bool) {
        if ready {
            self.links_ready.insert(peer.to_string());
            self.negotiation_queue.retain(|p| p != peer);
        } else {
            self.links_ready.remove(peer);
            self.outbound.remove(peer);
        }
    }

    pub fn is_link_ready(&self, peer: &str) -> bool {
        self.links_ready.contains(peer)
    }

    pub fn active_link_count(&self) -> usize {
        self.links_ready.len()
    }

    pub fn transport_scores(&self) -> &TransportScoreStore {
        &self.scores
    }

    pub fn transport_scores_mut(&mut self) -> &mut TransportScoreStore {
        &mut self.scores
    }

    pub fn record_observation(&mut self, sample: TransportScoreSample) {
        self.scores.record(TransportBearer::WebRtc, sample);
    }

    /// Schedule SDP offer creation toward `peer` (deduped).
    pub fn schedule_negotiation(&mut self, peer: &str) {
        if !self.host_available || self.is_link_ready(peer) {
            return;
        }
        if self.negotiation_queue.iter().any(|p| p == peer) {
            return;
        }
        if self.negotiation_queue.len() >= NEGOTIATION_CAP {
            self.negotiation_queue.pop_front();
        }
        self.negotiation_queue.push_back(peer.to_string());
    }

    pub fn drain_negotiation_requests(&mut self, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.negotiation_queue.pop_front() {
                Some(p) => out.push(p),
                None => break,
            }
        }
        out
    }

    pub fn push_inbound_signal(&mut self, signal: WebRtcSignalInbound) {
        if signal.payload.len() > MAX_SIGNALING_PAYLOAD {
            return;
        }
        if self.inbound_signals.len() >= INBOUND_SIGNAL_CAP {
            self.inbound_signals.pop_front();
        }
        self.inbound_signals.push_back(signal);
    }

    pub fn drain_inbound_signals(&mut self, max: usize) -> Vec<WebRtcSignalInbound> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.inbound_signals.pop_front() {
                Some(s) => out.push(s),
                None => break,
            }
        }
        out
    }

    pub fn queue_frame(&mut self, peer: &str, frame: Vec<u8>) {
        let byte_len = frame.len();
        let q = self.outbound.entry(peer.to_string()).or_default();
        q.push_back(frame);
        while q.len() > OUTBOUND_QUEUE_CAP {
            q.pop_front();
        }
        let now = now_ms();
        let latency = self
            .scores
            .estimated_latency_ms(TransportBearer::WebRtc, byte_len);
        self.record_observation(TransportScoreSample {
            latency_ms: latency,
            bytes: byte_len.min(u32::MAX as usize) as u32,
            success: true,
            at_ms: now,
        });
    }

    pub fn drain_outbound(
        &mut self,
        to_peer: Option<&str>,
        max: usize,
    ) -> Vec<(String, Vec<u8>, BearerMedium)> {
        let mut out = Vec::new();
        if let Some(peer) = to_peer {
            drain_peer(&mut self.outbound, peer, max, &mut out);
            return out;
        }
        let peers: Vec<String> = self.outbound.keys().cloned().collect();
        for peer in peers {
            if out.len() >= max {
                break;
            }
            let take = max.saturating_sub(out.len());
            drain_peer(&mut self.outbound, &peer, take, &mut out);
        }
        out
    }
}

fn drain_peer(
    map: &mut HashMap<String, VecDeque<Vec<u8>>>,
    peer: &str,
    max: usize,
    out: &mut Vec<(String, Vec<u8>, BearerMedium)>,
) {
    let Some(q) = map.get_mut(peer) else {
        return;
    };
    while out.len() < max {
        match q.pop_front() {
            Some(frame) => out.push((peer.to_string(), frame, BearerMedium::WebRtc)),
            None => break,
        }
    }
    if q.is_empty() {
        map.remove(peer);
    }
}

impl BearerTransport for BearerWebRtcRouter {
    fn mtu(&self) -> BearerMtu {
        webrtc_mtu()
    }

    fn reliability_class(&self) -> ReliabilityClass {
        ReliabilityClass::Acknowledged
    }

    fn send_frame(&mut self, peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        self.queue_frame(peer, frame.to_vec());
        Ok(())
    }

    fn recv_frame(&mut self, _peer: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn drain_outbound(&mut self, peer: &str, max: usize) -> Vec<Vec<u8>> {
        self.drain_outbound(Some(peer), max)
            .into_iter()
            .map(|(_, f, _)| f)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::transport_score::decide_transport_path;
    use mycena_core::transport_score::TransportAvailability;

    #[test]
    fn link_ready_enables_webrtc_path() {
        let mut router = BearerWebRtcRouter::new();
        router.set_host_available(true);
        router.set_link_ready("peer-a", true);
        let availability = TransportAvailability {
            internet: true,
            webrtc: router.is_link_ready("peer-a"),
            ..Default::default()
        };
        assert_eq!(
            decide_transport_path(&availability, router.transport_scores(), 512, false, 0),
            mycena_core::transport_score::TransportPath::WebRtc
        );
    }

    #[test]
    fn negotiation_dedupes_peers() {
        let mut router = BearerWebRtcRouter::new();
        router.set_host_available(true);
        router.schedule_negotiation("p1");
        router.schedule_negotiation("p1");
        assert_eq!(router.drain_negotiation_requests(4), vec!["p1".to_string()]);
    }

    #[test]
    fn outbound_drains_with_medium_tag() {
        let mut router = BearerWebRtcRouter::new();
        router.queue_frame("p1", b"MYBR".to_vec());
        let drained = router.drain_outbound(None, 4);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].2, BearerMedium::WebRtc);
    }
}
