// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Dense-cluster scaling scenarios (AR-06): geohash gossip shards + crowd density.

use mycena_core::crowd_density::CrowdDensityConfig;
use mycena_core::sos::SOS_SCOPE;
use mycena_core::topic_shard;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct DenseClusterReport {
    pub nodes: usize,
    pub shard_count: usize,
    pub global_gossip_fanout: u64,
    pub sharded_gossip_fanout: u64,
    pub density_backoff_triggered: bool,
    pub shard_containment_ratio: f64,
}

/// Model 500+ peers in one dense cluster; compare global vs geohash-sharded gossip fanout.
pub fn dense_cluster_shard_containment(node_count: usize) -> DenseClusterReport {
    let nodes = node_count.max(500);
    let shard_count = 8usize;
    let mut shard_members: HashMap<String, Vec<usize>> = HashMap::new();
    let prefixes = [
        "u4pr", "u4ps", "u4pt", "u4pu", "u4pv", "u4pw", "u4px", "u4py",
    ];
    for i in 0..nodes {
        let prefix = prefixes[i % shard_count];
        shard_members.entry(prefix.to_string()).or_default().push(i);
    }

    let global_gossip_fanout = (nodes.saturating_sub(1)) as u64;
    let largest_shard = shard_members.values().map(|v| v.len()).max().unwrap_or(1);
    let sharded_gossip_fanout = largest_shard.saturating_sub(1).max(1) as u64;

    let density_cfg = CrowdDensityConfig::default();
    let density_backoff_triggered = density_cfg.should_activate(nodes.saturating_sub(1), false);

    let shard_containment_ratio = sharded_gossip_fanout as f64 / global_gossip_fanout.max(1) as f64;

    DenseClusterReport {
        nodes,
        shard_count,
        global_gossip_fanout,
        sharded_gossip_fanout,
        density_backoff_triggered,
        shard_containment_ratio,
    }
}

/// Assign each peer a geohash shard and count who would receive a scoped publish.
pub fn count_sharded_recipients(
    subscribers: &[(String, HashSet<String>)],
    publish_scope: &str,
) -> usize {
    subscribers
        .iter()
        .filter(|(_, subs)| subs.contains(publish_scope))
        .count()
}

/// Simulate SOS publish in a dense cluster with geohash sharding.
pub fn dense_sos_publish_recipients(nodes: usize, publisher_geohash: &str) -> (usize, usize) {
    let nodes = nodes.max(500);
    let topic = topic_shard::gossip_topic(SOS_SCOPE, publisher_geohash).expect("shard topic");
    let prefixes = [
        "u4pr", "u4ps", "u4pt", "u4pu", "u4pv", "u4pw", "u4px", "u4py",
    ];

    let mut subscribers = Vec::with_capacity(nodes);
    for i in 0..nodes {
        let mut subs = HashSet::new();
        let local_prefix = prefixes[i % prefixes.len()];
        if let Some(t) = topic_shard::gossip_topic(SOS_SCOPE, local_prefix) {
            subs.insert(t);
        }
        subscribers.push((format!("n{i}"), subs));
    }

    let sharded = count_sharded_recipients(
        &subscribers
            .iter()
            .map(|(id, s)| (id.clone(), s.clone()))
            .collect::<Vec<_>>(),
        &topic,
    );
    let global = nodes.saturating_sub(1);
    (sharded, global)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_cluster_520_nodes_shard_containment() {
        let report = dense_cluster_shard_containment(520);
        assert_eq!(report.nodes, 520);
        assert!(report.density_backoff_triggered);
        assert!(
            report.sharded_gossip_fanout < report.global_gossip_fanout / 4,
            "sharded fanout {} should be << global {}",
            report.sharded_gossip_fanout,
            report.global_gossip_fanout
        );
        assert!(report.shard_containment_ratio < 0.15);
    }

    #[test]
    fn sos_sharded_publish_reaches_shard_only() {
        let (sharded, global) = dense_sos_publish_recipients(520, "u4pruyd");
        assert_eq!(global, 519);
        assert!(sharded < 80, "expected ~65 shard peers, got {sharded}");
        assert!(sharded < global / 4);
    }
}
