// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Targeted resource-map disinformation — one actor floods contradictory pins.

use mycena_core::information_trust::CorroborationStatus;
use mycena_core::resource_map::{ResourceCategory, ResourceMapEntry};
use mycena_node::{InformationTrustTracker, PeerTrust};
use std::collections::HashMap;

pub const ATTACKER_RESOURCE_PEERS: usize = 80;
pub const REPORTS_PER_PEER: u32 = 6;
pub const MAX_TRUST_SCORE_UNDER_ATTACK: f32 = 0.4;

const NOW_MS: u64 = 1_700_300_000_000;
const GEOHASH: &str = "u4pruydq";

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDisinfoReport {
    pub attacker_peers: usize,
    pub observations_recorded: u64,
    pub claim_a_trust_score: f32,
    pub claim_b_trust_score: f32,
    pub claim_a_status: CorroborationStatus,
    pub claim_b_status: CorroborationStatus,
    pub claim_a_display: String,
    pub claim_b_display: String,
}

fn sybil_trust(now_ms: u64, idx: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(25 * 60_000 + idx * 400),
        reliability: 0.99,
        spam_score: 0.01,
        relay_success_rate: 0.99,
        relay_success_count: 300,
        relay_failure_count: 0,
        mutual_contacts: 0,
        messages_accepted: 1500,
        messages_rejected: 0,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some("sybil-resource".into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

fn legit_trust(now_ms: u64, transport: &str) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(45 * 86_400_000),
        reliability: 0.86,
        spam_score: 0.02,
        relay_success_rate: 0.84,
        relay_success_count: 90,
        relay_failure_count: 4,
        mutual_contacts: 3,
        messages_accepted: 400,
        messages_rejected: 1,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: Some(transport.into()),
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

/// One Sybil actor floods contradictory water reports at the same geohash cell.
pub fn resource_disinfo_attack(_seed: u64) -> ResourceDisinfoReport {
    let mut tracker = InformationTrustTracker::default();
    let claim_a = ResourceMapEntry {
        geohash: GEOHASH.into(),
        category: ResourceCategory::Water,
        description: "Water distribution at north gate".into(),
        created_at_ms: NOW_MS,
        ttl_secs: 3600,
    };
    let claim_b = ResourceMapEntry {
        geohash: GEOHASH.into(),
        category: ResourceCategory::Water,
        description: "Water distribution at south plaza".into(),
        created_at_ms: NOW_MS,
        ttl_secs: 3600,
    };

    let mut recorded = 0u64;
    let mut peers = HashMap::new();
    for i in 0..ATTACKER_RESOURCE_PEERS {
        let peer = format!("S{i:04}");
        let trust = sybil_trust(NOW_MS, i as u64);
        peers.insert(peer.clone(), trust.clone());
        for j in 0..REPORTS_PER_PEER {
            let entry = if (i + j as usize).is_multiple_of(2) {
                &claim_a
            } else {
                &claim_b
            };
            tracker.record_resource(&peer, entry, &trust, NOW_MS + u64::from(j));
            recorded += 1;
        }
    }

    let trust_a = tracker.query_resource_trust(&claim_a, NOW_MS, &peers);
    let trust_b = tracker.query_resource_trust(&claim_b, NOW_MS, &peers);

    ResourceDisinfoReport {
        attacker_peers: ATTACKER_RESOURCE_PEERS,
        observations_recorded: recorded,
        claim_a_trust_score: trust_a.trust_score,
        claim_b_trust_score: trust_b.trust_score,
        claim_a_status: trust_a.status,
        claim_b_status: trust_b.status,
        claim_a_display: trust_a.display_label(),
        claim_b_display: trust_b.display_label(),
    }
}

/// After attack, three legit sources confirming claim A should raise trust.
pub fn resource_disinfo_recovery_after_legit() -> ResourceDisinfoReport {
    let mut tracker = InformationTrustTracker::default();
    let claim_a = ResourceMapEntry {
        geohash: GEOHASH.into(),
        category: ResourceCategory::Water,
        description: "Water distribution at north gate".into(),
        created_at_ms: NOW_MS,
        ttl_secs: 3600,
    };
    let claim_b = ResourceMapEntry {
        geohash: GEOHASH.into(),
        category: ResourceCategory::Water,
        description: "Water distribution at south plaza".into(),
        created_at_ms: NOW_MS,
        ttl_secs: 3600,
    };

    let mut peers = HashMap::new();
    for i in 0..ATTACKER_RESOURCE_PEERS {
        let peer = format!("S{i:04}");
        let trust = sybil_trust(NOW_MS, i as u64);
        peers.insert(peer.clone(), trust.clone());
        tracker.record_resource(
            &peer,
            if i % 2 == 0 { &claim_a } else { &claim_b },
            &trust,
            NOW_MS,
        );
    }

    for (idx, (peer, transport)) in [("L1", "wifi"), ("L2", "ble"), ("L3", "relay")]
        .iter()
        .enumerate()
    {
        let trust = legit_trust(NOW_MS, transport);
        peers.insert(peer.to_string(), trust.clone());
        tracker.record_resource(peer, &claim_a, &trust, NOW_MS + 50 + idx as u64);
    }

    let trust_a = tracker.query_resource_trust(&claim_a, NOW_MS + 100, &peers);
    let trust_b = tracker.query_resource_trust(&claim_b, NOW_MS + 100, &peers);

    ResourceDisinfoReport {
        attacker_peers: ATTACKER_RESOURCE_PEERS,
        observations_recorded: 0,
        claim_a_trust_score: trust_a.trust_score,
        claim_b_trust_score: trust_b.trust_score,
        claim_a_status: trust_a.status,
        claim_b_status: trust_b.status,
        claim_a_display: trust_a.display_label(),
        claim_b_display: trust_b.display_label(),
    }
}

/// CI gate: contradictory Sybil flood keeps both claims low-trust and conflicting.
pub fn run_resource_disinfo_ci_gate() -> anyhow::Result<ResourceDisinfoReport> {
    let report = resource_disinfo_attack(42);
    if report.claim_a_status != CorroborationStatus::Conflicting
        || report.claim_b_status != CorroborationStatus::Conflicting
    {
        anyhow::bail!(
            "resource disinfo CI gate: expected conflicting status, got {:?} / {:?}",
            report.claim_a_status,
            report.claim_b_status
        );
    }
    if report.claim_a_trust_score > MAX_TRUST_SCORE_UNDER_ATTACK
        || report.claim_b_trust_score > MAX_TRUST_SCORE_UNDER_ATTACK
    {
        anyhow::bail!(
            "resource disinfo CI gate: trust scores too high under attack ({:.3}, {:.3})",
            report.claim_a_trust_score,
            report.claim_b_trust_score
        );
    }
    let recovery = resource_disinfo_recovery_after_legit();
    if recovery.claim_a_status != CorroborationStatus::Conflicting {
        anyhow::bail!(
            "resource disinfo CI gate: claim A should stay conflicting while B exists ({:?})",
            recovery.claim_a_status
        );
    }
    if recovery.claim_a_trust_score <= report.claim_a_trust_score {
        anyhow::bail!("resource disinfo CI gate: legit corroboration should raise claim A trust");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disinfo_attack_keeps_trust_low() {
        let report = resource_disinfo_attack(42);
        assert_eq!(report.claim_a_status, CorroborationStatus::Conflicting);
        assert_eq!(report.claim_b_status, CorroborationStatus::Conflicting);
        assert!(report.claim_a_trust_score < MAX_TRUST_SCORE_UNDER_ATTACK);
        assert!(report.claim_b_trust_score < MAX_TRUST_SCORE_UNDER_ATTACK);
        assert!(report.claim_a_display.contains("conflicting"));
    }

    #[test]
    fn disinfo_ci_gate_passes() {
        run_resource_disinfo_ci_gate().expect("CI gate");
    }
}
