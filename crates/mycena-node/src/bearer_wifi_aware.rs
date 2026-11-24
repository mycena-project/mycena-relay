// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Wi-Fi Aware discovery bearer — small presence announcements only (no bulk data).

use mycena_core::bearer_transport::{BearerMedium, BearerMtu, BearerTransport, ReliabilityClass};
use mycena_core::data::now_ms;
use mycena_core::transport_score::{TransportBearer, TransportScoreSample, TransportScoreStore};
use std::collections::VecDeque;

/// Max invite URI bytes published over Wi-Fi Aware / NSD discovery.
pub const DISCOVERY_ANNOUNCE_MAX: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryAnnouncement {
    pub invite_uri: String,
    pub published_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPeerEvent {
    pub peer_hint: String,
    pub invite_uri: String,
    pub at_ms: u64,
}

/// Discovery-only [`BearerTransport`] implementation for Android Wi-Fi Aware / NSD hosts.
#[derive(Debug, Default)]
pub struct BearerWifiAwareRouter {
    publish_queue: VecDeque<DiscoveryAnnouncement>,
    inbound: VecDeque<DiscoveryPeerEvent>,
    scores: TransportScoreStore,
    available: bool,
    discovery_events_wifi_aware: u64,
    discovery_events_ble: u64,
}

impl BearerWifiAwareRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_available(&mut self, available: bool) {
        self.available = available;
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn discovery_events_wifi_aware(&self) -> u64 {
        self.discovery_events_wifi_aware
    }

    pub fn discovery_events_ble(&self) -> u64 {
        self.discovery_events_ble
    }

    pub fn transport_scores(&self) -> &TransportScoreStore {
        &self.scores
    }

    pub fn transport_scores_mut(&mut self) -> &mut TransportScoreStore {
        &mut self.scores
    }

    pub fn queue_announcement(&mut self, invite_uri: String) {
        let trimmed = invite_uri.trim().to_string();
        if trimmed.is_empty() || trimmed.len() > DISCOVERY_ANNOUNCE_MAX {
            return;
        }
        self.publish_queue.push_back(DiscoveryAnnouncement {
            invite_uri: trimmed,
            published_at_ms: now_ms(),
        });
        while self.publish_queue.len() > 4 {
            self.publish_queue.pop_front();
        }
    }

    pub fn drain_announcements(&mut self, max: usize) -> Vec<DiscoveryAnnouncement> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.publish_queue.pop_front() {
                Some(a) => out.push(a),
                None => break,
            }
        }
        out
    }

    pub fn ingest_peer(&mut self, peer_hint: String, invite_uri: String, latency_ms: u64) {
        let invite = invite_uri.trim().to_string();
        if invite.is_empty() || invite.len() > DISCOVERY_ANNOUNCE_MAX {
            return;
        }
        let at = now_ms();
        let byte_len = invite.len().min(u32::MAX as usize) as u32;
        self.discovery_events_wifi_aware = self.discovery_events_wifi_aware.saturating_add(1);
        self.inbound.push_back(DiscoveryPeerEvent {
            peer_hint,
            invite_uri: invite.clone(),
            at_ms: at,
        });
        while self.inbound.len() > 32 {
            self.inbound.pop_front();
        }
        self.scores.record(
            TransportBearer::WifiAware,
            TransportScoreSample {
                latency_ms,
                bytes: byte_len,
                success: true,
                at_ms: at,
            },
        );
    }

    pub fn drain_inbound(&mut self, max: usize) -> Vec<DiscoveryPeerEvent> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.inbound.pop_front() {
                Some(e) => out.push(e),
                None => break,
            }
        }
        out
    }

    pub fn record_ble_discovery_observation(&mut self, latency_ms: u64, bytes: u32, success: bool) {
        if success {
            self.discovery_events_ble = self.discovery_events_ble.saturating_add(1);
        }
        self.scores.record(
            TransportBearer::Ble,
            TransportScoreSample {
                latency_ms,
                bytes,
                success,
                at_ms: now_ms(),
            },
        );
    }
}

impl BearerTransport for BearerWifiAwareRouter {
    fn mtu(&self) -> BearerMtu {
        BearerMtu {
            max_frame: DISCOVERY_ANNOUNCE_MAX,
            chunk_payload: DISCOVERY_ANNOUNCE_MAX,
        }
    }

    fn reliability_class(&self) -> ReliabilityClass {
        ReliabilityClass::BestEffort
    }

    fn carries_mesh_data(&self) -> bool {
        false
    }

    fn send_frame(&mut self, _peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        let uri = std::str::from_utf8(frame).unwrap_or("").trim();
        if uri.is_empty() {
            anyhow::bail!("empty discovery frame");
        }
        self.queue_announcement(uri.to_string());
        Ok(())
    }

    fn recv_frame(&mut self, _peer: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn drain_outbound(&mut self, _peer: &str, max: usize) -> Vec<Vec<u8>> {
        self.drain_announcements(max)
            .into_iter()
            .map(|a| a.invite_uri.into_bytes())
            .collect()
    }
}

impl BearerWifiAwareRouter {
    pub fn medium(&self) -> BearerMedium {
        BearerMedium::WifiAware
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::transport_score::prefer_wifi_aware_for_discovery;

    #[test]
    fn discovery_bearer_does_not_carry_mesh_data() {
        let router = BearerWifiAwareRouter::new();
        assert!(!router.carries_mesh_data());
        assert_eq!(router.medium(), BearerMedium::WifiAware);
    }

    #[test]
    fn ingest_updates_scores_for_wifi_aware_preference() {
        let mut router = BearerWifiAwareRouter::new();
        router.set_available(true);
        router.ingest_peer("peer-a".into(), "mycena://invite/v1#abc".into(), 20);
        assert!(prefer_wifi_aware_for_discovery(
            true,
            true,
            router.transport_scores(),
            now_ms()
        ));
    }

    #[test]
    fn ingest_increments_discovery_event_counter() {
        let mut router = BearerWifiAwareRouter::new();
        router.ingest_peer("peer-a".into(), "mycena://invite/v1#abc".into(), 20);
        assert_eq!(router.discovery_events_wifi_aware(), 1);
    }
}
