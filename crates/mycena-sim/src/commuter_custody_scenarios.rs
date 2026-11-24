// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Commuter DTN custody — fixed daily routines vs recency-only routing.

use mycena_core::data::{ContentId, Envelope};
use mycena_core::transport::DirectMessage;
use mycena_node::{
    CustodyCandidateScoring, CustodyConfig, CustodyQueue, CustodyRoutingContext, EncounterLogStore,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tempfile::TempDir;

const HOME_X: f64 = 100.0;
const HOME_Y: f64 = 100.0;
const HUB_X: f64 = 850.0;
const HUB_Y: f64 = 900.0;
const RADIO: f64 = 55.0;
const STEPS_PER_DAY: u64 = 144;
const STEP_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommuterCustodyComparison {
    pub delivery_probability_delivered: bool,
    pub recency_only_delivered: bool,
    pub steps_probability: u64,
    pub steps_recency: u64,
    pub latency_probability_ms: u64,
    pub latency_recency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommuterCustodyRun {
    pub delivered: bool,
    pub steps: u64,
    pub latency_ms: u64,
    pub carrier_picked: Option<String>,
}

struct SimNode {
    id: String,
    x: f64,
    y: f64,
    encounter_log: EncounterLogStore,
    custody_queue: CustodyQueue,
    delivered: bool,
}

/// Run the same commuter scenario twice: delivery-probability routing vs recency-only.
pub fn commuter_custody_comparison(seed: u64) -> CommuterCustodyComparison {
    let base = TempDir::new().expect("tempdir");
    let prob = run_commuter_scenario(
        &base.path().join("prob"),
        seed,
        CustodyCandidateScoring::DeliveryProbabilityAndRecency,
    );
    let recency = run_commuter_scenario(
        &base.path().join("recency"),
        seed,
        CustodyCandidateScoring::RecencyOnly,
    );
    CommuterCustodyComparison {
        delivery_probability_delivered: prob.delivered,
        recency_only_delivered: recency.delivered,
        steps_probability: prob.steps,
        steps_recency: recency.steps,
        latency_probability_ms: prob.latency_ms,
        latency_recency_ms: recency.latency_ms,
    }
}

fn run_commuter_scenario(
    base: &Path,
    seed: u64,
    scoring: CustodyCandidateScoring,
) -> CommuterCustodyRun {
    std::fs::create_dir_all(base).expect("mkdir");
    let sender = "sender".to_string();
    let receiver = "receiver".to_string();
    let commuter = "commuter".to_string();
    let wanderer = "wanderer".to_string();
    let ids = [
        sender.clone(),
        receiver.clone(),
        commuter.clone(),
        wanderer.clone(),
    ];

    let sim_custody = CustodyConfig {
        flush_every_ms: None,
        ..CustodyConfig::default()
    };

    let mut nodes: HashMap<String, SimNode> = HashMap::new();
    for id in &ids {
        let path = base.join(id);
        std::fs::create_dir_all(&path).expect("node dir");
        let p = path.to_str().expect("utf8 path");
        nodes.insert(
            id.clone(),
            SimNode {
                id: id.clone(),
                x: HOME_X,
                y: HOME_Y,
                encounter_log: EncounterLogStore::open_sync(p).expect("encounter log"),
                custody_queue: CustodyQueue::open(p, sim_custody.clone()).expect("custody"),
                delivered: false,
            },
        );
    }

    let t0 = 1_700_000_000_000u64 + seed * 1_000;
    let warmup_days = 4u64;
    let test_day = warmup_days;
    let total_days = test_day + 1;
    let max_steps = total_days * STEPS_PER_DAY + 200;
    let mut active_links: HashSet<(String, String)> = HashSet::new();
    let mut enqueued_at: Option<u64> = None;
    let mut carrier_picked: Option<String> = None;

    let routing = CustodyRoutingContext {
        candidate_scoring: scoring,
        ..CustodyRoutingContext::default()
    };

    for step in 0..max_steps {
        let day = step / STEPS_PER_DAY;
        let day_step = step % STEPS_PER_DAY;
        let now = t0 + step * STEP_MS;
        place_nodes(&mut nodes, day, day_step, test_day);

        let mut links_now = HashSet::new();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let a = &ids[i];
                let b = &ids[j];
                if in_range(nodes.get(a).expect("node"), nodes.get(b).expect("node")) {
                    links_now.insert(link_key(a, b));
                }
            }
        }

        let mut new_links: Vec<_> = links_now
            .iter()
            .filter(|k| !active_links.contains(*k))
            .cloned()
            .collect();
        new_links.sort();
        for (a, b) in &new_links {
            let _ = nodes
                .get(a)
                .expect("node")
                .encounter_log
                .begin_encounter(b, now);
            let _ = nodes
                .get(b)
                .expect("node")
                .encounter_log
                .begin_encounter(a, now);
        }

        for id in &ids {
            let peers: Vec<String> = links_now
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
            if peers.len() >= 2 {
                let _ = nodes
                    .get(id)
                    .expect("node")
                    .encounter_log
                    .observe_co_presence_among(&peers, now);
            }
            transfer_on_link(&mut nodes, id, &peers, now, &receiver);
        }

        let mut broken_links: Vec<_> = active_links
            .iter()
            .filter(|k| !links_now.contains(*k))
            .cloned()
            .collect();
        broken_links.sort();
        for (a, b) in broken_links {
            let _ = nodes
                .get(&a)
                .expect("node")
                .encounter_log
                .end_encounter(&b, now);
            let _ = nodes
                .get(&b)
                .expect("node")
                .encounter_log
                .end_encounter(&a, now);
        }
        active_links = links_now;

        if day == test_day && day_step == 38 && enqueued_at.is_none() {
            let mut env =
                Envelope::new_custody(sender.clone(), receiver.clone(), b"commute-msg".to_vec());
            env.id = ContentId(format!("commute-{seed}-{scoring:?}"));
            env.max_custody_spray = 3;
            env.max_hops = 64;
            env.created_at_ms = now;
            let msg = DirectMessage {
                envelope: env,
                body: "commute-msg".into(),
            };
            nodes
                .get_mut(&sender)
                .expect("sender")
                .custody_queue
                .enqueue(&msg)
                .expect("enqueue");
            enqueued_at = Some(now);
        }

        if enqueued_at.is_some() && step % 2 == 0 {
            for id in &ids {
                let peers: Vec<String> = active_links
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
                if peers.is_empty() {
                    continue;
                }
                let (node_id, plan_out) = {
                    let node = nodes.get_mut(id).expect("node");
                    let _ = node.custody_queue.prune_expired(now);
                    let plan = node.custody_queue.next_forward_plan_at(
                        &node.id,
                        &peers,
                        &node.encounter_log,
                        now,
                        Some(&routing),
                    );
                    (node.id.clone(), plan)
                };
                if let Ok(Some(plan)) = plan_out {
                    if node_id == sender && carrier_picked.is_none() {
                        carrier_picked = Some(plan.to_peer.clone());
                    }
                    let msg = plan.message.clone();
                    if !deliver_to_peer(&mut nodes, &node_id, &plan.to_peer, plan.message) {
                        let _ = nodes
                            .get_mut(&node_id)
                            .expect("node")
                            .custody_queue
                            .enqueue(&msg);
                    }
                }
            }
        }

        if nodes.get(&receiver).expect("receiver").delivered {
            let latency = now.saturating_sub(enqueued_at.unwrap_or(now));
            return CommuterCustodyRun {
                delivered: true,
                steps: step + 1,
                latency_ms: latency,
                carrier_picked,
            };
        }
    }

    CommuterCustodyRun {
        delivered: nodes.get(&receiver).expect("receiver").delivered,
        steps: max_steps,
        latency_ms: enqueued_at.map(|t| max_steps * STEP_MS + t).unwrap_or(0),
        carrier_picked,
    }
}

fn place_nodes(nodes: &mut HashMap<String, SimNode>, day: u64, day_step: u64, test_day: u64) {
    if day < test_day {
        commuter_position(nodes.get_mut("commuter").expect("commuter"), day_step);
        commuter_position(nodes.get_mut("sender").expect("sender"), day_step);
        let receiver = nodes.get_mut("receiver").expect("receiver");
        receiver.x = HUB_X;
        receiver.y = HUB_Y;
        let wanderer = nodes.get_mut("wanderer").expect("wanderer");
        wanderer.x = HOME_X + 5.0;
        wanderer.y = HOME_Y + 5.0;
        return;
    }

    {
        let sender = nodes.get_mut("sender").expect("sender");
        sender.x = HOME_X;
        sender.y = HOME_Y;
    }

    {
        let receiver = nodes.get_mut("receiver").expect("receiver");
        receiver.x = HUB_X;
        receiver.y = HUB_Y;
    }

    commuter_position(nodes.get_mut("commuter").expect("commuter"), day_step);

    let wanderer = nodes.get_mut("wanderer").expect("wanderer");
    if (30..=45).contains(&day_step) {
        wanderer.x = HOME_X + 2.0;
        wanderer.y = HOME_Y + 2.0;
    } else {
        wanderer.x = HOME_X + 40.0;
        wanderer.y = HOME_Y + 40.0;
    }
}

fn commuter_position(node: &mut SimNode, day_step: u64) {
    if day_step < 35 {
        node.x = HOME_X;
        node.y = HOME_Y;
    } else if day_step < 55 {
        let t = (day_step - 35) as f64 / 20.0;
        node.x = HOME_X + t * (HUB_X - HOME_X);
        node.y = HOME_Y + t * (HUB_Y - HOME_Y);
    } else if day_step < 85 {
        node.x = HUB_X;
        node.y = HUB_Y;
    } else if day_step < 105 {
        let t = (day_step - 85) as f64 / 20.0;
        node.x = HUB_X + t * (HOME_X - HUB_X);
        node.y = HUB_Y + t * (HOME_Y - HUB_Y);
    } else {
        node.x = HOME_X;
        node.y = HOME_Y;
    }
}

fn in_range(a: &SimNode, b: &SimNode) -> bool {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt() <= RADIO
}

fn link_key(a: &str, b: &str) -> (String, String) {
    if a < b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

fn transfer_on_link(
    nodes: &mut HashMap<String, SimNode>,
    from: &str,
    peers: &[String],
    now: u64,
    receiver: &str,
) {
    for to in peers {
        let plans = nodes
            .get(from)
            .expect("node")
            .custody_queue
            .plans_for_encounter_at(from, to, 2, now)
            .unwrap_or_default();
        for plan in plans {
            let msg = plan.message.clone();
            if deliver_to_peer(nodes, from, &plan.to_peer, plan.message) {
                continue;
            }
            let _ = nodes
                .get_mut(from)
                .expect("node")
                .custody_queue
                .enqueue(&msg);
        }
        let _ = receiver;
    }
}

fn deliver_to_peer(
    nodes: &mut HashMap<String, SimNode>,
    _from: &str,
    to: &str,
    message: DirectMessage,
) -> bool {
    if message
        .envelope
        .to_peer
        .as_deref()
        .is_some_and(|dest| dest == to)
    {
        let node = nodes.get_mut(to).expect("node");
        node.delivered = true;
        let _ = node
            .custody_queue
            .mark_delivered_to_local(&message.envelope.id.0);
        return true;
    }
    if CustodyQueue::eligible_for_custody(&message) {
        return nodes
            .get(to)
            .expect("node")
            .custody_queue
            .enqueue(&message)
            .unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commuter_delivery_probability_beats_recency_only() {
        let cmp = commuter_custody_comparison(7);
        assert!(
            cmp.delivery_probability_delivered,
            "delivery-probability routing should deliver (steps={})",
            cmp.steps_probability
        );
        assert!(
            cmp.steps_probability <= cmp.steps_recency,
            "probability routing should be faster or equal: prob={} recency={}",
            cmp.steps_probability,
            cmp.steps_recency
        );
        assert!(
            cmp.latency_probability_ms <= cmp.latency_recency_ms,
            "probability routing latency should be lower or equal: prob={}ms recency={}ms",
            cmp.latency_probability_ms,
            cmp.latency_recency_ms
        );
    }

    #[test]
    fn commuter_scenario_is_deterministic() {
        let base = TempDir::new().unwrap();
        let a = run_commuter_scenario(
            &base.path().join("a"),
            3,
            CustodyCandidateScoring::DeliveryProbabilityAndRecency,
        );
        let b = run_commuter_scenario(
            &base.path().join("b"),
            3,
            CustodyCandidateScoring::DeliveryProbabilityAndRecency,
        );
        assert_eq!(a.delivered, b.delivered);
        assert_eq!(a.steps, b.steps);
        assert_eq!(a.carrier_picked, b.carrier_picked);
    }
}
