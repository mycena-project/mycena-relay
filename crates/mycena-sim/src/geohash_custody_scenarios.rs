// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Geohash-clustered custody carrier selection — sybils share a bucket; diversity keeps legit path.

use mycena_node::{apply_peer_diversity, PeerDiversityConfig, PeerRoutingMeta, PeerTrust};
use std::collections::HashMap;

const NOW_MS: u64 = 1_700_000_000_000;
const GEO_SYBIL_BUCKET: &str = "u4pr";
const GEO_LEGIT_BUCKET: &str = "u33d";
const LEGIT_CARRIER: &str = "12D3KooWLegitCommuterCarrier";
const SYBIL_COUNT: usize = 4;

type GeohashCustodyTopology = (
    Vec<(String, f64)>,
    HashMap<String, PeerTrust>,
    HashMap<String, PeerRoutingMeta>,
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeohashCustodyCarrierReport {
    pub score_only_legit_rounds: u32,
    pub diverse_legit_rounds: u32,
    pub selection_rounds: u32,
}

fn sybil_trust(now_ms: u64, idx: usize) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(15 * 60 * 1000 + idx as u64 * 60 * 1000),
        reliability: 0.96,
        spam_score: 0.01,
        relay_success_rate: 0.94,
        relay_success_count: 180,
        relay_failure_count: 3,
        mutual_contacts: 0,
        messages_accepted: 700,
        messages_rejected: 0,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("attacker-relay".into()),
        last_geohash_bucket: Some(GEO_SYBIL_BUCKET.into()),
        last_geohash_ms: now_ms,
    }
}

fn legit_trust(now_ms: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(50 * 86_400_000),
        reliability: 0.86,
        spam_score: 0.02,
        relay_success_rate: 0.80,
        relay_success_count: 90,
        relay_failure_count: 5,
        mutual_contacts: 2,
        messages_accepted: 850,
        messages_rejected: 2,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("mesh-lan".into()),
        last_geohash_bucket: Some(GEO_LEGIT_BUCKET.into()),
        last_geohash_ms: now_ms,
    }
}

fn build_geohash_custody_topology() -> GeohashCustodyTopology {
    let mut trust = HashMap::new();
    let mut meta = HashMap::new();
    let mut scored = Vec::new();

    for i in 0..SYBIL_COUNT {
        let id = format!("12D3KooWSybilCarrier{i:02}");
        trust.insert(id.clone(), sybil_trust(NOW_MS, i));
        meta.insert(
            id.clone(),
            PeerRoutingMeta {
                transport_hint: "attacker-relay".into(),
                first_encounter_ms: NOW_MS.saturating_sub(15 * 60 * 1000),
                geohash_bucket: Some(GEO_SYBIL_BUCKET.into()),
            },
        );
        scored.push((id, 0.95 - i as f64 * 0.01));
    }

    trust.insert(LEGIT_CARRIER.into(), legit_trust(NOW_MS));
    meta.insert(
        LEGIT_CARRIER.into(),
        PeerRoutingMeta {
            transport_hint: "mesh-lan".into(),
            first_encounter_ms: NOW_MS.saturating_sub(40 * 86_400_000),
            geohash_bucket: Some(GEO_LEGIT_BUCKET.into()),
        },
    );
    scored.push((LEGIT_CARRIER.into(), 0.82));

    (scored, trust, meta)
}

fn carrier_rounds_with_legit(
    scored: &[(String, f64)],
    trust: &HashMap<String, PeerTrust>,
    meta: &HashMap<String, PeerRoutingMeta>,
    diversity: &PeerDiversityConfig,
    rounds: u32,
) -> u32 {
    let mut legit_hits = 0u32;
    for _ in 0..rounds {
        let picked = apply_peer_diversity(scored, trust, meta, NOW_MS, 1, diversity);
        if picked.iter().any(|p| p == LEGIT_CARRIER) {
            legit_hits += 1;
        }
    }
    legit_hits
}

/// Custody-style carrier pick (max fanout 1): young geohash-clustered sybils vs established legit.
pub fn geohash_custody_carrier_resilience(selection_rounds: u32) -> GeohashCustodyCarrierReport {
    let rounds = selection_rounds.max(1);
    let (scored, trust, meta) = build_geohash_custody_topology();
    let score_only = carrier_rounds_with_legit(
        &scored,
        &trust,
        &meta,
        &PeerDiversityConfig::score_only(),
        rounds,
    );
    let diverse = carrier_rounds_with_legit(
        &scored,
        &trust,
        &meta,
        &PeerDiversityConfig::default(),
        rounds,
    );
    GeohashCustodyCarrierReport {
        score_only_legit_rounds: score_only,
        diverse_legit_rounds: diverse,
        selection_rounds: rounds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geohash_custody_sybls_win_without_diversity() {
        let report = geohash_custody_carrier_resilience(20);
        assert_eq!(
            report.score_only_legit_rounds, 0,
            "score-only should pick high-scoring young sybil carriers"
        );
    }

    #[test]
    fn geohash_custody_diversity_picks_legit_carrier() {
        let report = geohash_custody_carrier_resilience(20);
        assert!(
            report.diverse_legit_rounds > 0,
            "diversity must defer young sybils and pick established legit carrier: {report:?}"
        );
        assert_eq!(
            report.diverse_legit_rounds, report.selection_rounds,
            "established legit carrier should win every diverse round: {report:?}"
        );
    }
}
