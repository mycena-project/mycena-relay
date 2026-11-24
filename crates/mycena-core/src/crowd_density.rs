// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Automatic crowd-mode backoff when visible peer count exceeds a threshold (AR-06).

/// Hysteresis config for density-triggered crowd backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrowdDensityConfig {
    /// Enter backoff when visible peers ≥ this (0 = disabled).
    pub enter_threshold: usize,
    /// Leave backoff when visible peers ≤ this.
    pub clear_threshold: usize,
    /// Relay fanout cap while density backoff is active.
    pub max_relay_fanout: usize,
    /// Direct peer cap while density backoff is active.
    pub max_direct_peers: usize,
}

impl Default for CrowdDensityConfig {
    fn default() -> Self {
        Self {
            enter_threshold: default_enter_threshold(),
            clear_threshold: default_clear_threshold(),
            max_relay_fanout: 2,
            max_direct_peers: 20,
        }
    }
}

impl CrowdDensityConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("MYCENA_CROWD_DENSITY_ENTER") {
            if let Ok(n) = v.parse() {
                cfg.enter_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("MYCENA_CROWD_DENSITY_CLEAR") {
            if let Ok(n) = v.parse() {
                cfg.clear_threshold = n;
            }
        }
        cfg
    }

    pub fn disabled() -> Self {
        Self {
            enter_threshold: 0,
            clear_threshold: 0,
            max_relay_fanout: 3,
            max_direct_peers: 50,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enter_threshold > 0 && self.clear_threshold < self.enter_threshold
    }

    /// Update density-backoff state with hysteresis.
    pub fn should_activate(&self, visible_peers: usize, currently_active: bool) -> bool {
        if !self.is_enabled() {
            return false;
        }
        if currently_active {
            visible_peers > self.clear_threshold
        } else {
            visible_peers >= self.enter_threshold
        }
    }
}

fn default_enter_threshold() -> usize {
    25
}

fn default_clear_threshold() -> usize {
    20
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn hysteresis_prevents_flap() {
        let cfg = CrowdDensityConfig::default();
        assert!(!cfg.should_activate(24, false));
        assert!(cfg.should_activate(25, false));
        assert!(cfg.should_activate(22, true));
        assert!(!cfg.should_activate(20, true));
    }

    #[test]
    fn disabled_when_enter_zero() {
        let cfg = CrowdDensityConfig::disabled();
        assert!(!cfg.should_activate(500, false));
    }
}
