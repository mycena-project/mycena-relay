// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Multi-relay bootstrap pool with hash-based client rotation (Phase D2, SD-071).

use crate::bootstrap::{peer_id_from_multiaddr, standard_bootstrap_tcp_ip4_multiaddrs};
use crate::relay_diversity::{select_parallel_dial_indices, RelayDiversityPolicy};
use crate::relay_registry::{
    ipv4_prefix_from_multiaddr, RelayRegistration, RelayRegistryManifest, VerifiedRelayRegistry,
};

/// One public relay reachable via raw TCP (and optional QUIC in bootstrap list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEntry {
    pub peer_id: String,
    /// TCP multiaddr including `/p2p/<relay_id>` (no circuit suffix).
    pub tcp_multiaddr: String,
    pub operator_label: String,
    pub region: String,
    pub operator_pubkey_hex: String,
    pub ipv4_prefix: Option<String>,
    pub asn: u32,
    /// Client-side consecutive dial/reservation failures (not persisted).
    pub consecutive_failures: u8,
}

#[derive(Debug, Clone, Default)]
pub struct RelayPool {
    entries: Vec<RelayEntry>,
}

impl RelayPool {
    /// Build a deduplicated pool from bootstrap multiaddrs (TCP entries only).
    pub fn from_bootstrap_peers(peers: &[impl AsRef<str>]) -> Self {
        let mut entries: Vec<RelayEntry> = Vec::new();
        for addr in peers {
            let s = addr.as_ref().trim();
            if s.is_empty() || !s.contains("/tcp/") || s.contains("/p2p-circuit") {
                continue;
            }
            let Some(relay_id) = peer_id_from_multiaddr(s) else {
                continue;
            };
            if entries.iter().any(|e| e.peer_id == relay_id) {
                continue;
            }
            entries.push(RelayEntry {
                peer_id: relay_id,
                tcp_multiaddr: s.to_string(),
                operator_label: String::new(),
                region: String::new(),
                operator_pubkey_hex: String::new(),
                ipv4_prefix: ipv4_prefix_from_multiaddr(s),
                asn: 0,
                consecutive_failures: 0,
            });
        }
        if entries.is_empty() {
            for addr in standard_bootstrap_tcp_ip4_multiaddrs() {
                let Some(relay_id) = peer_id_from_multiaddr(&addr) else {
                    continue;
                };
                if entries.iter().any(|e| e.peer_id == relay_id) {
                    continue;
                }
                entries.push(RelayEntry {
                    peer_id: relay_id,
                    tcp_multiaddr: addr.clone(),
                    operator_label: String::new(),
                    region: String::new(),
                    operator_pubkey_hex: String::new(),
                    ipv4_prefix: ipv4_prefix_from_multiaddr(&addr),
                    asn: 0,
                    consecutive_failures: 0,
                });
            }
        }
        Self { entries }
    }

    /// Federated pool from a verified registry manifest.
    pub fn from_verified_registry(verified: &VerifiedRelayRegistry) -> Self {
        let mut entries: Vec<RelayEntry> = Vec::new();
        for reg in &verified.dialable {
            if let Some(new_entry) = relay_entry_from_registration(reg) {
                if entries
                    .iter()
                    .any(|existing| existing.peer_id == new_entry.peer_id)
                {
                    continue;
                }
                entries.push(new_entry);
            }
        }
        if entries.is_empty() {
            return Self::from_bootstrap_peers(&[] as &[&str]);
        }
        Self { entries }
    }

    pub fn from_registry_manifest(manifest: &RelayRegistryManifest, include_catalog: bool) -> Self {
        match manifest.verify_and_filter(include_catalog) {
            Ok(v) => Self::from_verified_registry(&v),
            Err(_) => Self::from_bootstrap_peers(&[] as &[&str]),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[RelayEntry] {
        &self.entries
    }

    pub fn is_relay_peer(&self, peer_id: &str) -> bool {
        let p = peer_id.trim();
        self.entries.iter().any(|e| e.peer_id == p)
    }

    /// Indices for a diverse parallel bootstrap dial batch (ASN /24 / operator spread).
    pub fn select_parallel_dial_indices(
        &self,
        seed: &str,
        policy: &RelayDiversityPolicy,
    ) -> Vec<usize> {
        select_parallel_dial_indices(&self.entries, seed, policy)
    }

    /// Stable relay pick for a remote peer (spreads load across the pool).
    pub fn select_index_for(&self, seed: &str) -> usize {
        self.select_index_for_with_skip(seed, &[])
    }

    /// Pick circuit relay preferring the diverse parallel batch, then hash fallback.
    pub fn select_diverse_index_for(&self, seed: &str) -> usize {
        let policy = RelayDiversityPolicy::default();
        let batch = self.select_parallel_dial_indices(seed, &policy);
        if let Some(&idx) = batch.first() {
            return idx;
        }
        self.select_index_for_with_skip(seed, &[])
    }

    /// Pick relay index skipping failed/unhealthy entries when possible.
    pub fn select_index_for_with_skip(&self, seed: &str, skip: &[usize]) -> usize {
        if self.entries.is_empty() {
            return 0;
        }
        let h = blake3::hash(seed.as_bytes());
        let b = h.as_bytes();
        let base = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as usize
            % self.entries.len();
        for offset in 0..self.entries.len() {
            let idx = (base + offset) % self.entries.len();
            if skip.contains(&idx) {
                continue;
            }
            if self.entries[idx].consecutive_failures >= 3 {
                continue;
            }
            return idx;
        }
        base
    }

    pub fn entry(&self, index: usize) -> Option<&RelayEntry> {
        self.entries.get(index % self.entries.len().max(1))
    }

    pub fn entry_mut(&mut self, index: usize) -> Option<&mut RelayEntry> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = index % self.entries.len();
        self.entries.get_mut(idx)
    }

    /// Rotate to next healthy relay after failure.
    pub fn rotate_index(&self, current: usize) -> usize {
        if self.entries.len() <= 1 {
            return current;
        }
        for step in 1..=self.entries.len() {
            let idx = (current + step) % self.entries.len();
            if self.entries[idx].consecutive_failures < 3 {
                return idx;
            }
        }
        (current + 1) % self.entries.len()
    }

    pub fn mark_failure(&mut self, index: usize) {
        if let Some(e) = self.entry_mut(index) {
            e.consecutive_failures = e.consecutive_failures.saturating_add(1);
        }
    }

    pub fn mark_success(&mut self, index: usize) {
        if let Some(e) = self.entry_mut(index) {
            e.consecutive_failures = 0;
        }
    }

    /// Circuit multiaddr to reach `remote_peer_id` via relay at `relay_index`.
    pub fn circuit_multiaddr(&self, remote_peer_id: &str, relay_index: usize) -> Option<String> {
        let remote = remote_peer_id.trim();
        if remote.is_empty() {
            return None;
        }
        let entry = self.entry(relay_index)?;
        Some(format!("{}/p2p-circuit/p2p/{remote}", entry.tcp_multiaddr))
    }

    /// Load distribution when `node_count` clients each hash-pick a relay.
    pub fn load_distribution(node_count: usize, pool: &RelayPool) -> Vec<usize> {
        let n = pool.len().max(1);
        let mut counts = vec![0usize; n];
        for i in 0..node_count {
            let seed = format!("node-{i}");
            counts[pool.select_index_for(&seed)] += 1;
        }
        counts
    }

    /// Max share of nodes on any single relay (0.0–1.0).
    pub fn max_relay_share(node_count: usize, pool: &RelayPool) -> f64 {
        if node_count == 0 || pool.is_empty() {
            return 0.0;
        }
        let counts = Self::load_distribution(node_count, pool);
        let max = *counts.iter().max().unwrap_or(&0);
        max as f64 / node_count as f64
    }
}

fn relay_entry_from_registration(reg: &RelayRegistration) -> Option<RelayEntry> {
    let tcp = reg.primary_tcp_multiaddr()?.to_string();
    Some(RelayEntry {
        peer_id: reg.peer_id.clone(),
        tcp_multiaddr: tcp,
        operator_label: reg.operator_label.clone(),
        region: reg.region.clone(),
        operator_pubkey_hex: reg.operator_pubkey_hex.clone(),
        ipv4_prefix: reg.ipv4_prefix(),
        asn: reg.asn,
        consecutive_failures: 0,
    })
}

/// True when `peer_id` matches any relay in the built-in bootstrap pool.
pub fn is_relay_peer_in_pool(peer_id: &str, bootstrap_peers: &[impl AsRef<str>]) -> bool {
    RelayPool::from_bootstrap_peers(bootstrap_peers).is_relay_peer(peer_id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::relay_registry::RelayRegistryManifest;
    use libp2p::identity;

    fn relay_addr(ip: &str) -> String {
        let kp = identity::Keypair::generate_ed25519();
        format!("/ip4/{}/tcp/4001/p2p/{}", ip, kp.public().to_peer_id())
    }

    fn sample_pool() -> RelayPool {
        RelayPool::from_bootstrap_peers(&[
            relay_addr("10.0.0.1"),
            relay_addr("10.0.0.2"),
            relay_addr("10.0.0.3"),
        ])
    }

    #[test]
    fn pool_deduplicates_relays() {
        let pool = sample_pool();
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn circuit_multiaddr_uses_selected_relay() {
        let pool = sample_pool();
        let remote = "12D3KooWRemotePeerIdForTestOnly012345678901234";
        let addr = pool.circuit_multiaddr(remote, 1).expect("circuit");
        assert!(addr.contains("10.0.0.2"));
        assert!(addr.contains(remote));
    }

    #[test]
    fn festival_soak_500_nodes_no_relay_collapse() {
        let pool = sample_pool();
        let share = RelayPool::max_relay_share(500, &pool);
        assert!(
            share < 0.45,
            "single relay share {share} too high for 500-node soak"
        );
        let counts = RelayPool::load_distribution(500, &pool);
        assert!(
            counts.iter().all(|&c| c > 0),
            "every relay should get traffic"
        );
    }

    #[test]
    fn relay_failover_drill_rotates_index() {
        let pool = sample_pool();
        let start = pool.select_index_for("client-a");
        let after_fail = pool.rotate_index(start);
        assert_ne!(start, after_fail);
        let after_second = pool.rotate_index(after_fail);
        assert_ne!(after_fail, after_second);
    }

    #[test]
    fn hash_selection_is_stable() {
        let pool = sample_pool();
        assert_eq!(
            pool.select_index_for("peer-xyz"),
            pool.select_index_for("peer-xyz")
        );
    }

    #[test]
    fn federated_pool_from_default_manifest() {
        let manifest = RelayRegistryManifest::default_embedded();
        let pool = RelayPool::from_registry_manifest(&manifest, false);
        assert!(pool.len() >= 6, "multi-relay default manifest");
        assert!(pool.is_relay_peer(crate::standard_bootstrap::RELAY_PEER_ID));
    }

    #[test]
    fn failure_tracking_skips_unhealthy() {
        let mut pool = sample_pool();
        pool.mark_failure(0);
        pool.mark_failure(0);
        pool.mark_failure(0);
        let idx = pool.select_index_for_with_skip("seed", &[]);
        assert_ne!(idx, 0);
    }
}
