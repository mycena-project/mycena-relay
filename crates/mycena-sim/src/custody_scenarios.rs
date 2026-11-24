// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! City-scale DTN custody — delivery without sender/receiver simultaneous connectivity.

use mycena_core::data::{ContentId, Envelope};
use mycena_core::transport::DirectMessage;
use mycena_node::{CustodyConfig, CustodyQueue, EncounterLogStore};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tempfile::TempDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CityCustodyReport {
    pub delivered: bool,
    pub nodes: usize,
    pub steps: u64,
    /// Times sender and receiver had a direct radio link (same partition).
    pub sender_receiver_direct_links: u64,
    pub max_custody_spray_seen: u8,
    /// Max hop_count observed on any forward (propagation depth).
    pub max_hops_seen: u8,
    pub custody_holders_at_end: usize,
}

struct SimNode {
    id: String,
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    online: bool,
    encounter_log: EncounterLogStore,
    custody_queue: CustodyQueue,
    delivered: bool,
}

/// Mobility + encounter simulation using real [`CustodyQueue`] / [`EncounterLogStore`].
pub fn city_custody_delivery(seed: u64, nodes: usize) -> CityCustodyReport {
    let base = TempDir::new().expect("tempdir");
    run_city_custody_at(base.path(), seed, nodes)
}

fn run_city_custody_at(base: &Path, seed: u64, node_count: usize) -> CityCustodyReport {
    let n = node_count.clamp(50, 200);
    let sender = "node-sender".to_string();
    let receiver = "node-receiver".to_string();
    let mut rng = seed.max(1);
    let world = 1000.0f64;
    let radio = 90.0f64;
    let mut nodes: HashMap<String, SimNode> = HashMap::new();

    let sim_custody = CustodyConfig {
        flush_every_ms: None,
        ..CustodyConfig::default()
    };
    for i in 0..n {
        let id = if i == 0 {
            sender.clone()
        } else if i == 1 {
            receiver.clone()
        } else {
            format!("node-{i}")
        };
        let path = base.join(&id);
        std::fs::create_dir_all(&path).unwrap();
        let p = path.to_str().unwrap();
        let cols = (n as f64).sqrt().ceil() as usize;
        let row = i / cols.max(1);
        let col = i % cols.max(1);
        let grid_x = (col as f64 + rand_f64(&mut rng)) * world / cols.max(1) as f64;
        let grid_y = (row as f64 + rand_f64(&mut rng)) * world / cols.max(1) as f64;
        nodes.insert(
            id.clone(),
            SimNode {
                id: id.clone(),
                x: grid_x,
                y: grid_y,
                vx: (rand_f64(&mut rng) - 0.5) * 3.0,
                vy: (rand_f64(&mut rng) - 0.5) * 3.0,
                online: true,
                encounter_log: EncounterLogStore::open_sync(p).unwrap(),
                custody_queue: CustodyQueue::open(p, sim_custody.clone()).unwrap(),
                delivered: false,
            },
        );
    }

    // Start sender and receiver far apart — unlikely to meet directly.
    if let Some(s) = nodes.get_mut(&sender) {
        s.x = 50.0;
        s.y = 50.0;
    }
    if let Some(r) = nodes.get_mut(&receiver) {
        r.x = world - 50.0;
        r.y = world - 50.0;
    }

    let mut env = Envelope::new_custody(sender.clone(), receiver.clone(), b"city-msg".to_vec());
    env.id = ContentId(format!("city-custody-{seed}"));
    env.max_custody_spray = 10;
    env.max_hops = 255;
    let t0 = 1_700_000_000_000u64 + seed * 1_000;
    env.created_at_ms = t0;
    let msg = DirectMessage {
        envelope: env,
        body: "city-msg".into(),
    };
    nodes
        .get_mut(&sender)
        .unwrap()
        .custody_queue
        .enqueue(&msg)
        .unwrap();

    let mut active_links: HashSet<(String, String)> = HashSet::new();
    let mut direct_sr_links = 0u64;
    let mut max_hops = 0u8;
    let steps = 10000u64;
    let step_ms = 1000u64;

    let ids: Vec<String> = (0..n)
        .map(|i| {
            if i == 0 {
                sender.clone()
            } else if i == 1 {
                receiver.clone()
            } else {
                format!("node-{i}")
            }
        })
        .collect();
    let goal = CustodySimGoal {
        goal_x: world - 50.0,
        goal_y: world - 50.0,
    };

    for step in 0..steps {
        let now = t0 + step * step_ms;
        // Random walk + bounce (sorted ids for determinism)
        for id in &ids {
            let node = nodes.get_mut(id).unwrap();
            if !node.online || id == &sender || id == &receiver {
                continue;
            }
            node.x = (node.x + node.vx).clamp(0.0, world);
            node.y = (node.y + node.vy).clamp(0.0, world);
            if node.x <= 0.0 || node.x >= world {
                node.vx = -node.vx;
            }
            if node.y <= 0.0 || node.y >= world {
                node.vy = -node.vy;
            }
            // Churn (carriers only)
            if step % 120 == 0 && rand_u32(&mut rng) % 100 < 2 {
                node.online = !node.online;
            }
        }

        // Sender/receiver stay at fixed corners — never meet directly.
        if let Some(s) = nodes.get_mut(&sender) {
            s.x = 50.0;
            s.y = 50.0;
            s.online = true;
        }
        if let Some(r) = nodes.get_mut(&receiver) {
            r.x = world - 50.0;
            r.y = world - 50.0;
            r.online = true;
        }

        let mut links_now = HashSet::new();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let a = &ids[i];
                let b = &ids[j];
                let na = nodes.get(a).unwrap();
                let nb = nodes.get(b).unwrap();
                if !na.online || !nb.online {
                    continue;
                }
                let dx = na.x - nb.x;
                let dy = na.y - nb.y;
                if (dx * dx + dy * dy).sqrt() <= radio {
                    let key = link_key(a, b);
                    links_now.insert(key);
                    if (a == &sender && b == &receiver) || (a == &receiver && b == &sender) {
                        direct_sr_links += 1;
                    }
                }
            }
        }

        // New links — encounters + custody transfer (sorted for determinism)
        let mut new_links: Vec<_> = links_now
            .iter()
            .filter(|key| !active_links.contains(*key))
            .cloned()
            .collect();
        new_links.sort();
        for (a, b) in new_links {
            let _ = nodes
                .get(&a)
                .unwrap()
                .encounter_log
                .begin_encounter(&b, now);
            let _ = nodes
                .get(&b)
                .unwrap()
                .encounter_log
                .begin_encounter(&a, now);
            transfer_custody_between(
                &mut nodes,
                &a,
                &b,
                now,
                &sender,
                &receiver,
                goal,
                &mut max_hops,
            );
            transfer_custody_between(
                &mut nodes,
                &b,
                &a,
                now,
                &sender,
                &receiver,
                goal,
                &mut max_hops,
            );
        }
        // Broken links
        let mut broken_links: Vec<_> = active_links
            .iter()
            .filter(|key| !links_now.contains(*key))
            .cloned()
            .collect();
        broken_links.sort();
        for (a, b) in broken_links {
            let _ = nodes.get(&a).unwrap().encounter_log.end_encounter(&b, now);
            let _ = nodes.get(&b).unwrap().encounter_log.end_encounter(&a, now);
        }
        active_links = links_now.clone();

        // Periodic custody spray tick on each online node
        if step % 5 == 0 {
            for id in ids.iter() {
                let online_peers: Vec<String> = links_now
                    .iter()
                    .filter_map(|(a, b)| {
                        if a == id {
                            Some(b.clone())
                        } else if b == id {
                            Some(a.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if online_peers.is_empty() {
                    continue;
                }
                let mut peers = online_peers;
                peers.sort_by(|a, b| {
                    dist_to(nodes.get(a).unwrap(), world - 50.0, world - 50.0)
                        .partial_cmp(&dist_to(nodes.get(b).unwrap(), world - 50.0, world - 50.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.cmp(b))
                });
                let (node_id, plan_out) = {
                    let node = nodes.get_mut(id).unwrap();
                    let _ = node.custody_queue.prune_expired(now);
                    let plan = node.custody_queue.next_forward_plan_at(
                        &node.id,
                        &peers,
                        &node.encounter_log,
                        now,
                        None,
                    );
                    (node.id.clone(), plan)
                };
                if let Ok(Some(plan)) = plan_out {
                    let msg = plan.message.clone();
                    let to_peer = plan.to_peer.clone();
                    if !deliver_to_peer(&mut nodes, &node_id, &to_peer, plan.message, &mut max_hops)
                    {
                        let _ = nodes.get_mut(&node_id).unwrap().custody_queue.enqueue(&msg);
                    }
                }
            }
        }

        if nodes.get(&receiver).unwrap().delivered {
            return finish_report(&nodes, true, n, step + 1, direct_sr_links, max_hops);
        }
    }

    finish_report(
        &nodes,
        nodes.get(&receiver).unwrap().delivered,
        n,
        steps,
        direct_sr_links,
        max_hops,
    )
}

fn custody_holders(nodes: &HashMap<String, SimNode>) -> usize {
    nodes
        .values()
        .filter(|n| n.custody_queue.len().unwrap_or(0) > 0)
        .count()
}

fn finish_report(
    nodes: &HashMap<String, SimNode>,
    delivered: bool,
    n: usize,
    steps: u64,
    direct_sr_links: u64,
    max_hops: u8,
) -> CityCustodyReport {
    CityCustodyReport {
        delivered,
        nodes: n,
        steps,
        sender_receiver_direct_links: direct_sr_links,
        max_custody_spray_seen: 0,
        max_hops_seen: max_hops,
        custody_holders_at_end: custody_holders(nodes),
    }
}

fn deliver_to_peer(
    nodes: &mut HashMap<String, SimNode>,
    from: &str,
    to: &str,
    message: DirectMessage,
    max_hops: &mut u8,
) -> bool {
    *max_hops = (*max_hops).max(message.envelope.hop_count);
    if message
        .envelope
        .to_peer
        .as_deref()
        .is_some_and(|dest| dest == to)
    {
        let node = nodes.get_mut(to).unwrap();
        node.delivered = true;
        let _ = node
            .custody_queue
            .mark_delivered_to_local(&message.envelope.id.0);
        return true;
    }
    if to == message.envelope.from_peer {
        return false;
    }
    if CustodyQueue::eligible_for_custody(&message) {
        if nodes
            .get(to)
            .unwrap()
            .custody_queue
            .enqueue(&message)
            .unwrap_or(false)
        {
            return true;
        }
        return false;
    }
    let _ = from;
    false
}

#[derive(Clone, Copy)]
struct CustodySimGoal {
    goal_x: f64,
    goal_y: f64,
}

#[allow(clippy::too_many_arguments)]
fn transfer_custody_between(
    nodes: &mut HashMap<String, SimNode>,
    from: &str,
    to: &str,
    now: u64,
    sender: &str,
    receiver: &str,
    goal: CustodySimGoal,
    max_hops: &mut u8,
) {
    if !should_handoff(nodes, from, to, sender, receiver, goal.goal_x, goal.goal_y) {
        return;
    }
    let plans = nodes
        .get(from)
        .unwrap()
        .custody_queue
        .plans_for_encounter_at(from, to, 4, now)
        .unwrap_or_default();
    for plan in plans {
        let msg = plan.message.clone();
        if deliver_to_peer(nodes, from, &plan.to_peer, plan.message, max_hops) {
            continue;
        }
        let _ = nodes.get_mut(from).unwrap().custody_queue.enqueue(&msg);
    }
}

fn should_handoff(
    nodes: &HashMap<String, SimNode>,
    from: &str,
    to: &str,
    sender: &str,
    receiver: &str,
    goal_x: f64,
    goal_y: f64,
) -> bool {
    if to == receiver || from == sender {
        return true;
    }
    let from_d = dist_to(nodes.get(from).unwrap(), goal_x, goal_y);
    let to_d = dist_to(nodes.get(to).unwrap(), goal_x, goal_y);
    to_d + 10.0 <= from_d
}

fn dist_to(node: &SimNode, x: f64, y: f64) -> f64 {
    let dx = node.x - x;
    let dy = node.y - y;
    (dx * dx + dy * dy).sqrt()
}

fn link_key(a: &str, b: &str) -> (String, String) {
    if a < b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

fn rand_f64(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*state >> 11) as f64 / (u64::MAX >> 11) as f64
}

fn rand_u32(state: &mut u64) -> u32 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*state >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_custody_sim_is_deterministic_for_seed() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        let r1 = run_city_custody_at(a.path(), 1, 80);
        let r2 = run_city_custody_at(b.path(), 1, 80);
        assert_eq!(r1.delivered, r2.delivered, "delivered mismatch");
        assert_eq!(r1.steps, r2.steps, "steps mismatch");
    }

    #[test]
    fn city_custody_delivers_without_sender_receiver_overlap() {
        let report = city_custody_delivery(1, 80);
        assert!(
            report.delivered,
            "message should reach receiver via carriers (steps={} holders={})",
            report.steps, report.custody_holders_at_end
        );
        assert_eq!(
            report.sender_receiver_direct_links, 0,
            "sender and receiver should never share a direct link (carriers only)"
        );
    }

    #[test]
    fn city_custody_scales_to_120_nodes() {
        let report = city_custody_delivery(8, 120);
        assert!(report.nodes >= 50);
        assert!(
            report.delivered,
            "120-node city sim should deliver within TTL"
        );
    }

    #[test]
    fn city_custody_scales_to_200_nodes() {
        let report = city_custody_delivery(42, 200);
        assert_eq!(report.nodes, 200);
        assert!(
            report.delivered,
            "200-node city sim should deliver within TTL (steps={})",
            report.steps
        );
    }
}
