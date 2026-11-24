// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Client-side relay dial diversity — never contact a parallel batch exclusively
//! from one IPv4 /24 or ASN; reserve quota for minority operators.

use crate::relay_pool::RelayEntry;
use std::collections::{HashMap, HashSet};

/// Default parallel bootstrap dials per connection attempt.
pub const DEFAULT_PARALLEL_BOOTSTRAP_DIALS: usize = 3;

/// Tunables for infrastructure diversity during parallel relay dials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayDiversityPolicy {
    /// Target number of relays to dial in one diverse batch.
    pub parallel_dial_count: usize,
    /// Max relays sharing the same IPv4 /24 prefix in one batch.
    pub max_same_ipv4_prefix: usize,
    /// Max relays sharing the same ASN in one batch.
    pub max_same_asn: usize,
    /// Max relays from the same operator identity in one batch.
    pub max_same_operator_parallel: usize,
}

impl Default for RelayDiversityPolicy {
    fn default() -> Self {
        Self {
            parallel_dial_count: DEFAULT_PARALLEL_BOOTSTRAP_DIALS,
            max_same_ipv4_prefix: 1,
            max_same_asn: 1,
            max_same_operator_parallel: 2,
        }
    }
}

/// Select relay indices for a diverse parallel dial batch.
///
/// Guarantees (when the pool allows):
/// - Never **exclusively** one IPv4 /24 or one ASN when alternatives exist.
/// - At least one slot for minority operators (bottom share of the pool).
pub fn select_parallel_dial_indices(
    entries: &[RelayEntry],
    seed: &str,
    policy: &RelayDiversityPolicy,
) -> Vec<usize> {
    let n = policy.parallel_dial_count.min(entries.len());
    if n == 0 {
        return Vec::new();
    }
    if entries.len() <= n {
        return (0..entries.len()).collect();
    }

    let healthy: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.consecutive_failures < 3)
        .map(|(i, _)| i)
        .collect();
    let candidates = if healthy.len() >= n {
        healthy
    } else {
        (0..entries.len()).collect()
    };

    let minority = minority_operator_indices(entries, &candidates);
    let minority_quota = minority_dial_quota(entries.len(), minority.len(), n);

    let mut selected = Vec::with_capacity(n);
    let mut used = HashSet::new();

    let minority_order = hash_ordered_indices(seed, &minority, 0);
    for idx in minority_order {
        if selected.len() >= minority_quota || selected.len() >= n {
            break;
        }
        if used.insert(idx) && candidate_fits(&selected, idx, entries, policy) {
            selected.push(idx);
        }
    }

    let fill_order = hash_ordered_indices(seed, &candidates, 0);
    for idx in fill_order {
        if selected.len() >= n {
            break;
        }
        if used.contains(&idx) {
            continue;
        }
        if candidate_fits(&selected, idx, entries, policy) {
            used.insert(idx);
            selected.push(idx);
        }
    }

    if selected.len() < n {
        let relaxed = RelayDiversityPolicy {
            max_same_ipv4_prefix: n,
            max_same_asn: n,
            ..*policy
        };
        for idx in hash_ordered_indices(seed, &candidates, 1) {
            if selected.len() >= n {
                break;
            }
            if used.contains(&idx) {
                continue;
            }
            if candidate_fits(&selected, idx, entries, &relaxed) {
                used.insert(idx);
                selected.push(idx);
            }
        }
    }

    if selected.len() < n {
        for idx in hash_ordered_indices(seed, &candidates, 2) {
            if selected.len() >= n {
                break;
            }
            if used.insert(idx) {
                selected.push(idx);
            }
        }
    }

    enforce_non_exclusive_infrastructure(&mut selected, entries, &candidates, seed, policy);
    selected
}

/// Probability that a diverse parallel batch includes ≥1 relay matching `is_independent`.
pub fn independent_connection_probability(
    entries: &[RelayEntry],
    is_independent: impl Fn(&RelayEntry) -> bool,
    attempts: usize,
    policy: &RelayDiversityPolicy,
) -> f64 {
    if attempts == 0 || entries.is_empty() {
        return 0.0;
    }
    let mut hits = 0usize;
    for i in 0..attempts {
        let seed = format!("sim-{i}");
        let picked = select_parallel_dial_indices(entries, &seed, policy);
        if picked.iter().any(|&idx| is_independent(&entries[idx])) {
            hits += 1;
        }
    }
    hits as f64 / attempts as f64
}

fn minority_dial_quota(pool_len: usize, minority_len: usize, dial_n: usize) -> usize {
    if minority_len == 0 || pool_len == 0 {
        return 0;
    }
    let quota = dial_n.saturating_mul(minority_len).div_ceil(pool_len);
    quota.max(1).min(dial_n)
}

fn minority_operator_indices(entries: &[RelayEntry], candidates: &[usize]) -> Vec<usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &idx in candidates {
        *counts
            .entry(entries[idx].operator_pubkey_hex.as_str())
            .or_insert(0) += 1;
    }
    if counts.len() <= 1 {
        return Vec::new();
    }
    let max_share = *counts.values().max().unwrap_or(&0);
    candidates
        .iter()
        .copied()
        .filter(|&idx| {
            counts
                .get(entries[idx].operator_pubkey_hex.as_str())
                .is_some_and(|&c| c < max_share)
        })
        .collect()
}

fn candidate_fits(
    selected: &[usize],
    candidate: usize,
    entries: &[RelayEntry],
    policy: &RelayDiversityPolicy,
) -> bool {
    let mut trial = selected.to_vec();
    trial.push(candidate);
    batch_infrastructure_ok(&trial, entries, policy)
}

fn batch_infrastructure_ok(
    selected: &[usize],
    entries: &[RelayEntry],
    policy: &RelayDiversityPolicy,
) -> bool {
    if selected.is_empty() {
        return true;
    }
    let prefixes: HashSet<_> = selected
        .iter()
        .filter_map(|&i| entries[i].ipv4_prefix.clone())
        .collect();
    let asns: HashSet<_> = selected.iter().map(|&i| entries[i].asn).collect();
    let pool_prefixes: HashSet<_> = entries
        .iter()
        .filter_map(|e| e.ipv4_prefix.clone())
        .collect();
    let pool_asns: HashSet<_> = entries.iter().map(|e| e.asn).collect();

    if selected.len() > 1 && pool_prefixes.len() > 1 && prefixes.len() == 1 {
        return false;
    }
    if selected.len() > 1 && pool_asns.len() > 1 && asns.len() == 1 {
        return false;
    }

    let mut prefix_counts: HashMap<&str, usize> = HashMap::new();
    let mut asn_counts: HashMap<u32, usize> = HashMap::new();
    let mut operator_counts: HashMap<&str, usize> = HashMap::new();
    for &idx in selected {
        if let Some(ref p) = entries[idx].ipv4_prefix {
            *prefix_counts.entry(p.as_str()).or_insert(0) += 1;
        }
        *asn_counts.entry(entries[idx].asn).or_insert(0) += 1;
        *operator_counts
            .entry(entries[idx].operator_pubkey_hex.as_str())
            .or_insert(0) += 1;
    }
    if prefix_counts
        .values()
        .any(|&c| c > policy.max_same_ipv4_prefix)
    {
        return false;
    }
    if asn_counts.values().any(|&c| c > policy.max_same_asn) {
        return false;
    }
    if operator_counts
        .values()
        .any(|&c| c > policy.max_same_operator_parallel)
    {
        return false;
    }
    true
}

fn enforce_non_exclusive_infrastructure(
    selected: &mut Vec<usize>,
    entries: &[RelayEntry],
    candidates: &[usize],
    seed: &str,
    policy: &RelayDiversityPolicy,
) {
    if selected.len() <= 1 {
        return;
    }
    let pool_prefixes: HashSet<_> = entries
        .iter()
        .filter_map(|e| e.ipv4_prefix.clone())
        .collect();
    let pool_asns: HashSet<_> = entries.iter().map(|e| e.asn).collect();
    if pool_prefixes.len() <= 1 && pool_asns.len() <= 1 {
        return;
    }

    let all_same_prefix = selected
        .iter()
        .filter_map(|&i| entries[i].ipv4_prefix.clone())
        .collect::<HashSet<_>>()
        .len()
        <= 1;
    let all_same_asn = selected
        .iter()
        .map(|&i| entries[i].asn)
        .collect::<HashSet<_>>()
        .len()
        <= 1;

    if (!all_same_prefix || pool_prefixes.len() <= 1) && (!all_same_asn || pool_asns.len() <= 1) {
        return;
    }

    for alt in hash_ordered_indices(seed, candidates, 3) {
        if selected.contains(&alt) {
            continue;
        }
        for pos in 0..selected.len() {
            let mut trial = selected.clone();
            trial[pos] = alt;
            if batch_infrastructure_ok(&trial, entries, policy) {
                *selected = trial;
                return;
            }
        }
    }
}

fn hash_ordered_indices(seed: &str, candidates: &[usize], salt: u8) -> Vec<usize> {
    let mut keyed: Vec<(u64, usize)> = candidates
        .iter()
        .map(|&idx| {
            let mut input = Vec::new();
            input.extend_from_slice(seed.as_bytes());
            input.push(0);
            input.push(salt);
            input.extend_from_slice(&(idx as u64).to_le_bytes());
            let h = blake3::hash(&input);
            let b = h.as_bytes();
            let score = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            (score, idx)
        })
        .collect();
    keyed.sort_by_key(|(score, _)| *score);
    keyed.into_iter().map(|(_, idx)| idx).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::relay_pool::RelayEntry;

    fn entry(ip: &str, asn: u32, operator: &str) -> RelayEntry {
        RelayEntry {
            peer_id: format!("peer-{ip}-{operator}"),
            tcp_multiaddr: format!("/ip4/{ip}/tcp/4001/p2p/peer"),
            operator_label: operator.into(),
            region: "test".into(),
            operator_pubkey_hex: operator.into(),
            ipv4_prefix: crate::relay_registry::ipv4_prefix_from_multiaddr(&format!(
                "/ip4/{ip}/tcp/4001"
            )),
            asn,
            consecutive_failures: 0,
        }
    }

    #[test]
    fn parallel_batch_never_single_prefix_when_alternatives_exist() {
        let entries = vec![
            entry("10.0.0.1", 64500, "op-a"),
            entry("10.0.0.2", 64500, "op-a"),
            entry("10.1.0.1", 64501, "op-b"),
        ];
        for i in 0..200 {
            let picked = select_parallel_dial_indices(
                &entries,
                &format!("seed-{i}"),
                &RelayDiversityPolicy {
                    parallel_dial_count: 2,
                    ..Default::default()
                },
            );
            assert_eq!(picked.len(), 2);
            let prefixes: HashSet<_> = picked
                .iter()
                .filter_map(|&idx| entries[idx].ipv4_prefix.clone())
                .collect();
            assert!(prefixes.len() > 1, "must not dial only one /24: {picked:?}");
        }
    }

    #[test]
    fn adversary_seventy_percent_independent_probability_floor() {
        let mut entries = Vec::new();
        for i in 0..7 {
            let ip = format!("20.0.{}.10", i + 1);
            entries.push(entry(&ip, 20_000 + i as u32, "adversary-bloc"));
        }
        for i in 0..3 {
            let ip = format!("30.0.{}.10", i + 1);
            entries.push(entry(&ip, 30_000 + i as u32, &format!("independent-{i}")));
        }
        let independent_ops: HashSet<String> = (0..3).map(|i| format!("independent-{i}")).collect();
        let prob = independent_connection_probability(
            &entries,
            |e| independent_ops.contains(&e.operator_pubkey_hex),
            5_000,
            &RelayDiversityPolicy::default(),
        );
        assert!(
            prob >= 0.95,
            "70% adversary pool must still reach independent relays: {prob:.3}"
        );
    }
}
