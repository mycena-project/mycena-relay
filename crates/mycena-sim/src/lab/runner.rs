// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! MycenaLab spatial simulation orchestrator.

use crate::lab::battery::{BatteryTotals, NodeBattery};
use crate::lab::mobility::{MobilityModel, Vec2};
use crate::lab::radio::{BearerKind, RadioConfig};
use crate::lab::report::LabReport;
use crate::lab::rng::SimRng;
use crate::lab::transport_score::{bearer_for_link_scored, record_lab_transfer, LabBearerQuery};
use crate::runner::SimulationRunner;
use crate::scheduler::LinkProfile;
use crate::transport::SimAction;
use mycena_core::data::Envelope;
use mycena_core::transport::{DirectMessage, WireMessage};
use mycena_core::transport_score::TransportScoreStore;
use std::collections::{HashMap, HashSet};

/// Typical lab direct-message size for bearer scoring (matches chat-scale frames).
const LAB_LINK_PAYLOAD_BYTES: usize = 2048;

#[derive(Debug, Clone)]
pub enum LabEvent {
    InternetOutage { at_ms: u64 },
    RemoveNodesFraction { at_ms: u64, fraction: f64 },
    NetworkSplit { at_ms: u64 },
    NetworkMerge { at_ms: u64 },
}

#[derive(Debug, Clone)]
pub enum MobilityPreset {
    LevyFlight,
    RandomWaypoint { speed_m_per_s: f64 },
    StaticCluster { jitter_m: f64, cluster_count: usize },
}

#[derive(Debug, Clone)]
pub struct PresetConfig {
    pub name: String,
    pub seed: u64,
    pub node_count: usize,
    pub world_size_m: f64,
    pub duration_ms: u64,
    pub tick_ms: u64,
    pub mobility: MobilityPreset,
    pub radio: RadioConfig,
    pub malicious_fraction: f64,
    pub malicious_drop_per_mille: u16,
    pub churn_per_hour: f64,
    pub messages_per_node: u32,
    pub bandwidth_bytes_per_sec: u64,
    pub events: Vec<LabEvent>,
    pub split_cluster: bool,
    /// CI / regression: zero loss, minimal latency.
    pub ideal_links: bool,
}

struct LabNode {
    id: String,
    position: Vec2,
    mobility: MobilityModel,
    online: bool,
    has_internet: bool,
    cluster: u8,
    #[allow(dead_code)]
    malicious: bool,
    battery: NodeBattery,
    bearers_in_use: Vec<BearerKind>,
}

pub struct LabRunner {
    config: PresetConfig,
    sim: SimulationRunner,
    rng: SimRng,
    nodes: Vec<LabNode>,
    attempted: u64,
    battery: BatteryTotals,
    network_split_active: bool,
    split_applied: bool,
    pending_events: Vec<LabEvent>,
    static_centers: Vec<Vec2>,
    transport_scores: TransportScoreStore,
}

impl LabRunner {
    pub fn new(config: PresetConfig) -> Self {
        let mut pending_events = config.events.clone();
        pending_events.sort_by_key(event_time_ms);
        Self {
            sim: SimulationRunner::new(0, config.seed),
            rng: SimRng::new(config.seed),
            nodes: Vec::new(),
            attempted: 0,
            battery: BatteryTotals::default(),
            network_split_active: false,
            split_applied: config.split_cluster,
            pending_events,
            static_centers: Vec::new(),
            config,
            transport_scores: TransportScoreStore::default(),
        }
    }

    fn pick_bearer(
        &self,
        dist: f64,
        a: &LabNode,
        b: &LabNode,
        same_cluster: bool,
        now_ms: u64,
    ) -> Option<BearerKind> {
        bearer_for_link_scored(
            &LabBearerQuery {
                distance_m: dist,
                radio: &self.config.radio,
                a_online: a.online,
                b_online: b.online,
                a_internet: a.has_internet,
                b_internet: b.has_internet,
                same_cluster,
                payload_bytes: LAB_LINK_PAYLOAD_BYTES,
                now: now_ms,
            },
            &self.transport_scores,
        )
    }

    fn record_bearer_observations(&mut self, now_ms: u64) {
        for node in &self.nodes {
            if !node.online {
                continue;
            }
            let mut seen = HashSet::new();
            for bearer in &node.bearers_in_use {
                if !seen.insert(*bearer) {
                    continue;
                }
                let profile = bearer.link_profile();
                let success = self
                    .rng
                    .gen_bool(1.0 - profile.loss_per_mille as f64 / 1000.0);
                record_lab_transfer(
                    &mut self.transport_scores,
                    *bearer,
                    profile.latency_ms,
                    1024,
                    success,
                    now_ms,
                );
            }
        }
    }

    pub fn run(&mut self) -> LabReport {
        self.bootstrap_nodes();
        self.rebuild_links(0);

        let tick_ms = self.config.tick_ms.max(1);
        let ticks = self.config.duration_ms / tick_ms;
        for tick in 0..ticks {
            let now_ms = tick * tick_ms;
            self.apply_due_events(now_ms);
            self.mobility_tick(now_ms, tick_ms);
            self.churn_tick(tick_ms);
            self.rebuild_links(now_ms);
            self.inject_messages(tick, now_ms);
            self.sim.run_for(tick_ms);
            self.record_bearer_observations(now_ms);
            self.battery_tick(tick_ms);
        }

        let memory = estimate_memory_bytes(self.config.node_count);
        LabReport::from_run(crate::lab::report::LabRunSummary {
            scenario: self.config.name.clone(),
            seed: self.config.seed,
            node_count: self.config.node_count,
            duration_ms: self.config.duration_ms,
            attempted: self.attempted,
            metrics: self.sim.metrics(),
            battery: self.battery.clone(),
            memory_estimate_bytes: memory,
        })
    }

    fn bootstrap_nodes(&mut self) {
        let n = self.config.node_count;
        if matches!(self.config.mobility, MobilityPreset::StaticCluster { .. }) {
            let count = match self.config.mobility {
                MobilityPreset::StaticCluster { cluster_count, .. } => cluster_count,
                _ => 1,
            };
            let ws = self.config.world_size_m;
            for i in 0..count {
                self.static_centers.push(Vec2 {
                    x: ws * (i + 1) as f64 / (count + 1) as f64,
                    y: ws * 0.5,
                });
            }
        }

        let malicious_count = ((n as f64 * self.config.malicious_fraction).round() as usize).min(n);
        let mut malicious_idxs = std::collections::HashSet::new();
        while malicious_idxs.len() < malicious_count {
            malicious_idxs.insert(self.rng.gen_range_usize(0, n));
        }

        for idx in 0..n {
            let id = format!("N{idx:05}");
            let mut mobility = self.build_mobility(idx);
            let mut position =
                mobility.initial_position(&mut self.rng, idx, self.config.world_size_m);
            mobility.clamp_to_world(&mut position, self.config.world_size_m);
            let cluster = if self.split_applied && self.config.split_cluster {
                if position.x < self.config.world_size_m * 0.5 {
                    0
                } else {
                    1
                }
            } else {
                0
            };
            let malicious = malicious_idxs.contains(&idx);
            let bandwidth = self.config.bandwidth_bytes_per_sec;
            let _ = self.sim.build_transport(id.clone(), None, bandwidth);
            if malicious {
                self.sim
                    .set_malicious(id.clone(), self.config.malicious_drop_per_mille);
            }
            self.nodes.push(LabNode {
                id,
                position,
                mobility,
                online: true,
                has_internet: self.config.radio.internet_relay_available,
                cluster,
                malicious,
                battery: NodeBattery::phone_default(),
                bearers_in_use: Vec::new(),
            });
        }
    }

    fn build_mobility(&self, idx: usize) -> MobilityModel {
        match self.config.mobility {
            MobilityPreset::LevyFlight => MobilityModel::levy_flight_city(),
            MobilityPreset::RandomWaypoint { speed_m_per_s } => {
                MobilityModel::random_waypoint(speed_m_per_s)
            }
            MobilityPreset::StaticCluster {
                jitter_m,
                cluster_count,
            } => {
                let center = self.static_centers[idx % cluster_count.max(1)];
                MobilityModel::static_cluster(center, jitter_m)
            }
        }
    }

    fn rebuild_links(&mut self, now_ms: u64) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        if n <= 2_500 {
            self.rebuild_links_pairwise(now_ms);
        } else {
            self.rebuild_links_grid(now_ms);
        }
    }

    fn rebuild_links_pairwise(&mut self, now_ms: u64) {
        let mut pairs: Vec<(String, String, LinkProfile)> = Vec::new();
        let mut bearer_usage: HashMap<usize, Vec<BearerKind>> = HashMap::new();
        for i in 0..self.nodes.len() {
            if !self.nodes[i].online {
                continue;
            }
            for j in (i + 1)..self.nodes.len() {
                let a = &self.nodes[i];
                let b = &self.nodes[j];
                let same_cluster = !self.network_split_active || a.cluster == b.cluster;
                let dist = a.position.distance(&b.position);
                let Some(bearer) = self.pick_bearer(dist, a, b, same_cluster, now_ms) else {
                    continue;
                };
                let mut profile = bearer.link_profile();
                if self.config.ideal_links {
                    profile.loss_per_mille = 0;
                    profile.latency_ms = 1;
                }
                pairs.push((a.id.clone(), b.id.clone(), profile));
                bearer_usage.entry(i).or_default().push(bearer);
                bearer_usage.entry(j).or_default().push(bearer);
            }
        }
        for (idx, node) in self.nodes.iter_mut().enumerate() {
            node.bearers_in_use = bearer_usage.remove(&idx).unwrap_or_default();
        }
        self.sim.replace_links(&pairs);
    }

    fn rebuild_links_grid(&mut self, now_ms: u64) {
        let cell_size = self
            .config
            .radio
            .wifi_range_m
            .max(self.config.radio.ble_range_m)
            .max(1.0)
            / 2.0;
        let mut buckets: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for (idx, node) in self.nodes.iter().enumerate() {
            if !node.online {
                continue;
            }
            let cx = (node.position.x / cell_size).floor() as usize;
            let cy = (node.position.y / cell_size).floor() as usize;
            buckets.entry((cx, cy)).or_default().push(idx);
        }

        let mut pairs: Vec<(String, String, LinkProfile)> = Vec::new();
        let mut bearer_usage: HashMap<usize, Vec<BearerKind>> = HashMap::new();

        for ((cx, cy), members) in &buckets {
            for dx in -1i32..=1 {
                for dy in -1i32..=1 {
                    let nx = *cx as i32 + dx;
                    let ny = *cy as i32 + dy;
                    if nx < 0 || ny < 0 {
                        continue;
                    }
                    let Some(neighbors) = buckets.get(&(nx as usize, ny as usize)) else {
                        continue;
                    };
                    for &i in members {
                        for &j in neighbors {
                            if i >= j {
                                continue;
                            }
                            let a = &self.nodes[i];
                            let b = &self.nodes[j];
                            let same_cluster = !self.network_split_active || a.cluster == b.cluster;
                            let dist = a.position.distance(&b.position);
                            let Some(bearer) = self.pick_bearer(dist, a, b, same_cluster, now_ms)
                            else {
                                continue;
                            };
                            let mut profile = bearer.link_profile();
                            if self.config.ideal_links {
                                profile.loss_per_mille = 0;
                                profile.latency_ms = 1;
                            }
                            pairs.push((a.id.clone(), b.id.clone(), profile));
                            bearer_usage.entry(i).or_default().push(bearer);
                            bearer_usage.entry(j).or_default().push(bearer);
                        }
                    }
                }
            }
        }

        for (idx, node) in self.nodes.iter_mut().enumerate() {
            node.bearers_in_use = bearer_usage.remove(&idx).unwrap_or_default();
        }

        self.sim.replace_links(&pairs);
    }

    fn mobility_tick(&mut self, now_ms: u64, tick_ms: u64) {
        let dt = tick_ms as f64 / 1000.0;
        let ws = self.config.world_size_m;
        for node in &mut self.nodes {
            if !node.online {
                continue;
            }
            node.mobility
                .step(&mut self.rng, &mut node.position, now_ms, dt, ws);
            node.mobility.clamp_to_world(&mut node.position, ws);
            if self.split_applied && self.config.split_cluster && !self.network_split_active {
                node.cluster = if node.position.x < ws * 0.5 { 0 } else { 1 };
            }
        }
    }

    fn churn_tick(&mut self, tick_ms: u64) {
        if self.config.churn_per_hour <= 0.0 {
            return;
        }
        let p = self.config.churn_per_hour * (tick_ms as f64 / 3_600_000.0);
        for node in &mut self.nodes {
            if self.rng.gen_bool(p) {
                node.online = !node.online;
                if let Some(spec) = self.sim.nodes.get_mut(&node.id) {
                    spec.availability.online = node.online;
                }
            }
        }
    }

    fn inject_messages(&mut self, tick: u64, now_ms: u64) {
        if self.nodes.len() < 2 {
            return;
        }
        let interval = (self.config.duration_ms / self.config.tick_ms)
            / u64::from(self.config.messages_per_node.max(1));
        if interval == 0 || !tick.is_multiple_of(interval.max(1)) {
            return;
        }
        let from_idx = self.rng.gen_range_usize(0, self.nodes.len());
        if !self.nodes[from_idx].online {
            return;
        }
        let from_id = self.nodes[from_idx].id.clone();
        let from_pos = self.nodes[from_idx].position;
        let from_cluster = self.nodes[from_idx].cluster;
        let from_node = &self.nodes[from_idx];
        let candidates: Vec<usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(j, n)| {
                if *j == from_idx || !n.online {
                    return false;
                }
                if self.network_split_active && n.cluster != from_cluster {
                    return false;
                }
                let dist = from_pos.distance(&n.position);
                self.pick_bearer(dist, from_node, n, true, now_ms).is_some()
            })
            .map(|(j, _)| j)
            .collect();
        if candidates.is_empty() {
            return;
        }
        let to_idx = candidates[self.rng.gen_range_usize(0, candidates.len())];
        let to = self.nodes[to_idx].id.clone();
        let body = format!("lab-{tick}-{from_id}-{to}");
        let msg = DirectMessage {
            envelope: Envelope::new(from_id.clone(), Some(to.clone()), body.as_bytes().to_vec()),
            body,
        };
        self.attempted += 1;
        let _ = self.sim.action_tx.send(SimAction::SendDirect {
            from_peer: from_id,
            to_peer: to,
            message: WireMessage::Data(msg),
        });
    }

    fn battery_tick(&mut self, tick_ms: u64) {
        let dt = tick_ms as f64 / 1000.0;
        for node in &mut self.nodes {
            if !node.online {
                continue;
            }
            let unique: Vec<BearerKind> = node
                .bearers_in_use
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            self.battery.add_idle(&unique, dt);
            let idle_j: f64 = unique.iter().map(|b| b.idle_joules_per_sec() * dt).sum();
            node.battery.drain(idle_j);
        }
    }

    fn apply_due_events(&mut self, now_ms: u64) {
        while self
            .pending_events
            .first()
            .is_some_and(|e| event_time_ms(e) <= now_ms)
        {
            let event = self.pending_events.remove(0);
            match event {
                LabEvent::InternetOutage { .. } => {
                    self.config.radio.internet_relay_available = false;
                    for node in &mut self.nodes {
                        node.has_internet = false;
                    }
                }
                LabEvent::RemoveNodesFraction { fraction, .. } => {
                    let remove = ((self.nodes.len() as f64 * fraction).round() as usize)
                        .min(self.nodes.len());
                    let mut indices: Vec<usize> = (0..self.nodes.len()).collect();
                    for i in 0..remove {
                        let pick = self.rng.gen_range_usize(i, indices.len());
                        indices.swap(i, pick);
                    }
                    for &idx in &indices[..remove] {
                        let id = self.nodes[idx].id.clone();
                        self.sim.remove_node(&id);
                        self.nodes[idx].online = false;
                    }
                }
                LabEvent::NetworkSplit { .. } => {
                    self.network_split_active = true;
                    let ws = self.config.world_size_m;
                    for node in &mut self.nodes {
                        node.cluster = if node.position.x < ws * 0.5 { 0 } else { 1 };
                    }
                }
                LabEvent::NetworkMerge { .. } => {
                    self.network_split_active = false;
                    for node in &mut self.nodes {
                        node.cluster = 0;
                    }
                }
            }
        }
    }
}

fn event_time_ms(event: &LabEvent) -> u64 {
    match event {
        LabEvent::InternetOutage { at_ms } => *at_ms,
        LabEvent::RemoveNodesFraction { at_ms, .. } => *at_ms,
        LabEvent::NetworkSplit { at_ms } => *at_ms,
        LabEvent::NetworkMerge { at_ms } => *at_ms,
    }
}

fn estimate_memory_bytes(node_count: usize) -> u64 {
    // Rough: node spec + avg 8 links × 128 B + transport state.
    (node_count as u64).saturating_mul(4_096 + 8 * 128)
}

use crate::lab::preset::LabPreset;

/// CI regression gates — minimum delivery rates per preset (seed=42, ci_mode).
pub fn ci_gates() -> Vec<(LabPreset, f64)> {
    crate::lab::gates::lab_delivery_thresholds().to_vec()
}

pub fn run_ci_gates() -> anyhow::Result<Vec<LabReport>> {
    let mut reports = Vec::new();
    for (preset, min_rate) in ci_gates() {
        let config = crate::lab::preset::preset_config(preset, 42, 0, true);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        if report.delivery_rate < min_rate {
            anyhow::bail!(
                "MycenaLab CI gate failed: {} delivery {:.1}% < {:.0}% (seed=42)",
                report.scenario,
                report.delivery_rate * 100.0,
                min_rate * 100.0,
            );
        }
        reports.push(report);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab::preset::{preset_config, LabPreset};

    #[test]
    fn city_walk_ci_gate_passes() {
        let config = preset_config(LabPreset::CityWalk, 42, 200, true);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        assert!(
            report.delivery_rate >= 0.85,
            "city_walk delivery {:.1}%",
            report.delivery_rate * 100.0
        );
    }

    #[test]
    fn mycenalab_ci_gates_all() {
        run_ci_gates().expect("all CI gates");
    }

    #[test]
    fn stadium_dense_runs() {
        let config = preset_config(LabPreset::Stadium, 7, 800, true);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        assert!(report.delivery_rate > 0.0);
        assert_eq!(report.node_count, 800);
    }

    #[test]
    fn earthquake_removes_nodes() {
        let config = preset_config(LabPreset::Earthquake, 99, 150, true);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        assert!(report.messages_attempted > 0);
        assert!(report.delivery_rate >= 0.40);
    }
}
