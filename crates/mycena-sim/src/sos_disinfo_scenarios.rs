// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Targeted SOS / crisis-message disinformation — contradictory help claims at one cell.

use mycena_core::information_trust::CorroborationStatus;
use mycena_core::sos::SosPayload;
use mycena_node::{InformationTrustTracker, PeerTrust};
use std::collections::HashMap;

pub const ATTACKER_SOS_PEERS: usize = 100;
pub const REPORTS_PER_PEER: u32 = 5;
pub const MAX_TRUST_SCORE_UNDER_ATTACK: f32 = 0.4;

const NOW_MS: u64 = 1_700_400_000_000;
const GEOHASH: &str = "u4pruydq";

#[derive(Debug, Clone, PartialEq)]
pub struct SosDisinfoReport {
    pub attacker_peers: usize,
    pub claim_a_trust_score: f32,
    pub claim_b_trust_score: f32,
    pub claim_a_status: CorroborationStatus,
    pub claim_b_status: CorroborationStatus,
    pub claim_a_display: String,
    pub claim_b_display: String,
}

fn sybil_trust(now_ms: u64, idx: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(20 * 60_000 + idx * 300),
        reliability: 0.99,
        spam_score: 0.01,
        relay_success_rate: 0.99,
        relay_success_count: 250,
        relay_failure_count: 0,
        mutual_contacts: 0,
        messages_accepted: 1200,
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

/// One actor floods contradictory SOS messages for the same geohash cell.
pub fn sos_disinfo_attack(_seed: u64) -> SosDisinfoReport {
    let mut tracker = InformationTrustTracker::default();
    let claim_a = SosPayload {
        geohash: GEOHASH.into(),
        message: "Help needed at north shelter".into(),
        created_at_ms: NOW_MS,
    };
    let claim_b = SosPayload {
        geohash: GEOHASH.into(),
        message: "Help needed at south bridge".into(),
        created_at_ms: NOW_MS,
    };

    let mut peers = HashMap::new();
    for i in 0..ATTACKER_SOS_PEERS {
        let peer = format!("S{i:04}");
        let trust = sybil_trust(NOW_MS, i as u64);
        peers.insert(peer.clone(), trust.clone());
        for j in 0..REPORTS_PER_PEER {
            let payload = if (i + j as usize).is_multiple_of(2) {
                &claim_a
            } else {
                &claim_b
            };
            tracker.record_sos(&peer, payload, &trust, NOW_MS + u64::from(j));
        }
    }

    let trust_a = tracker.query_sos_trust(&claim_a, NOW_MS, &peers);
    let trust_b = tracker.query_sos_trust(&claim_b, NOW_MS, &peers);

    SosDisinfoReport {
        attacker_peers: ATTACKER_SOS_PEERS,
        claim_a_trust_score: trust_a.trust_score,
        claim_b_trust_score: trust_b.trust_score,
        claim_a_status: trust_a.status,
        claim_b_status: trust_b.status,
        claim_a_display: trust_a.display_label(),
        claim_b_display: trust_b.display_label(),
    }
}

/// CI gate: contradictory SOS flood keeps both crisis messages low-trust.
pub fn run_sos_disinfo_ci_gate() -> anyhow::Result<SosDisinfoReport> {
    let report = sos_disinfo_attack(42);
    if report.claim_a_status != CorroborationStatus::Conflicting
        || report.claim_b_status != CorroborationStatus::Conflicting
    {
        anyhow::bail!(
            "SOS disinfo CI gate: expected conflicting status, got {:?} / {:?}",
            report.claim_a_status,
            report.claim_b_status
        );
    }
    if report.claim_a_trust_score > MAX_TRUST_SCORE_UNDER_ATTACK
        || report.claim_b_trust_score > MAX_TRUST_SCORE_UNDER_ATTACK
    {
        anyhow::bail!(
            "SOS disinfo CI gate: trust scores too high ({:.3}, {:.3})",
            report.claim_a_trust_score,
            report.claim_b_trust_score
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sos_disinfo_attack_keeps_trust_low() {
        let report = sos_disinfo_attack(42);
        assert_eq!(report.claim_a_status, CorroborationStatus::Conflicting);
        assert_eq!(report.claim_b_status, CorroborationStatus::Conflicting);
        assert!(report.claim_a_trust_score < MAX_TRUST_SCORE_UNDER_ATTACK);
        assert!(report.claim_b_trust_score < MAX_TRUST_SCORE_UNDER_ATTACK);
    }

    #[test]
    fn sos_disinfo_ci_gate_passes() {
        run_sos_disinfo_ci_gate().expect("CI gate");
    }
}
