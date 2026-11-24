// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! **Crisis Mode** — emergency traffic prioritization when infrastructure fails.
//!
//! Manual activation or auto when internet is fully unavailable and nearby SOS
//! traffic is observed (see [`evaluate_auto_activation`]).

use serde::{Deserialize, Serialize};

/// Tunable crisis runtime (applied by `mycena-node` when active).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrisisRuntimeParams {
    /// Forwarding tick interval (ms); lower = faster relay retries.
    pub forwarding_interval_ms: u64,
    /// Bloom anti-entropy interval (secs); higher = less background sync.
    pub sync_interval_secs: u64,
    /// Extra relay fanout for emergency traffic.
    pub relay_fanout_boost: usize,
    /// `max_hops` for SOS / resource-map envelopes.
    pub emergency_max_hops: u8,
    /// Privacy-shaping batch window (ms) — smaller batches in crisis.
    pub batch_min_ms: u64,
    pub batch_max_ms: u64,
    /// Messages drained per forward tick when node is Active.
    pub forward_budget_active: usize,
}

impl Default for CrisisRuntimeParams {
    fn default() -> Self {
        Self {
            forwarding_interval_ms: 150,
            sync_interval_secs: 300,
            relay_fanout_boost: 2,
            emergency_max_hops: 16,
            batch_min_ms: 200,
            batch_max_ms: 800,
            forward_budget_active: 16,
        }
    }
}

/// Baseline intervals when crisis mode is off (node defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrisisBaseline {
    pub forwarding_interval_ms: u64,
    pub sync_interval_secs: u64,
    pub max_relay_fanout: usize,
}

impl CrisisRuntimeParams {
    pub fn from_baseline(baseline: CrisisBaseline) -> Self {
        let mut p = Self::default();
        p.forwarding_interval_ms = p
            .forwarding_interval_ms
            .min(baseline.forwarding_interval_ms);
        p
    }

    pub fn effective_relay_fanout(&self, baseline: usize) -> usize {
        (baseline + self.relay_fanout_boost).min(8)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrisisAutoConfig {
    /// SOS posts within this window (ms) count as “nearby activity”.
    pub sos_activity_window_ms: u64,
    /// Minimum geohash prefix overlap (chars) for “nearby” SOS.
    pub geohash_prefix_len: usize,
    /// Independent trusted SOS relay peers required before auto crisis activates.
    pub min_trusted_sos_sources: usize,
}

impl Default for CrisisAutoConfig {
    fn default() -> Self {
        Self {
            sos_activity_window_ms: 15 * 60 * 1000,
            geohash_prefix_len: 4,
            min_trusted_sos_sources: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CrisisManualMode {
    #[default]
    FollowAuto,
    ForceOn,
    ForceOff,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrisisModeState {
    pub manual: CrisisManualMode,
    pub auto_active: bool,
}

impl CrisisModeState {
    pub fn is_active(&self) -> bool {
        match self.manual {
            CrisisManualMode::ForceOn => true,
            CrisisManualMode::ForceOff => false,
            CrisisManualMode::FollowAuto => self.auto_active,
        }
    }
}

/// Auto-activate when internet is down, user listens for SOS, and enough trusted
/// independent SOS sources were observed nearby (see [`CrisisAutoConfig::min_trusted_sos_sources`]).
pub fn evaluate_auto_activation(
    config: &CrisisAutoConfig,
    internet_reachable: bool,
    listen_sos: bool,
    trusted_nearby_sos_sources: usize,
) -> bool {
    !internet_reachable
        && listen_sos
        && trusted_nearby_sos_sources >= config.min_trusted_sos_sources
}

/// Apply [`EnergyProfile`] adjustments to crisis runtime params (thermal/movement).
pub fn adjust_runtime_params(
    base: CrisisRuntimeParams,
    profile: &crate::energy::EnergyProfile,
) -> CrisisRuntimeParams {
    let mut p = base;
    if profile.thermal == crate::energy::ThermalState::Critical {
        p.forwarding_interval_ms = p.forwarding_interval_ms.saturating_mul(2);
        p.relay_fanout_boost = 0;
    }
    p.forward_budget_active = profile.scale_crisis_forward_budget(p.forward_budget_active);
    p.relay_fanout_boost = profile.scale_crisis_relay_fanout_boost(p.relay_fanout_boost);
    if profile.thermal == crate::energy::ThermalState::Warm {
        p.forwarding_interval_ms = p
            .forwarding_interval_ms
            .saturating_add(p.forwarding_interval_ms / 4);
    }
    p
}

/// Effective relay fanout when crisis mode is active, including energy profile.
pub fn effective_relay_fanout_with_profile(
    baseline: usize,
    params: &CrisisRuntimeParams,
    profile: &crate::energy::EnergyProfile,
) -> usize {
    let boost = profile.scale_crisis_relay_fanout_boost(params.relay_fanout_boost);
    (baseline + boost).min(8)
}

/// True when `sos_geohash` is “near” `local_prefix` (shared prefix).
pub fn geohash_nearby(local_prefix: &str, sos_geohash: &str, prefix_len: usize) -> bool {
    let local = local_prefix.trim();
    let sos = sos_geohash.trim();
    if local.is_empty() || sos.is_empty() || prefix_len == 0 {
        return false;
    }
    let n = prefix_len.min(local.len()).min(sos.len());
    local.as_bytes()[..n] == sos.as_bytes()[..n]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn manual_override_wins() {
        let mut s = CrisisModeState {
            manual: CrisisManualMode::ForceOn,
            auto_active: false,
        };
        assert!(s.is_active());
        s.manual = CrisisManualMode::ForceOff;
        assert!(!s.is_active());
    }

    #[test]
    fn auto_requires_all_signals() {
        let cfg = CrisisAutoConfig::default();
        assert!(!evaluate_auto_activation(&cfg, true, true, 3));
        assert!(!evaluate_auto_activation(&cfg, false, false, 3));
        assert!(!evaluate_auto_activation(&cfg, false, true, 0));
        assert!(!evaluate_auto_activation(&cfg, false, true, 1));
        assert!(!evaluate_auto_activation(&cfg, false, true, 2));
        assert!(evaluate_auto_activation(&cfg, false, true, 3));
    }

    #[test]
    fn single_sos_does_not_activate_crisis() {
        let cfg = CrisisAutoConfig::default();
        assert!(!evaluate_auto_activation(&cfg, false, true, 1));
    }

    #[test]
    fn geohash_prefix_match() {
        assert!(geohash_nearby("u4pr", "u4pruyd", 4));
        assert!(!geohash_nearby("u4pr", "u5xxx", 4));
    }

    #[test]
    fn critical_thermal_reduces_crisis_budget() {
        use crate::energy::{EnergyProfile, MovementIndicator, NodeState, ThermalState};
        let base = CrisisRuntimeParams::default();
        let profile = EnergyProfile {
            battery: NodeState::Active,
            thermal: ThermalState::Critical,
            movement: MovementIndicator::Unknown,
        };
        let adjusted = adjust_runtime_params(base, &profile);
        assert!(adjusted.forward_budget_active < base.forward_budget_active);
        assert_eq!(adjusted.relay_fanout_boost, 0);
    }
}
