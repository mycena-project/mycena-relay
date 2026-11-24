// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Proptest: Sybil relay swarm must not outrank established peers (SD-031).

use mycena_node::security::{select_relay_candidates, PeerTrust};
use proptest::prelude::*;

fn established_trust(now_ms: u64, reliability: f32, relay_rate: f32, age_days: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(age_days * 86_400_000),
        reliability,
        spam_score: 0.02,
        relay_success_rate: relay_rate,
        relay_success_count: 100,
        relay_failure_count: 5,
        mutual_contacts: 2,
        messages_accepted: 500,
        messages_rejected: 2,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: None,
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

proptest! {
    #[test]
    fn sybil_swarm_never_displaces_established(
        sybil_count in 1usize..120,
        fanout in 1usize..=3usize,
        established_count in 1usize..=5usize,
    ) {
        prop_assume!(established_count + sybil_count >= fanout);
        let now = 1_700_000_000_000u64;
        let mut trust = std::collections::HashMap::new();
        let mut peers = Vec::new();

        for i in 0..established_count {
            let id = format!("12D3KooWEst{i:03}");
            peers.push(id.clone());
            trust.insert(id, established_trust(now, 0.91, 0.89, 10 + i as u64));
        }
        for i in 0..sybil_count {
            let id = format!("12D3KooWSybil{i:03}");
            peers.push(id.clone());
            trust.insert(id, PeerTrust::cold_unknown(now));
        }

        let selected = select_relay_candidates(&peers, "12D3KooWDest", &trust, None, now, fanout);
        prop_assert_eq!(selected.len(), fanout);
        let established_in_top = selected
            .iter()
            .filter(|p| p.starts_with("12D3KooWEst"))
            .count();
        prop_assert_eq!(established_in_top, established_count.min(fanout));
        if established_count >= fanout {
            prop_assert!(selected.iter().all(|p| p.starts_with("12D3KooWEst")));
        }
    }
}
