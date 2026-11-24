// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! BLE libp2p overlay — direct protocol over GATT streams (ADR 002 F3).
//! Frames use serde_cbor for compatibility with existing mobiles; do not swap
//! the codec without a wire version (crate is unmaintained).

#![allow(dead_code)] // F3 stream path wired on mobile hosts.

use crate::behaviour::{DirectMessageRequest, DirectMessageResponse};
use mycena_core::ble_multiaddr::{build_ble_multiaddr, parse_ble_multiaddr};
use mycena_core::ble_stream::{decode_stream_frame, encode_stream_frame};
use mycena_core::data::now_ms;
use mycena_core::transport::{MessageAck, TransportEvent, WireMessage};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;

const OUTBOUND_CAP: usize = 64;

#[derive(Debug, Clone)]
pub struct BlePeerInfo {
    pub multiaddr: String,
}

#[derive(Debug)]
enum BleInternalEvent {
    PeerUp {
        peer_id: String,
    },
    PeerDown {
        peer_id: String,
    },
    DirectReceived {
        from_peer: String,
        message: WireMessage,
    },
    DirectAck {
        from_peer: String,
        ack: MessageAck,
    },
}

/// Active BLE links carrying libp2p-style direct requests (F3).
#[derive(Debug)]
pub struct BleTransportRegistry {
    links: HashMap<String, BleLink>,
    event_tx: mpsc::UnboundedSender<BleInternalEvent>,
    event_rx: mpsc::UnboundedReceiver<BleInternalEvent>,
}

#[derive(Debug)]
struct BleLink {
    multiaddr: String,
    outbound: VecDeque<Vec<u8>>,
}

impl Default for BleTransportRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BleTransportRegistry {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            links: HashMap::new(),
            event_tx,
            event_rx,
        }
    }

    pub fn register_link(&mut self, peer_id: String, device_id: &str) -> Option<TransportEvent> {
        let multiaddr = build_ble_multiaddr(device_id, &peer_id);
        self.links.insert(
            peer_id.clone(),
            BleLink {
                multiaddr: multiaddr.clone(),
                outbound: VecDeque::new(),
            },
        );
        let _ = self.event_tx.send(BleInternalEvent::PeerUp {
            peer_id: peer_id.clone(),
        });
        Some(TransportEvent::PeerUp { peer_id })
    }

    pub fn remove_link(&mut self, peer_id: &str) -> Option<TransportEvent> {
        if self.links.remove(peer_id).is_some() {
            let _ = self.event_tx.send(BleInternalEvent::PeerDown {
                peer_id: peer_id.to_string(),
            });
            Some(TransportEvent::PeerDown {
                peer_id: peer_id.to_string(),
            })
        } else {
            None
        }
    }

    pub fn is_connected(&self, peer_id: &str) -> bool {
        self.links.contains_key(peer_id)
    }

    pub fn known_peers(&self) -> Vec<String> {
        self.links.keys().cloned().collect()
    }

    pub fn all_peer_infos(&self) -> Vec<BlePeerInfo> {
        self.links
            .values()
            .map(|l| BlePeerInfo {
                multiaddr: l.multiaddr.clone(),
            })
            .collect()
    }

    pub fn queue_dial(&mut self, multiaddr: &str) -> anyhow::Result<()> {
        let parsed = parse_ble_multiaddr(multiaddr)
            .ok_or_else(|| anyhow::anyhow!("not a BLE multiaddr: {multiaddr}"))?;
        if !self.links.contains_key(&parsed.peer_id) {
            tracing::info!(
                "BLE dial queued for {} @ {} (host must open GATT)",
                parsed.peer_id,
                parsed.device_id
            );
        }
        Ok(())
    }

    pub fn send_direct(&mut self, to_peer: &str, message: WireMessage) -> anyhow::Result<()> {
        let link = self
            .links
            .get_mut(to_peer)
            .ok_or_else(|| anyhow::anyhow!("no BLE link to {to_peer}"))?;
        let request = DirectMessageRequest { message };
        let cbor = serde_cbor::to_vec(&request)?;
        let frame = encode_stream_frame(&cbor)?;
        link.outbound.push_back(frame);
        while link.outbound.len() > OUTBOUND_CAP {
            link.outbound.pop_front();
        }
        Ok(())
    }

    pub fn drain_outbound(&mut self, to_peer: &str, max: usize) -> Vec<Vec<u8>> {
        let Some(link) = self.links.get_mut(to_peer) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while out.len() < max {
            match link.outbound.pop_front() {
                Some(f) => out.push(f),
                None => break,
            }
        }
        out
    }

    /// Host pushes reassembled MYLP frame from GATT.
    pub fn ingest_stream_frame(&mut self, from_peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        let payload = decode_stream_frame(frame)?;
        if let Ok(request) = serde_cbor::from_slice::<DirectMessageRequest>(&payload) {
            let _ = self.event_tx.send(BleInternalEvent::DirectReceived {
                from_peer: from_peer.to_string(),
                message: request.message,
            });
            let response = DirectMessageResponse {
                message_id: "ble".into(),
                accepted: true,
                received_at_ms: now_ms(),
            };
            self.queue_response(from_peer, &response)?;
            return Ok(());
        }
        if let Ok(response) = serde_cbor::from_slice::<DirectMessageResponse>(&payload) {
            let _ = self.event_tx.send(BleInternalEvent::DirectAck {
                from_peer: from_peer.to_string(),
                ack: MessageAck {
                    message_id: response.message_id,
                    accepted: response.accepted,
                },
            });
            return Ok(());
        }
        anyhow::bail!("unrecognized BLE stream CBOR payload")
    }

    fn queue_response(
        &mut self,
        to_peer: &str,
        response: &DirectMessageResponse,
    ) -> anyhow::Result<()> {
        let link = self
            .links
            .get_mut(to_peer)
            .ok_or_else(|| anyhow::anyhow!("no BLE link to {to_peer}"))?;
        let cbor = serde_cbor::to_vec(response)?;
        let frame = encode_stream_frame(&cbor)?;
        link.outbound.push_back(frame);
        Ok(())
    }

    pub fn try_recv_event(&mut self) -> Option<TransportEvent> {
        match self.event_rx.try_recv().ok()? {
            BleInternalEvent::PeerUp { peer_id, .. } => Some(TransportEvent::PeerUp { peer_id }),
            BleInternalEvent::PeerDown { peer_id } => Some(TransportEvent::PeerDown { peer_id }),
            BleInternalEvent::DirectReceived { from_peer, message } => {
                Some(TransportEvent::DirectReceived { from_peer, message })
            }
            BleInternalEvent::DirectAck { from_peer, ack } => {
                Some(TransportEvent::DirectAck { from_peer, ack })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::data::Envelope;
    use mycena_core::transport::DirectMessage;

    #[test]
    fn ble_direct_roundtrip() {
        let mut a = BleTransportRegistry::new();
        let mut b = BleTransportRegistry::new();
        a.register_link("peerB".into(), "aabbccddeeff");
        b.register_link("peerA".into(), "112233445566");

        let msg = WireMessage::Data(DirectMessage {
            envelope: Envelope::new("peerA".into(), Some("peerB".into()), b"hi".to_vec()),
            body: "hello over ble".into(),
        });
        a.send_direct("peerB", msg.clone()).unwrap();
        let frame = a.drain_outbound("peerB", 1).pop().expect("frame");
        let _ = b.try_recv_event(); // PeerUp from register_link
        b.ingest_stream_frame("peerA", &frame).unwrap();

        let ev = b.try_recv_event().expect("direct after ingest");
        match ev {
            TransportEvent::DirectReceived { from_peer, message } => {
                assert_eq!(from_peer, "peerA");
                match message {
                    WireMessage::Data(m) => assert_eq!(m.body, "hello over ble"),
                    _ => panic!("wrong variant"),
                }
            }
            _ => panic!("expected DirectReceived"),
        }
    }

    #[test]
    fn register_exposes_ble_multiaddr() {
        let mut reg = BleTransportRegistry::new();
        reg.register_link("12D3KooWTest".into(), "AA:BB:CC:DD:EE:FF");
        let info = reg
            .all_peer_infos()
            .into_iter()
            .find(|p| p.multiaddr.contains("12D3KooWTest"))
            .expect("info");
        assert!(info.multiaddr.starts_with("/ble/aabbccddeeff/gatt/"));
        assert!(info.multiaddr.ends_with("/p2p/12D3KooWTest"));
    }

    #[test]
    fn remove_link_clears_connected_state() {
        let mut reg = BleTransportRegistry::new();
        reg.register_link("peerA".into(), "aabbccddeeff");
        assert!(reg.is_connected("peerA"));
        assert_eq!(reg.known_peers(), vec!["peerA"]);
        reg.remove_link("peerA");
        assert!(!reg.is_connected("peerA"));
        assert!(reg.known_peers().is_empty());
    }
}
