// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Sliding-window tracker for mesh fact-claim corroboration (resource map + SOS).

use crate::information_trust_store::InformationTrustStore;
use crate::peer_trust::PeerTrust;
use crate::security::{corroboration_source_diversity, peer_routing_meta, PeerDiversityConfig};
use crate::sos_crisis_gate::{is_sos_source_trusted_for_crisis, SosSecurityConfig};
use mycena_core::information_trust::{
    compute_information_trust_with_diversity, count_conflicting_claims_in_cell, FactClaimKey,
    InformationTrust, InformationTrustConfig, SourceObservation,
};
use mycena_core::resource_map::{ResourceMapEntry, RESOURCE_DEFAULT_TTL_SECS};
use mycena_core::sos::{SosPayload, SOS_DEFAULT_TTL_SECS};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredObservation {
    key: FactClaimKey,
    observation: SourceObservation,
}

/// Records relay observations and computes per-claim [`InformationTrust`].
pub struct InformationTrustTracker {
    config: InformationTrustConfig,
    security_config: SosSecurityConfig,
    diversity_config: PeerDiversityConfig,
    store: Option<InformationTrustStore>,
    observations: VecDeque<StoredObservation>,
    by_claim: HashMap<FactClaimKey, Vec<SourceObservation>>,
}

impl Default for InformationTrustTracker {
    fn default() -> Self {
        Self::new_in_memory(
            InformationTrustConfig::default(),
            SosSecurityConfig::default(),
            PeerDiversityConfig::default(),
        )
    }
}

impl InformationTrustTracker {
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let store = InformationTrustStore::open(db_path)?;
        let mut tracker = Self::new_in_memory(
            InformationTrustConfig::default(),
            SosSecurityConfig::default(),
            PeerDiversityConfig::default(),
        );
        tracker.store = Some(store);
        tracker.reload_from_store()?;
        Ok(tracker)
    }

    fn new_in_memory(
        config: InformationTrustConfig,
        security_config: SosSecurityConfig,
        diversity_config: PeerDiversityConfig,
    ) -> Self {
        Self {
            config,
            security_config,
            diversity_config,
            store: None,
            observations: VecDeque::new(),
            by_claim: HashMap::new(),
        }
    }

    fn reload_from_store(&mut self) -> anyhow::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        for (key, observation) in store.load_all()? {
            self.observations.push_back(StoredObservation {
                key: key.clone(),
                observation: observation.clone(),
            });
            self.by_claim.entry(key).or_default().push(observation);
        }
        Ok(())
    }

    pub fn config(&self) -> &InformationTrustConfig {
        &self.config
    }

    pub fn diversity_config(&self) -> &PeerDiversityConfig {
        &self.diversity_config
    }

    pub fn record_resource(
        &mut self,
        relay_peer: &str,
        entry: &ResourceMapEntry,
        trust: &PeerTrust,
        now_ms: u64,
    ) {
        let key = FactClaimKey::resource(&entry.geohash, entry.category, &entry.description);
        self.record(relay_peer, key, trust, now_ms);
    }

    pub fn record_sos(
        &mut self,
        relay_peer: &str,
        payload: &SosPayload,
        trust: &PeerTrust,
        now_ms: u64,
    ) {
        let key = FactClaimKey::sos(&payload.geohash, &payload.message);
        self.record(relay_peer, key, trust, now_ms);
    }

    fn record(&mut self, relay_peer: &str, key: FactClaimKey, trust: &PeerTrust, now_ms: u64) {
        self.prune(now_ms);
        if self.observation_exists(&key, relay_peer, now_ms) {
            return;
        }
        let meta = peer_routing_meta(trust, None, trust.transport_hint.as_deref());
        let trusted = is_sos_source_trusted_for_crisis(trust, now_ms, &self.security_config);
        let observation = SourceObservation {
            relay_peer: relay_peer.to_string(),
            observed_at_ms: now_ms,
            trusted_independent: trusted,
            transport_hint: meta.transport_hint,
            first_encounter_ms: meta.first_encounter_ms,
            // Peer relay location only — claim geohash is already on FactClaimKey.
            geohash_bucket: meta.geohash_bucket,
        };
        if let Some(store) = &self.store {
            let _ = store.append(&key, &observation);
        }
        self.observations.push_back(StoredObservation {
            key: key.clone(),
            observation: observation.clone(),
        });
        self.by_claim.entry(key).or_default().push(observation);
    }

    fn observation_exists(&self, key: &FactClaimKey, relay_peer: &str, now_ms: u64) -> bool {
        let window = self.config.observation_window_ms;
        self.by_claim.get(key).is_some_and(|list| {
            list.iter().any(|o| {
                o.relay_peer == relay_peer && now_ms.saturating_sub(o.observed_at_ms) <= window
            })
        })
    }

    pub fn query_resource_trust(
        &self,
        entry: &ResourceMapEntry,
        now_ms: u64,
        peer_trust: &HashMap<String, PeerTrust>,
    ) -> InformationTrust {
        let key = FactClaimKey::resource(&entry.geohash, entry.category, &entry.description);
        self.query(&key, entry.ttl_secs, now_ms, peer_trust)
    }

    pub fn query_sos_trust(
        &self,
        payload: &SosPayload,
        now_ms: u64,
        peer_trust: &HashMap<String, PeerTrust>,
    ) -> InformationTrust {
        let key = FactClaimKey::sos(&payload.geohash, &payload.message);
        self.query(&key, SOS_DEFAULT_TTL_SECS, now_ms, peer_trust)
    }

    fn query(
        &self,
        key: &FactClaimKey,
        ttl_secs: u64,
        now_ms: u64,
        peer_trust: &HashMap<String, PeerTrust>,
    ) -> InformationTrust {
        let ttl = if ttl_secs == 0 {
            match key.kind {
                mycena_core::information_trust::FactKind::Resource => RESOURCE_DEFAULT_TTL_SECS,
                mycena_core::information_trust::FactKind::Sos => SOS_DEFAULT_TTL_SECS,
            }
        } else {
            ttl_secs
        };
        let claim_obs = self.by_claim.get(key).map(|v| v.as_slice()).unwrap_or(&[]);
        let conflicts = count_conflicting_claims_in_cell(
            &self.by_claim,
            key,
            self.config.geohash_cell_prefix_len,
            now_ms,
            self.config.observation_window_ms,
        );
        let window = self.config.observation_window_ms;
        let mut trusted_peers = HashSet::new();
        let mut trusted_obs = Vec::new();
        for obs in claim_obs {
            if now_ms.saturating_sub(obs.observed_at_ms) > window {
                continue;
            }
            if obs.trusted_independent && trusted_peers.insert(obs.relay_peer.as_str()) {
                trusted_obs.push(obs.clone());
            }
        }
        let diversity = corroboration_source_diversity(
            &trusted_obs,
            peer_trust,
            now_ms,
            &self.diversity_config,
        );
        compute_information_trust_with_diversity(
            claim_obs,
            conflicts,
            now_ms,
            ttl,
            &self.config,
            Some(diversity),
        )
    }

    fn prune(&mut self, now_ms: u64) {
        let window = self.config.observation_window_ms;
        let cutoff = now_ms.saturating_sub(window);
        if let Some(store) = &self.store {
            let _ = store.prune_before(cutoff);
        }
        while self
            .observations
            .front()
            .is_some_and(|stored| now_ms.saturating_sub(stored.observation.observed_at_ms) > window)
        {
            if let Some(stored) = self.observations.pop_front() {
                if let Some(list) = self.by_claim.get_mut(&stored.key) {
                    list.retain(|o| {
                        o.relay_peer != stored.observation.relay_peer
                            || o.observed_at_ms != stored.observation.observed_at_ms
                    });
                    if list.is_empty() {
                        self.by_claim.remove(&stored.key);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::resource_map::ResourceCategory;

    fn established_trust(now_ms: u64) -> PeerTrust {
        PeerTrust {
            first_seen_ms: now_ms.saturating_sub(30 * 86_400_000),
            reliability: 0.82,
            spam_score: 0.02,
            relay_success_rate: 0.80,
            relay_success_count: 80,
            relay_failure_count: 4,
            mutual_contacts: 2,
            messages_accepted: 200,
            messages_rejected: 1,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: Some("mesh-wifi".into()),
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    fn sybil_trust(now_ms: u64, idx: u64) -> PeerTrust {
        PeerTrust {
            first_seen_ms: now_ms.saturating_sub(40 * 60_000 + idx * 1000),
            reliability: 0.99,
            spam_score: 0.01,
            relay_success_rate: 0.99,
            relay_success_count: 400,
            relay_failure_count: 0,
            mutual_contacts: 0,
            messages_accepted: 1000,
            messages_rejected: 0,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: Some("sybil-local".into()),
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    fn peer_map<'a>(
        pairs: impl IntoIterator<Item = (&'a str, PeerTrust)>,
    ) -> HashMap<String, PeerTrust> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn resource_confirmed_after_three_legit_sources() {
        let now = 1_700_000_000_000u64;
        let mut tracker = InformationTrustTracker::default();
        let entry = ResourceMapEntry {
            geohash: "u4pruyd".into(),
            category: ResourceCategory::Water,
            description: "Fountain at plaza".into(),
            created_at_ms: now,
            ttl_secs: 3600,
        };
        let legit = established_trust(now);
        let mut peers = HashMap::new();
        for peer in ["L1", "L2", "L3"] {
            let mut t = legit.clone();
            t.transport_hint = Some(
                match peer {
                    "L1" => "wifi",
                    "L2" => "ble",
                    _ => "relay",
                }
                .into(),
            );
            peers.insert(peer.to_string(), t.clone());
            tracker.record_resource(peer, &entry, &t, now);
        }
        let trust = tracker.query_resource_trust(&entry, now, &peers);
        assert_eq!(
            trust.status,
            mycena_core::information_trust::CorroborationStatus::Confirmed
        );
    }

    #[test]
    fn conflicting_resource_reports_both_low_trust() {
        let now = 1_700_000_100_000u64;
        let mut tracker = InformationTrustTracker::default();
        let entry_a = ResourceMapEntry {
            geohash: "u4pruyd".into(),
            category: ResourceCategory::Water,
            description: "Water at north gate".into(),
            created_at_ms: now,
            ttl_secs: 3600,
        };
        let entry_b = ResourceMapEntry {
            geohash: "u4pruyd".into(),
            category: ResourceCategory::Water,
            description: "Water at south plaza".into(),
            created_at_ms: now,
            ttl_secs: 3600,
        };
        let mut peers = HashMap::new();
        for i in 0..20 {
            let peer = format!("S{i:03}");
            let t = sybil_trust(now, i as u64);
            peers.insert(peer.clone(), t.clone());
            tracker.record_resource(&peer, if i % 2 == 0 { &entry_a } else { &entry_b }, &t, now);
        }
        let trust_a = tracker.query_resource_trust(&entry_a, now, &peers);
        let trust_b = tracker.query_resource_trust(&entry_b, now, &peers);
        assert_eq!(
            trust_a.status,
            mycena_core::information_trust::CorroborationStatus::Conflicting
        );
        assert_eq!(trust_a.status, trust_b.status);
        assert!(trust_a.trust_score < 0.4);
        assert!(trust_b.trust_score < 0.4);
    }

    #[test]
    fn tracker_reopens_from_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("path");
        let now = 1_700_000_200_000u64;
        let entry = ResourceMapEntry {
            geohash: "u4pruyd".into(),
            category: ResourceCategory::Water,
            description: "Well".into(),
            created_at_ms: now,
            ttl_secs: 3600,
        };
        let trust = established_trust(now);
        {
            let mut tracker = InformationTrustTracker::open(path).expect("open");
            tracker.record_resource("L1", &entry, &trust, now);
        }
        let tracker = InformationTrustTracker::open(path).expect("reopen");
        let peers = peer_map([("L1", trust)]);
        let scored = tracker.query_resource_trust(&entry, now, &peers);
        assert_eq!(scored.corroboration_count, 1);
    }
}
