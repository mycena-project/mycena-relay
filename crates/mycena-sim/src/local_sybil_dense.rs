// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Dense local Sybil cluster — 200 legit + 500 attacker peers in one radio neighborhood.
//!
//! Threat model: co-located fake devices degrade mesh relay/custody for real users
//! (availability eclipse), not targeted de-anonymization of one individual.

use mycena_node::{
    build_routing_meta_map, select_relay_candidates_with_diversity, PeerDiversityConfig,
    PeerRoutingMeta, PeerTrust, RelaySelectionRequest,
};
use std::collections::{HashMap, HashSet};

pub const LEGIT_PEER_COUNT: usize = 200;
pub const SYBIL_PEER_COUNT: usize = 500;
pub const TOTAL_PEER_COUNT: usize = LEGIT_PEER_COUNT + SYBIL_PEER_COUNT;

/// Minimum legit→legit delivery rate with diversity under 500-local Sybil peers (CI gate).
pub const MIN_LEGIT_LEGIT_DELIVERY_RATE: f64 = 0.55;

const NOW_MS: u64 = 1_700_100_000_000;
const RADIO_M: f64 = 54.0;
const GRID_SPACING_M: f64 = 24.0;
const MAX_HOPS: u8 = 72;
const SYBIL_DROP_PER_MILLE: u16 = 850;
const MESSAGE_SAMPLES: usize = 120;
const SIM_STEPS: u32 = 1200;
const RELAY_FANOUT: usize = 4;
const ROUTING_CANDIDATE_POOL: usize = 28;
/// Max geographic separation for sampled legit→legit messages (local venue zone).
const MAX_PAIR_DISTANCE_M: f64 = 520.0;

type LocalSybilTopology = (
    Vec<String>,
    HashMap<String, (f64, f64)>,
    HashMap<String, Vec<String>>,
);

#[derive(Debug, Clone, PartialEq)]
pub struct LocalSybilDenseReport {
    pub legit_nodes: usize,
    pub sybil_nodes: usize,
    pub messages_attempted: u64,
    pub messages_delivered_diverse: u64,
    pub messages_delivered_score_only: u64,
    pub legit_legit_delivery_rate_diverse: f64,
    pub legit_legit_delivery_rate_score_only: f64,
}

fn is_legit(id: &str) -> bool {
    id.starts_with('L')
}

fn is_sybil(id: &str) -> bool {
    id.starts_with('S')
}

fn legit_trust(now_ms: u64, idx: usize) -> PeerTrust {
    let transport = match idx % 3 {
        0 => "mesh-ble",
        1 => "mesh-wifi",
        _ => "mesh-lan",
    };
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(45 * 86_400_000 + idx as u64 * 3_600_000),
        reliability: 0.62,
        spam_score: 0.04,
        relay_success_rate: 0.60,
        relay_success_count: 70,
        relay_failure_count: 12,
        mutual_contacts: 2,
        messages_accepted: 400,
        messages_rejected: 4,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some(transport.into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

fn sybil_trust(now_ms: u64, idx: usize) -> PeerTrust {
    // Appeared ~90 min ago: young for age-diversity, but past relay score youth penalty.
    let age_ms = 90 * 60_000 + idx as u64 * 15_000;
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(age_ms),
        reliability: 0.99,
        spam_score: 0.01,
        relay_success_rate: 0.99,
        relay_success_count: 800,
        relay_failure_count: 0,
        mutual_contacts: 0,
        messages_accepted: 3000,
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

fn build_topology(seed: u64) -> LocalSybilTopology {
    let cols = 35usize;
    let mut rng = seed.max(1);
    let mut cells: Vec<bool> = (0..TOTAL_PEER_COUNT)
        .map(|i| i < LEGIT_PEER_COUNT)
        .collect();
    for i in (1..cells.len()).rev() {
        rng = lcg(rng);
        let j = (rng as usize) % (i + 1);
        cells.swap(i, j);
    }

    let mut ids = Vec::with_capacity(TOTAL_PEER_COUNT);
    let mut positions = HashMap::new();
    let mut legit_idx = 0usize;
    let mut sybil_idx = 0usize;
    for (cell, is_legit_cell) in cells.into_iter().enumerate() {
        let row = cell / cols;
        let col = cell % cols;
        let id = if is_legit_cell {
            let id = format!("L{legit_idx:03}");
            legit_idx += 1;
            id
        } else {
            let id = format!("S{sybil_idx:03}");
            sybil_idx += 1;
            id
        };
        positions.insert(
            id.clone(),
            (col as f64 * GRID_SPACING_M, row as f64 * GRID_SPACING_M),
        );
        ids.push(id);
    }
    assert_eq!(legit_idx, LEGIT_PEER_COUNT);
    assert_eq!(sybil_idx, SYBIL_PEER_COUNT);

    let mut neighbors: HashMap<String, Vec<String>> = HashMap::new();
    for a in &ids {
        let (ax, ay) = positions[a];
        let mut nbrs = Vec::new();
        for b in &ids {
            if a == b {
                continue;
            }
            let (bx, by) = positions[b];
            let dx = ax - bx;
            let dy = ay - by;
            if (dx * dx + dy * dy).sqrt() <= RADIO_M {
                nbrs.push(b.clone());
            }
        }
        nbrs.sort();
        neighbors.insert(a.clone(), nbrs);
    }
    (ids, positions, neighbors)
}

fn build_trust_maps(
    ids: &[String],
) -> (HashMap<String, PeerTrust>, HashMap<String, PeerRoutingMeta>) {
    let mut trust = HashMap::new();
    let mut hints = HashMap::new();
    for id in ids {
        if is_legit(id) {
            let idx = id.trim_start_matches('L').parse::<usize>().unwrap_or(0);
            let t = legit_trust(NOW_MS, idx);
            hints.insert(id.clone(), t.transport_hint.clone().unwrap_or_default());
            trust.insert(id.clone(), t);
        } else {
            let idx = id.trim_start_matches('S').parse::<usize>().unwrap_or(0);
            let t = sybil_trust(NOW_MS, idx);
            hints.insert(id.clone(), "sybil-local".into());
            trust.insert(id.clone(), t);
        }
    }
    let encounter_first: HashMap<String, u64> = trust
        .iter()
        .map(|(id, t)| (id.clone(), t.first_seen_ms))
        .collect();
    let meta = build_routing_meta_map(ids, &trust, &encounter_first, &hints);
    (trust, meta)
}

struct InFlight {
    id: usize,
    dest: String,
    holder: String,
    hops: u8,
}

fn dist(pos: (f64, f64), dest: (f64, f64)) -> f64 {
    let dx = pos.0 - dest.0;
    let dy = pos.1 - dest.1;
    (dx * dx + dy * dy).sqrt()
}

fn pick_relays(
    holder: &str,
    dest: &str,
    neighbors: &[String],
    trust: &HashMap<String, PeerTrust>,
    meta: &HashMap<String, PeerRoutingMeta>,
    positions: &HashMap<String, (f64, f64)>,
    diversity: &PeerDiversityConfig,
) -> Vec<String> {
    let holder_pos = match positions.get(holder) {
        Some(p) => *p,
        None => return Vec::new(),
    };
    let dest_pos = match positions.get(dest) {
        Some(p) => *p,
        None => return Vec::new(),
    };
    let holder_dist = dist(holder_pos, dest_pos);

    let mut pool: Vec<String> = neighbors
        .iter()
        .filter(|p| *p != holder && *p != dest)
        .cloned()
        .collect();
    pool.sort_by(|a, b| {
        dist(*positions.get(a).unwrap_or(&holder_pos), dest_pos)
            .partial_cmp(&dist(*positions.get(b).unwrap_or(&holder_pos), dest_pos))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pool.truncate(ROUTING_CANDIDATE_POOL);

    let diverse = select_relay_candidates_with_diversity(&RelaySelectionRequest {
        peers: &pool,
        to_peer: dest,
        peer_trust: trust,
        endorsement_boost: None,
        now_ms: NOW_MS,
        max_fanout: RELAY_FANOUT,
        routing_meta: Some(meta),
        diversity: Some(diversity),
    });

    let progressing: Vec<String> = diverse
        .iter()
        .filter(|p| {
            positions
                .get(p.as_str())
                .is_some_and(|pos| dist(*pos, dest_pos) < holder_dist)
        })
        .cloned()
        .collect();
    if !progressing.is_empty() {
        return progressing;
    }
    diverse
}

fn simulate_delivery(seed: u64, diversity: PeerDiversityConfig) -> (u64, u64) {
    let (ids, positions, neighbors) = build_topology(seed);
    let (trust, meta) = build_trust_maps(&ids);
    let legit_ids: Vec<String> = ids.iter().filter(|id| is_legit(id)).cloned().collect();
    let mut rng = seed.max(1);

    let mut messages: Vec<InFlight> = Vec::with_capacity(MESSAGE_SAMPLES);
    for i in 0..MESSAGE_SAMPLES {
        let mut attempts = 0u32;
        loop {
            rng = lcg(rng);
            let from = &legit_ids[(rng as usize) % legit_ids.len()];
            rng = lcg(rng);
            let to = &legit_ids[(rng as usize) % legit_ids.len()];
            attempts += 1;
            if from == to {
                if attempts > 32 {
                    break;
                }
                continue;
            }
            let from_pos = positions[from];
            let to_pos = positions[to];
            if dist(from_pos, to_pos) <= MAX_PAIR_DISTANCE_M || attempts > 48 {
                messages.push(InFlight {
                    id: i,
                    dest: to.clone(),
                    holder: from.clone(),
                    hops: 0,
                });
                break;
            }
        }
    }

    let attempted = messages.len() as u64;
    let mut delivered_ids = HashSet::new();
    let mut visited: HashSet<(usize, String)> = HashSet::new();

    for _step in 0..SIM_STEPS {
        let mut next: Vec<InFlight> = Vec::new();
        for msg in messages {
            if delivered_ids.contains(&msg.id) {
                continue;
            }
            if msg.holder == msg.dest {
                delivered_ids.insert(msg.id);
                continue;
            }
            if msg.hops >= MAX_HOPS {
                continue;
            }
            let nbrs = neighbors.get(&msg.holder).map(Vec::as_slice).unwrap_or(&[]);
            if nbrs.contains(&msg.dest) {
                let key = (msg.id, msg.dest.clone());
                if visited.insert(key) {
                    next.push(InFlight {
                        id: msg.id,
                        dest: msg.dest.clone(),
                        holder: msg.dest,
                        hops: msg.hops.saturating_add(1),
                    });
                }
                continue;
            }
            let relays = pick_relays(
                &msg.holder,
                &msg.dest,
                nbrs,
                &trust,
                &meta,
                &positions,
                &diversity,
            );
            if relays.is_empty() {
                next.push(msg);
                continue;
            }
            let mut forwarded = false;
            for relay in relays {
                if is_sybil(&relay) {
                    rng = lcg(rng);
                    if (rng % 1000) < SYBIL_DROP_PER_MILLE as u64 {
                        continue;
                    }
                }
                let key = (msg.id, relay.clone());
                if !visited.insert(key) {
                    continue;
                }
                forwarded = true;
                next.push(InFlight {
                    id: msg.id,
                    dest: msg.dest.clone(),
                    holder: relay,
                    hops: msg.hops.saturating_add(1),
                });
            }
            if !forwarded {
                next.push(msg);
            }
        }
        messages = next;
        if messages.is_empty() {
            break;
        }
    }

    (attempted, delivered_ids.len() as u64)
}

fn lcg(state: u64) -> u64 {
    state.wrapping_mul(6364136223846793005).wrapping_add(1)
}

/// Run 200 legit + 500 local Sybil peers; compare diversity vs score-only routing.
pub fn local_sybil_dense_cluster(seed: u64) -> LocalSybilDenseReport {
    let (attempted, diverse_delivered) =
        simulate_delivery(seed, PeerDiversityConfig::dense_local_sybil());
    let (_, score_delivered) = simulate_delivery(seed, PeerDiversityConfig::score_only());

    LocalSybilDenseReport {
        legit_nodes: LEGIT_PEER_COUNT,
        sybil_nodes: SYBIL_PEER_COUNT,
        messages_attempted: attempted,
        messages_delivered_diverse: diverse_delivered,
        messages_delivered_score_only: score_delivered,
        legit_legit_delivery_rate_diverse: diverse_delivered as f64 / attempted as f64,
        legit_legit_delivery_rate_score_only: score_delivered as f64 / attempted as f64,
    }
}

/// CI gate: legit→legit delivery stays above [`MIN_LEGIT_LEGIT_DELIVERY_RATE`].
pub fn run_local_sybil_dense_ci_gate() -> anyhow::Result<LocalSybilDenseReport> {
    let report = local_sybil_dense_cluster(42);
    if report.legit_legit_delivery_rate_diverse < MIN_LEGIT_LEGIT_DELIVERY_RATE {
        anyhow::bail!(
            "local sybil dense CI gate: diverse legit delivery {:.1}% < {:.0}% (score-only {:.1}%)",
            report.legit_legit_delivery_rate_diverse * 100.0,
            MIN_LEGIT_LEGIT_DELIVERY_RATE * 100.0,
            report.legit_legit_delivery_rate_score_only * 100.0,
        );
    }
    if report.legit_legit_delivery_rate_diverse
        <= report.legit_legit_delivery_rate_score_only + 0.10
    {
        anyhow::bail!(
            "local sybil dense CI gate: diversity did not outperform score-only ({:.1}% vs {:.1}%)",
            report.legit_legit_delivery_rate_diverse * 100.0,
            report.legit_legit_delivery_rate_score_only * 100.0,
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sybil_dense_700_peer_topology() {
        let (ids, _, neighbors) = build_topology(7);
        assert_eq!(ids.len(), TOTAL_PEER_COUNT);
        let legit_count = ids.iter().filter(|id| is_legit(id)).count();
        assert_eq!(legit_count, LEGIT_PEER_COUNT);
        let avg_degree: f64 =
            neighbors.values().map(|v| v.len()).sum::<usize>() as f64 / neighbors.len() as f64;
        assert!(
            avg_degree > 8.0,
            "dense cluster avg degree {avg_degree} too sparse"
        );
    }

    #[test]
    fn local_sybil_dense_relay_selection_differs() {
        let (ids, positions, neighbors) = build_topology(42);
        let (trust, meta) = build_trust_maps(&ids);
        let legit = ids.iter().find(|id| is_legit(id)).expect("legit");
        let dest = ids
            .iter()
            .find(|id| is_legit(id) && *id != legit)
            .expect("dest");
        let nbrs = &neighbors[legit];
        assert!(!nbrs.is_empty(), "legit must have neighbors");
        let sybil_score = sybil_trust(NOW_MS, 0).relay_selection_score(NOW_MS);
        let legit_score = legit_trust(NOW_MS, 0).relay_selection_score(NOW_MS);
        assert!(
            sybil_score > legit_score,
            "sybil must outscore legit in score-only (sybil={sybil_score}, legit={legit_score})"
        );
        let score_only = pick_relays(
            legit,
            dest,
            nbrs,
            &trust,
            &meta,
            &positions,
            &PeerDiversityConfig::score_only(),
        );
        let diverse = pick_relays(
            legit,
            dest,
            nbrs,
            &trust,
            &meta,
            &positions,
            &PeerDiversityConfig::dense_local_sybil(),
        );
        let legit_in = |v: &[String]| v.iter().filter(|p| is_legit(p)).count();
        assert!(
            legit_in(&diverse) >= legit_in(&score_only),
            "diverse {diverse:?} vs score-only {score_only:?}"
        );
        assert!(
            legit_in(&diverse) > 0,
            "diversity must retain at least one legit relay among {nbrs:?}"
        );
    }

    #[test]
    fn local_sybil_dense_diversity_beats_score_only() {
        let report = local_sybil_dense_cluster(42);
        assert!(
            report.legit_legit_delivery_rate_diverse >= MIN_LEGIT_LEGIT_DELIVERY_RATE,
            "diverse {:.1}% score-only {:.1}%",
            report.legit_legit_delivery_rate_diverse * 100.0,
            report.legit_legit_delivery_rate_score_only * 100.0,
        );
        assert!(
            report.legit_legit_delivery_rate_diverse > report.legit_legit_delivery_rate_score_only,
            "report: {report:?}"
        );
    }

    #[test]
    fn local_sybil_dense_ci_gate_passes() {
        run_local_sybil_dense_ci_gate().expect("CI gate");
    }
}
