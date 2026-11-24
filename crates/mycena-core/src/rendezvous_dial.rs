// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Bucketed rendezvous peer selection to avoid dial storms (Phase D4).

const DEFAULT_BUCKETS: u32 = 16;
const DEFAULT_MAX_PER_TICK: usize = 3;

/// Peers chosen for one rendezvous poll cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendezvousDialPlan {
    pub to_dial: Vec<String>,
    pub bucket: u32,
    pub num_buckets: u32,
}

/// Select up to `max_per_tick` peers: priority first, then hash-bucket matches.
pub fn plan_rendezvous_dials(
    local_peer_id: &str,
    remote_ids: &[String],
    priority_peers: &[String],
    already_connected: &[String],
    bucket: u32,
    max_per_tick: usize,
    num_buckets: u32,
) -> RendezvousDialPlan {
    let buckets = num_buckets.max(1);
    let cap = max_per_tick.max(1);
    let connected: std::collections::HashSet<&str> =
        already_connected.iter().map(String::as_str).collect();
    let local = local_peer_id.trim();

    let mut candidates: Vec<String> = remote_ids
        .iter()
        .filter(|id| {
            let p = id.trim();
            !p.is_empty() && p != local && !connected.contains(p)
        })
        .cloned()
        .collect();

    let priority_set: std::collections::HashSet<&str> =
        priority_peers.iter().map(String::as_str).collect();

    candidates.sort_by(|a, b| {
        let pa = priority_set.contains(a.as_str());
        let pb = priority_set.contains(b.as_str());
        pb.cmp(&pa).then_with(|| a.cmp(b))
    });

    let mut to_dial = Vec::new();
    for peer in candidates {
        if to_dial.len() >= cap {
            break;
        }
        if priority_set.contains(peer.as_str()) {
            to_dial.push(peer);
            continue;
        }
        if peer_in_bucket(&peer, bucket, buckets) {
            to_dial.push(peer);
        }
    }

    RendezvousDialPlan {
        to_dial,
        bucket: bucket % buckets,
        num_buckets: buckets,
    }
}

fn peer_in_bucket(peer_id: &str, bucket: u32, num_buckets: u32) -> bool {
    let h = blake3::hash(peer_id.as_bytes());
    let slot = match h.as_bytes()[0..4].try_into() {
        Ok(bytes) => u32::from_le_bytes(bytes) % num_buckets,
        // INVARIANT: blake3 output is 32 bytes; slicing [0..4] always fits [u8; 4].
        Err(_) => return false,
    };
    slot == bucket % num_buckets
}

pub fn default_rendezvous_plan(
    local_peer_id: &str,
    remote_ids: &[String],
    priority_peers: &[String],
    already_connected: &[String],
    bucket: u32,
) -> RendezvousDialPlan {
    plan_rendezvous_dials(
        local_peer_id,
        remote_ids,
        priority_peers,
        already_connected,
        bucket,
        DEFAULT_MAX_PER_TICK,
        DEFAULT_BUCKETS,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn peers(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("12D3KooWPeer{i:04}ForRendezvousDialTestOnly0123456"))
            .collect()
    }

    #[test]
    fn caps_dials_per_tick() {
        let remote = peers(100);
        let plan = default_rendezvous_plan(
            "12D3KooWLocalPeerForTestOnly012345678901234",
            &remote,
            &[],
            &[],
            0,
        );
        assert!(plan.to_dial.len() <= DEFAULT_MAX_PER_TICK);
    }

    #[test]
    fn priority_peers_dialed_first() {
        let remote = peers(50);
        let priority = vec![remote[7].clone(), remote[3].clone()];
        let plan = default_rendezvous_plan(
            "12D3KooWLocalPeerForTestOnly012345678901234",
            &remote,
            &priority,
            &[],
            0,
        );
        assert!(plan.to_dial.contains(&remote[7]));
    }

    #[test]
    fn skips_already_connected() {
        let remote = peers(10);
        let connected = vec![remote[0].clone()];
        let plan = default_rendezvous_plan(
            "12D3KooWLocalPeerForTestOnly012345678901234",
            &remote,
            &[],
            &connected,
            0,
        );
        assert!(!plan.to_dial.contains(&remote[0]));
    }

    #[test]
    fn bucket_rotation_changes_selection() {
        let remote = peers(200);
        let plan0 = default_rendezvous_plan(
            "12D3KooWLocalPeerForTestOnly012345678901234",
            &remote,
            &[],
            &[],
            0,
        );
        let plan1 = default_rendezvous_plan(
            "12D3KooWLocalPeerForTestOnly012345678901234",
            &remote,
            &[],
            &[],
            1,
        );
        assert_ne!(plan0.to_dial, plan1.to_dial);
    }
}
