// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Targeted SOS flooding — mass Sybil SOS must not force global crisis mode.

use mycena_core::crisis_mode::{evaluate_auto_activation, CrisisAutoConfig};
use mycena_node::{
    check_sos_ingest, is_sos_source_trusted_for_crisis, IngestGuard, PeerTrust, SosCrisisTracker,
    SosSecurityConfig,
};

pub const ATTACKER_SOS_PEERS: usize = 120;
pub const ATTACKS_PER_PEER: u32 = 8;
pub const MIN_TRUSTED_SOS_FOR_CRISIS: usize = 3;

const NOW_MS: u64 = 1_700_200_000_000;
const GEOHASH: &str = "u4pruydq";
const LOCAL_PREFIX: &str = "u4pr";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SosFloodReport {
    pub attacker_peers: usize,
    pub attacker_posts_accepted: u64,
    pub attacker_posts_rejected: u64,
    pub trusted_sources_after_attack: usize,
    pub crisis_after_attack: bool,
    pub crisis_after_three_legit: bool,
}

fn sybil_trust(now_ms: u64, idx: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(30 * 60_000 + idx * 500),
        reliability: 0.99,
        spam_score: 0.01,
        relay_success_rate: 0.99,
        relay_success_count: 500,
        relay_failure_count: 0,
        mutual_contacts: 0,
        messages_accepted: 2000,
        messages_rejected: 0,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("sybil-sos".into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

fn legit_trust(now_ms: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(60 * 86_400_000),
        reliability: 0.85,
        spam_score: 0.02,
        relay_success_rate: 0.82,
        relay_success_count: 120,
        relay_failure_count: 5,
        mutual_contacts: 3,
        messages_accepted: 500,
        messages_rejected: 2,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("mesh-wifi".into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

/// Simulate {ATTACKER_SOS_PEERS} Sybil peers flooding SOS in one geohash cell.
pub fn sos_flood_attack(_seed: u64) -> SosFloodReport {
    let mut tracker = SosCrisisTracker::default();
    let mut guard = IngestGuard::new();
    let security = SosSecurityConfig::default();
    let auto = CrisisAutoConfig::default();

    let mut accepted = 0u64;
    let mut rejected = 0u64;

    for i in 0..ATTACKER_SOS_PEERS {
        let peer = format!("S{i:04}");
        let trust = sybil_trust(NOW_MS, i as u64);
        assert!(!is_sos_source_trusted_for_crisis(&trust, NOW_MS, &security));
        for j in 0..ATTACKS_PER_PEER {
            let ts = NOW_MS + u64::from(j);
            match check_sos_ingest(&mut guard, &peer, Some(GEOHASH), &trust, ts, &security) {
                Ok(()) => {
                    accepted += 1;
                    tracker.record_observation(&peer, GEOHASH, &trust, ts);
                }
                Err(_) => rejected += 1,
            }
        }
    }

    let trusted = tracker.trusted_independent_sources_nearby(LOCAL_PREFIX, NOW_MS);
    let crisis_after_attack = evaluate_auto_activation(&auto, false, true, trusted);

    for (idx, id) in ["L001", "L002", "L003"].iter().enumerate() {
        let trust = legit_trust(NOW_MS);
        tracker.record_observation(id, GEOHASH, &trust, NOW_MS + 100 + idx as u64);
    }
    let trusted_legit = tracker.trusted_independent_sources_nearby(LOCAL_PREFIX, NOW_MS + 200);
    let crisis_after_three_legit = evaluate_auto_activation(&auto, false, true, trusted_legit);

    SosFloodReport {
        attacker_peers: ATTACKER_SOS_PEERS,
        attacker_posts_accepted: accepted,
        attacker_posts_rejected: rejected,
        trusted_sources_after_attack: trusted,
        crisis_after_attack,
        crisis_after_three_legit,
    }
}

/// CI gate: Sybil SOS flood must not flip crisis; three legit sources must.
pub fn run_sos_flood_ci_gate() -> anyhow::Result<SosFloodReport> {
    let report = sos_flood_attack(42);
    if report.crisis_after_attack {
        anyhow::bail!(
            "SOS flood CI gate: crisis activated with {} trusted sources after {} attacker peers",
            report.trusted_sources_after_attack,
            report.attacker_peers
        );
    }
    if report.trusted_sources_after_attack >= MIN_TRUSTED_SOS_FOR_CRISIS {
        anyhow::bail!(
            "SOS flood CI gate: {} sybil sources counted as trusted (max {})",
            report.trusted_sources_after_attack,
            MIN_TRUSTED_SOS_FOR_CRISIS - 1
        );
    }
    if !report.crisis_after_three_legit {
        anyhow::bail!("SOS flood CI gate: three legit SOS did not activate crisis");
    }
    if report.attacker_posts_rejected == 0 {
        anyhow::bail!("SOS flood CI gate: expected some attacker posts rate-limited");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sos_flood_does_not_force_crisis() {
        let report = sos_flood_attack(42);
        assert!(!report.crisis_after_attack, "{report:?}");
        assert!(report.trusted_sources_after_attack < MIN_TRUSTED_SOS_FOR_CRISIS);
        assert!(report.crisis_after_three_legit);
        assert!(report.attacker_posts_rejected > 0);
    }

    #[test]
    fn sos_flood_ci_gate_passes() {
        run_sos_flood_ci_gate().expect("CI gate");
    }
}
