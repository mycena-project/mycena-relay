// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Scenario presets for MycenaLab.

use crate::lab::radio::RadioConfig;
use crate::lab::runner::{LabEvent, MobilityPreset, PresetConfig};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum LabPreset {
    CityWalk,
    Stadium,
    Earthquake,
    NetworkSplit,
    /// Single tight cluster — 10k+ nodes (grid link discovery).
    DenseCluster,
    /// 50% adversarial peers with high drop rate (Sybil / eclipse stress).
    SybilSwarm,
}

impl LabPreset {
    pub fn all() -> &'static [LabPreset] {
        &[
            LabPreset::CityWalk,
            LabPreset::Stadium,
            LabPreset::Earthquake,
            LabPreset::NetworkSplit,
            LabPreset::DenseCluster,
            LabPreset::SybilSwarm,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::CityWalk => "city_walk",
            Self::Stadium => "stadium",
            Self::Earthquake => "earthquake",
            Self::NetworkSplit => "network_split",
            Self::DenseCluster => "dense_cluster",
            Self::SybilSwarm => "sybil_swarm",
        }
    }

    pub fn german_alias(self) -> &'static str {
        match self {
            Self::CityWalk => "stadtspaziergang",
            Self::Stadium => "stadion",
            Self::Earthquake => "erdbeben",
            Self::NetworkSplit => "netzwerk-split",
            Self::DenseCluster => "dichter-cluster",
            Self::SybilSwarm => "sybil-schwarm",
        }
    }

    pub fn parse_name(s: &str) -> Option<Self> {
        match s {
            "city_walk" | "stadtspaziergang" => Some(Self::CityWalk),
            "stadium" | "stadion" => Some(Self::Stadium),
            "earthquake" | "erdbeben" => Some(Self::Earthquake),
            "network_split" | "netzwerk-split" => Some(Self::NetworkSplit),
            "dense_cluster" | "dichter-cluster" => Some(Self::DenseCluster),
            "sybil_swarm" | "sybil-schwarm" => Some(Self::SybilSwarm),
            _ => None,
        }
    }
}

/// Build a preset; `ci_mode` shortens durations for CI gates.
pub fn preset_config(
    preset: LabPreset,
    seed: u64,
    node_count: usize,
    ci_mode: bool,
) -> PresetConfig {
    match preset {
        LabPreset::CityWalk => city_walk(seed, node_count, ci_mode),
        LabPreset::Stadium => stadium(seed, node_count, ci_mode),
        LabPreset::Earthquake => earthquake(seed, node_count, ci_mode),
        LabPreset::NetworkSplit => network_split(seed, node_count, ci_mode),
        LabPreset::DenseCluster => dense_cluster(seed, node_count, ci_mode),
        LabPreset::SybilSwarm => sybil_swarm(seed, node_count, ci_mode),
    }
}

fn adversary_fraction(ci_mode: bool, preserve_in_ci: bool, fraction: f64) -> f64 {
    if ci_mode && !preserve_in_ci {
        0.0
    } else {
        fraction
    }
}

pub fn city_walk(seed: u64, nodes: usize, ci_mode: bool) -> PresetConfig {
    let node_count = nodes.clamp(100, 1000);
    PresetConfig {
        name: LabPreset::CityWalk.name().to_string(),
        seed,
        node_count,
        world_size_m: if ci_mode { 600.0 } else { 2_000.0 },
        duration_ms: if ci_mode { 90_000 } else { 600_000 },
        tick_ms: if ci_mode { 1_000 } else { 2_000 },
        mobility: MobilityPreset::LevyFlight,
        radio: RadioConfig {
            ble_range_m: 45.0,
            wifi_range_m: 180.0,
            internet_relay_available: true,
        },
        malicious_fraction: adversary_fraction(ci_mode, false, 0.02),
        malicious_drop_per_mille: if ci_mode { 0 } else { 300 },
        churn_per_hour: if ci_mode { 0.0 } else { 0.05 },
        messages_per_node: if ci_mode { 12 } else { 8 },
        bandwidth_bytes_per_sec: 64 * 1024,
        events: Vec::new(),
        split_cluster: false,
        ideal_links: ci_mode,
    }
}

pub fn stadium(seed: u64, nodes: usize, ci_mode: bool) -> PresetConfig {
    let node_count = if ci_mode {
        nodes.clamp(500, 2_000)
    } else {
        nodes.max(10_000)
    };
    PresetConfig {
        name: LabPreset::Stadium.name().to_string(),
        seed,
        node_count,
        world_size_m: 250.0,
        duration_ms: if ci_mode { 60_000 } else { 300_000 },
        tick_ms: if ci_mode { 2_000 } else { 5_000 },
        mobility: MobilityPreset::StaticCluster {
            jitter_m: 35.0,
            cluster_count: 3,
        },
        radio: RadioConfig {
            ble_range_m: 30.0,
            wifi_range_m: 80.0,
            internet_relay_available: false,
        },
        malicious_fraction: adversary_fraction(ci_mode, false, 0.01),
        malicious_drop_per_mille: if ci_mode { 0 } else { 200 },
        churn_per_hour: if ci_mode { 0.0 } else { 0.02 },
        messages_per_node: if ci_mode { 6 } else { 4 },
        bandwidth_bytes_per_sec: 32 * 1024,
        events: Vec::new(),
        split_cluster: false,
        ideal_links: ci_mode,
    }
}

pub fn earthquake(seed: u64, nodes: usize, ci_mode: bool) -> PresetConfig {
    let node_count = nodes.clamp(100, 1000);
    let duration_ms = if ci_mode { 120_000 } else { 600_000 };
    let quake_at = duration_ms * 30 / 100;
    PresetConfig {
        name: LabPreset::Earthquake.name().to_string(),
        seed,
        node_count,
        world_size_m: if ci_mode { 800.0 } else { 1_500.0 },
        duration_ms,
        tick_ms: 1_000,
        mobility: MobilityPreset::RandomWaypoint { speed_m_per_s: 1.0 },
        radio: RadioConfig::default(),
        malicious_fraction: adversary_fraction(ci_mode, false, 0.03),
        malicious_drop_per_mille: if ci_mode { 0 } else { 250 },
        churn_per_hour: 0.0,
        messages_per_node: if ci_mode { 8 } else { 10 },
        bandwidth_bytes_per_sec: 64 * 1024,
        events: vec![
            LabEvent::InternetOutage { at_ms: quake_at },
            LabEvent::RemoveNodesFraction {
                at_ms: quake_at,
                fraction: 0.30,
            },
        ],
        split_cluster: false,
        ideal_links: ci_mode,
    }
}

pub fn network_split(seed: u64, nodes: usize, ci_mode: bool) -> PresetConfig {
    let node_count = nodes.clamp(100, 1000);
    // Full preset: 6h sim; CI: 6 minutes with split at 2min, merge at 4min.
    let duration_ms = if ci_mode { 360_000 } else { 6 * 60 * 60 * 1000 };
    let split_at = if ci_mode { 120_000 } else { 60 * 60 * 1000 };
    let merge_at = if ci_mode { 240_000 } else { 5 * 60 * 60 * 1000 };
    PresetConfig {
        name: LabPreset::NetworkSplit.name().to_string(),
        seed,
        node_count,
        world_size_m: if ci_mode { 800.0 } else { 2_000.0 },
        duration_ms,
        tick_ms: if ci_mode { 1_000 } else { 10_000 },
        mobility: MobilityPreset::LevyFlight,
        radio: RadioConfig::default(),
        malicious_fraction: adversary_fraction(ci_mode, false, 0.02),
        malicious_drop_per_mille: if ci_mode { 0 } else { 200 },
        churn_per_hour: if ci_mode { 0.0 } else { 0.03 },
        messages_per_node: if ci_mode { 8 } else { 6 },
        bandwidth_bytes_per_sec: 64 * 1024,
        events: vec![
            LabEvent::NetworkSplit { at_ms: split_at },
            LabEvent::NetworkMerge { at_ms: merge_at },
        ],
        split_cluster: true,
        ideal_links: ci_mode,
    }
}

/// One tight static cluster — stadium-scale density without multi-bowl layout.
pub fn dense_cluster(seed: u64, nodes: usize, ci_mode: bool) -> PresetConfig {
    let node_count = if ci_mode {
        nodes.clamp(1_500, 3_000)
    } else {
        nodes.max(10_000)
    };
    PresetConfig {
        name: LabPreset::DenseCluster.name().to_string(),
        seed,
        node_count,
        world_size_m: 100.0,
        duration_ms: if ci_mode { 45_000 } else { 180_000 },
        tick_ms: if ci_mode { 3_000 } else { 5_000 },
        mobility: MobilityPreset::StaticCluster {
            jitter_m: 12.0,
            cluster_count: 1,
        },
        radio: RadioConfig {
            ble_range_m: 28.0,
            wifi_range_m: 70.0,
            internet_relay_available: false,
        },
        malicious_fraction: adversary_fraction(ci_mode, false, 0.01),
        malicious_drop_per_mille: if ci_mode { 0 } else { 150 },
        churn_per_hour: if ci_mode { 0.0 } else { 0.01 },
        messages_per_node: if ci_mode { 4 } else { 3 },
        bandwidth_bytes_per_sec: 24 * 1024,
        events: Vec::new(),
        split_cluster: false,
        ideal_links: ci_mode,
    }
}

/// 50% adversarial peers — spatial delivery under Sybil-style drop attacks.
pub fn sybil_swarm(seed: u64, nodes: usize, ci_mode: bool) -> PresetConfig {
    let node_count = nodes.clamp(100, 1000);
    PresetConfig {
        name: LabPreset::SybilSwarm.name().to_string(),
        seed,
        node_count,
        world_size_m: if ci_mode { 500.0 } else { 1_200.0 },
        duration_ms: if ci_mode { 90_000 } else { 300_000 },
        tick_ms: if ci_mode { 1_000 } else { 2_000 },
        mobility: MobilityPreset::LevyFlight,
        radio: RadioConfig {
            ble_range_m: 40.0,
            wifi_range_m: 150.0,
            internet_relay_available: true,
        },
        malicious_fraction: adversary_fraction(ci_mode, true, 0.50),
        malicious_drop_per_mille: 700,
        churn_per_hour: 0.0,
        messages_per_node: if ci_mode { 10 } else { 8 },
        bandwidth_bytes_per_sec: 64 * 1024,
        events: Vec::new(),
        split_cluster: false,
        ideal_links: ci_mode,
    }
}
