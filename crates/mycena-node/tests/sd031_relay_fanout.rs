// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! SD-031: unknown-destination relay fanout must be bounded.

use mycena_node::security::{select_relay_candidates, PeerTrust};
use std::collections::HashMap;

const NOW_MS: u64 = 1_700_000_000_000;

fn trust_with_strikes(strikes: u8) -> PeerTrust {
    let mut t = PeerTrust::new_peer(NOW_MS);
    t.strike_count = strikes;
    t.reliability = 0.85;
    t.relay_success_rate = 0.80;
    t.first_seen_ms = NOW_MS - 86_400_000;
    t
}

#[test]
fn fanout_is_bounded() {
    const MAX_RELAY_FANOUT: usize = 3;
    let peers: Vec<String> = (0..10).map(|i| format!("12D3KooWPeer{i:02}")).collect();
    let target = "12D3KooWUnknown".to_string();
    let selected = select_relay_candidates(
        &peers,
        &target,
        &HashMap::new(),
        None,
        NOW_MS,
        MAX_RELAY_FANOUT,
    );
    assert_eq!(selected.len(), MAX_RELAY_FANOUT);
    assert!(!selected.contains(&target));
}

#[test]
fn lower_strike_peers_preferred() {
    let peers = vec![
        "peer_a".into(),
        "peer_b".into(),
        "peer_c".into(),
        "peer_d".into(),
    ];
    let mut trust = HashMap::new();
    trust.insert("peer_a".into(), trust_with_strikes(5));
    trust.insert("peer_b".into(), trust_with_strikes(0));
    trust.insert("peer_c".into(), trust_with_strikes(1));
    trust.insert("peer_d".into(), trust_with_strikes(0));
    let selected = select_relay_candidates(&peers, "missing", &trust, None, NOW_MS, 2);
    assert_eq!(selected.len(), 2);
    assert!(selected.contains(&"peer_b".to_string()) || selected.contains(&"peer_d".to_string()));
}
