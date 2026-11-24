// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Persisted peer trust must influence relay selection after reopen.

use mycena_node::security::{select_relay_candidates, PeerTrust, PeerTrustStore};

const NOW_MS: u64 = 1_700_000_000_000;

fn established(now_ms: u64) -> PeerTrust {
    PeerTrust {
        first_seen_ms: now_ms.saturating_sub(30 * 86_400_000),
        reliability: 0.94,
        spam_score: 0.02,
        relay_success_rate: 0.91,
        relay_success_count: 120,
        relay_failure_count: 4,
        mutual_contacts: 2,
        messages_accepted: 800,
        messages_rejected: 1,
        strike_count: 0,
        last_strike_ms: 0,
        throttled_until_ms: 0,
        last_updated_ms: now_ms,
        transport_hint: None,
        last_geohash_bucket: None,
        last_geohash_ms: 0,
    }
}

#[test]
fn persisted_trust_survives_reopen_and_prefers_established() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_str().expect("path");

    {
        let store = PeerTrustStore::open(base).expect("open");
        store
            .with_peer("12D3KooWEstablishedRelay01", NOW_MS, |t| {
                *t = established(NOW_MS);
            })
            .expect("save established");
        store
            .with_peer("12D3KooWSybilFresh01", NOW_MS, |t| {
                *t = PeerTrust::cold_unknown(NOW_MS);
            })
            .expect("save sybil");
    }

    let store2 = PeerTrustStore::open(base).expect("reopen");
    let trust = store2.load_all();
    assert_eq!(trust.len(), 2);

    let peers = vec![
        "12D3KooWEstablishedRelay01".into(),
        "12D3KooWSybilFresh01".into(),
    ];
    let selected = select_relay_candidates(&peers, "12D3KooWDest", &trust, None, NOW_MS, 1);
    assert_eq!(selected, vec!["12D3KooWEstablishedRelay01"]);
}

#[test]
fn sybil_swarm_after_reopen_still_loses_to_established_triple() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_str().expect("path");

    {
        let store = PeerTrustStore::open(base).expect("open");
        for i in 0..3 {
            let id = format!("12D3KooWEst{i:02}");
            store
                .with_peer(&id, NOW_MS, |t| *t = established(NOW_MS))
                .expect("save");
        }
    }

    let store2 = PeerTrustStore::open(base).expect("reopen");
    let mut trust = store2.load_all();
    let mut peers: Vec<String> = trust.keys().cloned().collect();
    for i in 0..20 {
        let id = format!("12D3KooWSybil{i:02}");
        peers.push(id.clone());
        trust.insert(id, PeerTrust::cold_unknown(NOW_MS));
    }

    let selected = select_relay_candidates(&peers, "12D3KooWDest", &trust, None, NOW_MS, 3);
    assert_eq!(selected.len(), 3);
    assert!(selected.iter().all(|p| p.starts_with("12D3KooWEst")));
}
