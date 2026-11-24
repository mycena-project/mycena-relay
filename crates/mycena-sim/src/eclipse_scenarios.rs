// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Eclipse attack simulation — attacker floods visible peer set; diversity keeps legit paths.

use mycena_node::{
    apply_peer_diversity, select_relay_candidates_with_diversity, PeerDiversityConfig,
    PeerRoutingMeta, PeerTrust, RelaySelectionRequest,
};
use std::collections::{HashMap, HashSet};

const NOW_MS: u64 = 1_700_000_000_000;
const FANOUT: usize = 3;
const ATTACKER_PEERS: usize = 9;
const LEGIT_PEER: &str = "12D3KooWLegitEstablishedPeer";
const DEST: &str = "12D3KooWMessageDestination";
const GEO_ATTACKER_BUCKET: &str = "u4pr";
const GEO_LEGIT_BUCKET: &str = "u33d";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EclipseScenarioReport {
    pub visible_peers: usize,
    pub attacker_share_pct: u32,
    pub score_only_legit_relay_rounds: u32,
    pub diverse_legit_relay_rounds: u32,
    pub traffic_rounds: u32,
}

fn sybil_trust(now_ms: u64, idx: usize) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(35 * 86_400_000 + idx as u64 * 86_400_000),
        reliability: 0.97,
        spam_score: 0.01,
        relay_success_rate: 0.96,
        relay_success_count: 200,
        relay_failure_count: 2,
        mutual_contacts: 0,
        messages_accepted: 800,
        messages_rejected: 0,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("attacker-relay".into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

fn legit_trust(now_ms: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(60 * 86_400_000),
        reliability: 0.84,
        spam_score: 0.02,
        relay_success_rate: 0.82,
        relay_success_count: 120,
        relay_failure_count: 4,
        mutual_contacts: 3,
        messages_accepted: 900,
        messages_rejected: 3,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("mesh-lan".into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

fn build_eclipse_topology() -> (
    Vec<String>,
    HashMap<String, PeerTrust>,
    HashMap<String, PeerRoutingMeta>,
) {
    let mut peers = Vec::new();
    let mut trust = HashMap::new();
    let mut meta = HashMap::new();

    for i in 0..ATTACKER_PEERS {
        let id = format!("12D3KooWAttackerSybil{i:02}");
        peers.push(id.clone());
        trust.insert(id.clone(), sybil_trust(NOW_MS, i));
        meta.insert(
            id,
            PeerRoutingMeta {
                transport_hint: "attacker-relay".into(),
                first_encounter_ms: NOW_MS.saturating_sub(3 * 86_400_000),
                geohash_bucket: None,
            },
        );
    }

    peers.push(LEGIT_PEER.into());
    trust.insert(LEGIT_PEER.into(), legit_trust(NOW_MS));
    meta.insert(
        LEGIT_PEER.into(),
        PeerRoutingMeta {
            transport_hint: "mesh-lan".into(),
            first_encounter_ms: NOW_MS.saturating_sub(45 * 86_400_000),
            geohash_bucket: None,
        },
    );

    (peers, trust, meta)
}

fn build_geohash_eclipse_topology() -> (
    Vec<String>,
    HashMap<String, PeerTrust>,
    HashMap<String, PeerRoutingMeta>,
) {
    let (peers, mut trust, mut meta) = build_eclipse_topology();
    for id in peers.iter().filter(|p| *p != LEGIT_PEER) {
        if let Some(t) = trust.get_mut(id) {
            t.last_geohash_bucket = Some(GEO_ATTACKER_BUCKET.into());
        }
        if let Some(m) = meta.get_mut(id) {
            m.geohash_bucket = Some(GEO_ATTACKER_BUCKET.into());
        }
    }
    if let Some(t) = trust.get_mut(LEGIT_PEER) {
        t.last_geohash_bucket = Some(GEO_LEGIT_BUCKET.into());
    }
    if let Some(m) = meta.get_mut(LEGIT_PEER) {
        m.geohash_bucket = Some(GEO_LEGIT_BUCKET.into());
    }
    (peers, trust, meta)
}

fn relay_rounds_with_legit(
    peers: &[String],
    trust: &HashMap<String, PeerTrust>,
    meta: &HashMap<String, PeerRoutingMeta>,
    diversity: Option<&PeerDiversityConfig>,
    rounds: u32,
) -> u32 {
    let mut legit_hits = 0u32;
    for _ in 0..rounds {
        let selected = select_relay_candidates_with_diversity(&RelaySelectionRequest {
            peers,
            to_peer: DEST,
            peer_trust: trust,
            endorsement_boost: None,
            now_ms: NOW_MS,
            max_fanout: FANOUT,
            routing_meta: Some(meta),
            diversity,
        });
        if selected.iter().any(|p| p == LEGIT_PEER) {
            legit_hits += 1;
        }
    }
    legit_hits
}

/// Simulate an eclipse attempt: 90% of visible peers are attacker-controlled sybils with
/// inflated scores. Returns relay-round counts with and without diversity enforcement.
pub fn eclipse_relay_resilience(traffic_rounds: u32) -> EclipseScenarioReport {
    let rounds = traffic_rounds.max(1);
    let (peers, trust, meta) = build_eclipse_topology();
    let visible = peers.len();
    let attacker_share_pct = ((ATTACKER_PEERS as f64 / visible as f64) * 100.0).round() as u32;

    let score_only = relay_rounds_with_legit(
        &peers,
        &trust,
        &meta,
        Some(&PeerDiversityConfig::score_only()),
        rounds,
    );
    let diverse = relay_rounds_with_legit(&peers, &trust, &meta, None, rounds);

    EclipseScenarioReport {
        visible_peers: visible,
        attacker_share_pct,
        score_only_legit_relay_rounds: score_only,
        diverse_legit_relay_rounds: diverse,
        traffic_rounds: rounds,
    }
}

/// Geohash-clustered eclipse: sybils share one bucket; legit peer is geographically distinct.
pub fn geohash_eclipse_relay_resilience(traffic_rounds: u32) -> EclipseScenarioReport {
    let rounds = traffic_rounds.max(1);
    let (peers, trust, meta) = build_geohash_eclipse_topology();
    let visible = peers.len();
    let attacker_share_pct = ((ATTACKER_PEERS as f64 / visible as f64) * 100.0).round() as u32;

    let score_only = relay_rounds_with_legit(
        &peers,
        &trust,
        &meta,
        Some(&PeerDiversityConfig::score_only()),
        rounds,
    );
    let diverse = relay_rounds_with_legit(&peers, &trust, &meta, None, rounds);

    EclipseScenarioReport {
        visible_peers: visible,
        attacker_share_pct,
        score_only_legit_relay_rounds: score_only,
        diverse_legit_relay_rounds: diverse,
        traffic_rounds: rounds,
    }
}

/// Fraction of relay rounds that include at least one non-attacker transport bucket.
pub fn diverse_transport_share(
    peers: &[String],
    trust: &HashMap<String, PeerTrust>,
    meta: &HashMap<String, PeerRoutingMeta>,
    rounds: u32,
) -> f64 {
    let mut mixed = 0u32;
    for _ in 0..rounds.max(1) {
        let mut scored: Vec<(String, f64)> = peers
            .iter()
            .filter(|p| *p != DEST)
            .map(|p| {
                let t = trust
                    .get(p)
                    .cloned()
                    .unwrap_or_else(|| PeerTrust::cold_unknown(NOW_MS));
                (p.clone(), t.relay_selection_score(NOW_MS))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let picked = apply_peer_diversity(
            &scored,
            trust,
            meta,
            NOW_MS,
            FANOUT,
            &PeerDiversityConfig::default(),
        );
        let transports: HashSet<_> = picked
            .iter()
            .map(|p| meta[p].transport_hint.clone())
            .collect();
        if transports.len() > 1 {
            mixed += 1;
        }
    }
    mixed as f64 / rounds.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eclipse_sim_attacker_dominates_without_diversity() {
        let report = eclipse_relay_resilience(20);
        assert_eq!(report.visible_peers, ATTACKER_PEERS + 1);
        assert_eq!(report.attacker_share_pct, 90);
        assert_eq!(
            report.score_only_legit_relay_rounds, 0,
            "pure score ranking should fully eclipse legit peer"
        );
    }

    #[test]
    fn eclipse_sim_diversity_preserves_legit_traffic() {
        let report = eclipse_relay_resilience(20);
        assert!(
            report.diverse_legit_relay_rounds > 0,
            "diversity must keep at least part of traffic on legit peers: {report:?}"
        );
        assert_eq!(
            report.diverse_legit_relay_rounds, report.traffic_rounds,
            "established legit peer should appear in every diverse relay round"
        );
    }

    #[test]
    fn eclipse_sim_transport_mix_not_monoculture() {
        let (peers, trust, meta) = build_eclipse_topology();
        let share = diverse_transport_share(&peers, &trust, &meta, 10);
        assert!(
            share > 0.0,
            "diversity should spread relays across transport buckets"
        );
    }

    #[test]
    fn geohash_eclipse_sim_attacker_dominates_without_diversity() {
        let report = geohash_eclipse_relay_resilience(20);
        assert_eq!(report.score_only_legit_relay_rounds, 0);
    }

    #[test]
    fn geohash_eclipse_sim_diversity_preserves_legit_traffic() {
        let report = geohash_eclipse_relay_resilience(20);
        assert_eq!(
            report.diverse_legit_relay_rounds, report.traffic_rounds,
            "geohash diversity must keep legit peer in every round: {report:?}"
        );
    }
}
