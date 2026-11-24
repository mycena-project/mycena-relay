// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Unified BLE bearer router — one ingest/drain path for F2 + F3 (ADR 002).

#![allow(dead_code)] // Host-facing API; not all paths exercised in unit tests.

use crate::ble_transport::BleTransportRegistry;
use mycena_core::bearer_transport::{
    self, BearerPayloadKind, BearerTransport, BearerWireVersion, ReliabilityClass,
};
use mycena_core::transport::{TransportEvent, WireMessage};
use std::collections::{HashMap, VecDeque};

const SIDE_QUEUE_CAP: usize = 128;

/// Where an inbound assembled bearer frame should be routed.
#[derive(Debug)]
pub enum BearerIngestAction {
    Wire(WireMessage),
    StreamFrame(Vec<u8>),
}

/// Side-channel (F2) outbound queue + F3 stream registry behind one bearer API.
#[derive(Debug)]
pub struct BearerBleRouter {
    stream: BleTransportRegistry,
    side_outbound: HashMap<String, VecDeque<Vec<u8>>>,
    wire_version: BearerWireVersion,
}

impl Default for BearerBleRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl BearerBleRouter {
    pub fn new() -> Self {
        Self {
            stream: BleTransportRegistry::new(),
            side_outbound: HashMap::new(),
            wire_version: bearer_transport::default_wire_version(),
        }
    }

    pub fn stream_registry(&mut self) -> &mut BleTransportRegistry {
        &mut self.stream
    }

    pub fn stream_registry_ref(&self) -> &BleTransportRegistry {
        &self.stream
    }

    /// Classify and decode an assembled GATT bearer frame (MYBR / MYBL / MYLP).
    pub fn decode_ingest(bytes: &[u8]) -> anyhow::Result<BearerIngestAction> {
        let frame = bearer_transport::decode_bearer_frame(bytes)?;
        match frame.kind {
            BearerPayloadKind::SideChannelWire | BearerPayloadKind::LegacyWire => {
                let wire: WireMessage = bincode::deserialize(&frame.payload)?;
                Ok(BearerIngestAction::Wire(wire))
            }
            BearerPayloadKind::StreamDirect | BearerPayloadKind::LegacyStream => {
                Ok(BearerIngestAction::StreamFrame(frame.payload))
            }
        }
    }

    pub fn queue_side_channel_wire(
        &mut self,
        to_peer: String,
        wire: &WireMessage,
    ) -> anyhow::Result<()> {
        let payload = bincode::serialize(wire)?;
        let encoded = bearer_transport::encode_bearer_frame(
            BearerPayloadKind::SideChannelWire,
            &payload,
            self.wire_version,
        )?;
        BearerTransport::send_frame(self, &to_peer, &encoded)
    }

    pub fn drain_side_outbound(
        &mut self,
        to_peer: Option<&str>,
        max: usize,
    ) -> Vec<(String, Vec<u8>)> {
        let mut drained = Vec::new();
        if let Some(peer) = to_peer {
            let frames = BearerTransport::drain_outbound(self, peer, max);
            drained.extend(frames.into_iter().map(|f| (peer.to_string(), f)));
            return drained;
        }
        let peers: Vec<String> = self.side_outbound.keys().cloned().collect();
        for peer in peers {
            if drained.len() >= max {
                break;
            }
            let take = max.saturating_sub(drained.len());
            for frame in BearerTransport::drain_outbound(self, &peer, take) {
                drained.push((peer.clone(), frame));
            }
        }
        drained
    }

    pub fn register_link(&mut self, peer_id: String, device_id: &str) -> Option<TransportEvent> {
        self.stream.register_link(peer_id, device_id)
    }

    pub fn remove_link(&mut self, peer_id: &str) -> Option<TransportEvent> {
        self.stream.remove_link(peer_id)
    }

    pub fn is_connected(&self, peer_id: &str) -> bool {
        self.stream.is_connected(peer_id)
    }

    pub fn known_peers(&self) -> Vec<String> {
        self.stream.known_peers()
    }

    pub fn queue_dial(&mut self, multiaddr: &str) -> anyhow::Result<()> {
        self.stream.queue_dial(multiaddr)
    }

    pub fn send_direct(&mut self, to_peer: &str, message: WireMessage) -> anyhow::Result<()> {
        self.stream.send_direct(to_peer, message)
    }

    pub fn try_recv_event(&mut self) -> Option<TransportEvent> {
        self.stream.try_recv_event()
    }

    pub fn ingest_stream_frame(&mut self, from_peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        self.stream.ingest_stream_frame(from_peer, frame)
    }

    pub fn drain_stream_outbound(&mut self, to_peer: &str, max: usize) -> Vec<Vec<u8>> {
        self.stream.drain_outbound(to_peer, max)
    }

    /// Unified F2 + F3 outbound drain for BLE hosts (one relay loop).
    pub fn drain_bearer_outbound(
        &mut self,
        to_peer: Option<&str>,
        max: usize,
    ) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        if let Some(peer) = to_peer {
            self.drain_peer_bearer(peer, max, &mut out);
            return out;
        }
        let mut peer_ids = self.stream.known_peers();
        for peer in self.side_outbound.keys() {
            if !peer_ids.iter().any(|p| p == peer) {
                peer_ids.push(peer.clone());
            }
        }
        for peer in peer_ids {
            if out.len() >= max {
                break;
            }
            let take = max.saturating_sub(out.len());
            self.drain_peer_bearer(&peer, take, &mut out);
        }
        out
    }

    fn drain_peer_bearer(&mut self, peer: &str, max: usize, out: &mut Vec<(String, Vec<u8>)>) {
        if max == 0 {
            return;
        }
        for frame in self.drain_stream_outbound(peer, max.saturating_sub(out.len())) {
            if out.len() >= max {
                break;
            }
            out.push((peer.to_string(), frame));
        }
        let remaining = max.saturating_sub(out.len());
        for frame in BearerTransport::drain_outbound(self, peer, remaining) {
            if out.len() >= max {
                break;
            }
            out.push((peer.to_string(), frame));
        }
    }

    pub fn all_peer_infos(&self) -> Vec<crate::ble_transport::BlePeerInfo> {
        self.stream.all_peer_infos()
    }
}

impl BearerTransport for BearerBleRouter {
    fn mtu(&self) -> bearer_transport::BearerMtu {
        bearer_transport::BearerMtu::default()
    }

    fn reliability_class(&self) -> ReliabilityClass {
        ReliabilityClass::BestEffort
    }

    fn send_frame(&mut self, peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        let q = self.side_outbound.entry(peer.to_string()).or_default();
        q.push_back(frame.to_vec());
        while q.len() > SIDE_QUEUE_CAP {
            q.pop_front();
        }
        Ok(())
    }

    fn recv_frame(&mut self, _peer: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn drain_outbound(&mut self, peer: &str, max: usize) -> Vec<Vec<u8>> {
        let Some(q) = self.side_outbound.get_mut(peer) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while out.len() < max {
            match q.pop_front() {
                Some(f) => out.push(f),
                None => break,
            }
        }
        if q.is_empty() {
            self.side_outbound.remove(peer);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::data::Envelope;
    use mycena_core::transport::DirectMessage;

    #[test]
    fn unified_decode_routes_wire_and_stream() {
        let wire = WireMessage::Data(DirectMessage {
            envelope: Envelope::new("a".into(), None, b"x".to_vec()),
            body: "hi".into(),
        });
        let wire_bytes = mycena_core::ble_mesh::encode_wire(&wire).unwrap();
        match BearerBleRouter::decode_ingest(&wire_bytes).unwrap() {
            BearerIngestAction::Wire(w) => match w {
                WireMessage::Data(m) => assert_eq!(m.body, "hi"),
                _ => panic!("expected data wire"),
            },
            _ => panic!("expected wire"),
        }

        let stream_bytes = mycena_core::ble_stream::encode_stream_frame(b"cbor").unwrap();
        match BearerBleRouter::decode_ingest(&stream_bytes).unwrap() {
            BearerIngestAction::StreamFrame(p) => assert_eq!(p, b"cbor"),
            _ => panic!("expected stream"),
        }
    }

    #[test]
    fn unified_drain_merges_stream_and_side() {
        let mut router = BearerBleRouter::new();
        router.register_link("peerB".into(), "aa:bb:cc:dd:ee:ff");
        router
            .send_direct(
                "peerB",
                WireMessage::Data(DirectMessage {
                    envelope: Envelope::new("peerB".into(), None, vec![]),
                    body: "f3".into(),
                }),
            )
            .unwrap();
        let wire = WireMessage::Data(DirectMessage {
            envelope: Envelope::new("p".into(), None, vec![]),
            body: "f2".into(),
        });
        router
            .queue_side_channel_wire("peerB".into(), &wire)
            .unwrap();
        let drained = router.drain_bearer_outbound(Some("peerB"), 8);
        assert_eq!(drained.len(), 2);
    }

    #[test]
    fn side_queue_roundtrip() {
        let mut router = BearerBleRouter::new();
        let wire = WireMessage::Data(DirectMessage {
            envelope: Envelope::new("p".into(), None, vec![]),
            body: "m".into(),
        });
        router
            .queue_side_channel_wire("peerB".into(), &wire)
            .unwrap();
        let drained = router.drain_side_outbound(Some("peerB"), 4);
        assert_eq!(drained.len(), 1);
        match BearerBleRouter::decode_ingest(&drained[0].1).unwrap() {
            BearerIngestAction::Wire(w) => match w {
                WireMessage::Data(m) => assert_eq!(m.body, "m"),
                _ => panic!("expected data wire"),
            },
            _ => panic!("expected wire"),
        }
    }
}
