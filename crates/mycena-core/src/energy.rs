// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum NodeState {
    Active,
    Intermittent,
    Passive,
}

impl NodeState {
    pub fn sync_interval_secs(self) -> u64 {
        match self {
            Self::Active => 2,
            Self::Intermittent => 10,
            Self::Passive => 45,
        }
    }

    /// Whether optional Shade cover-traffic may be emitted (SD-070).
    pub fn allows_cover_traffic(self) -> bool {
        !matches!(self, Self::Passive)
    }

    /// Multiplier on base cover interval (Passive → disabled via 0).
    pub fn cover_traffic_interval_multiplier(self) -> u64 {
        match self {
            Self::Active => 1,
            Self::Intermittent => 2,
            Self::Passive => 0,
        }
    }
}

/// Coarse device thermal headroom (from platform APIs when available).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Default)]
pub enum ThermalState {
    #[default]
    Normal,
    Warm,
    Critical,
}

impl ThermalState {
    /// Wi‑Fi Direct frames drained per host poll when warm (Normal = no cap here).
    pub fn wifi_direct_drain_cap(self) -> Option<usize> {
        match self {
            Self::Normal => None,
            Self::Warm => Some(2),
            Self::Critical => Some(0),
        }
    }

    pub fn allows_wifi_direct(self) -> bool {
        !matches!(self, Self::Critical)
    }
}

/// Whether this device is likely stationary or moving, derived from encounter patterns.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Default)]
pub enum MovementIndicator {
    #[default]
    Unknown,
    Stationary,
    InMotion,
}

/// Combined runtime energy/thermal/movement profile for relay and bearer policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub struct EnergyProfile {
    pub battery: NodeState,
    pub thermal: ThermalState,
    pub movement: MovementIndicator,
}

impl Default for EnergyProfile {
    fn default() -> Self {
        Self {
            battery: NodeState::Active,
            thermal: ThermalState::Normal,
            movement: MovementIndicator::Unknown,
        }
    }
}

impl EnergyProfile {
    /// Effective mesh forwarding energy state (thermal overrides battery).
    pub fn effective_forwarding_state(self, crowd_density_active: bool) -> NodeState {
        if crowd_density_active {
            return NodeState::Intermittent;
        }
        match self.thermal {
            ThermalState::Critical => NodeState::Passive,
            ThermalState::Warm if self.battery == NodeState::Active => NodeState::Intermittent,
            _ => self.battery,
        }
    }

    /// Scale crisis runtime params for thermal/movement (applied when crisis is active).
    pub fn scale_crisis_forward_budget(self, base: usize) -> usize {
        let mut budget = base;
        if self.thermal == ThermalState::Critical {
            budget = (budget / 2).max(2);
        } else if self.thermal == ThermalState::Warm {
            budget = (budget * 3 / 4).max(2);
        }
        if self.movement == MovementIndicator::Stationary {
            budget = (budget * 3 / 4).max(2);
        } else if self.movement == MovementIndicator::InMotion
            && self.battery == NodeState::Active
            && self.thermal == ThermalState::Normal
        {
            budget = budget.saturating_add(2).min(base.saturating_add(4));
        }
        budget.max(1)
    }

    pub fn scale_crisis_relay_fanout_boost(self, boost: usize) -> usize {
        if self.thermal == ThermalState::Critical {
            0
        } else if self.movement == MovementIndicator::InMotion
            && self.battery == NodeState::Active
            && self.thermal == ThermalState::Normal
        {
            boost.saturating_add(1).min(3)
        } else if self.movement == MovementIndicator::Stationary {
            boost.min(1)
        } else {
            boost
        }
    }

    /// Extra custody forward attempts per tick (0 = default single plan).
    pub fn custody_extra_forwards(self) -> usize {
        match (self.movement, self.thermal, self.battery) {
            (MovementIndicator::InMotion, ThermalState::Normal, NodeState::Active) => 1,
            (_, ThermalState::Critical, _) => 0,
            _ => 0,
        }
    }

    pub fn should_skip_nonessential_relay(self) -> bool {
        self.thermal == ThermalState::Critical
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn cover_traffic_gated_by_energy() {
        assert!(NodeState::Active.allows_cover_traffic());
        assert!(NodeState::Intermittent.allows_cover_traffic());
        assert!(!NodeState::Passive.allows_cover_traffic());
        assert_eq!(NodeState::Passive.cover_traffic_interval_multiplier(), 0);
    }

    #[test]
    fn critical_thermal_forces_passive_regardless_of_battery() {
        let p = EnergyProfile {
            battery: NodeState::Active,
            thermal: ThermalState::Critical,
            movement: MovementIndicator::Unknown,
        };
        assert_eq!(p.effective_forwarding_state(false), NodeState::Passive);
        assert!(!p.thermal.allows_wifi_direct());
    }

    #[test]
    fn warm_thermal_caps_active_to_intermittent() {
        let p = EnergyProfile {
            battery: NodeState::Active,
            thermal: ThermalState::Warm,
            movement: MovementIndicator::Unknown,
        };
        assert_eq!(p.effective_forwarding_state(false), NodeState::Intermittent);
        assert_eq!(p.thermal.wifi_direct_drain_cap(), Some(2));
    }

    #[test]
    fn in_motion_boosts_crisis_budget_when_healthy() {
        let p = EnergyProfile {
            battery: NodeState::Active,
            thermal: ThermalState::Normal,
            movement: MovementIndicator::InMotion,
        };
        assert!(p.scale_crisis_forward_budget(16) > 16);
        assert!(p.custody_extra_forwards() > 0);
    }

    #[test]
    fn stationary_reduces_crisis_budget() {
        let p = EnergyProfile {
            battery: NodeState::Active,
            thermal: ThermalState::Normal,
            movement: MovementIndicator::Stationary,
        };
        assert!(p.scale_crisis_forward_budget(16) < 16);
    }
}
