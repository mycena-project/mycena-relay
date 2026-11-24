// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Gossipsub mesh tuning profiles for sparse vs dense clusters (Phase D1).

/// Tunable gossipsub mesh parameters (applied in `mycena-node` behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshProfileParams {
    pub mesh_n: usize,
    pub mesh_n_low: usize,
    pub mesh_n_high: usize,
    pub mesh_outbound_min: usize,
    pub heartbeat_interval_secs: u64,
    pub history_length: usize,
    pub history_gossip: usize,
}

/// Mesh density profile — `Dense` / `Crowd` for festival-scale clusters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeshProfile {
    #[default]
    Default,
    Dense,
    /// Tighter mesh for auto density backoff (AR-06).
    Crowd,
    /// Disaster / crisis: wider mesh, faster heartbeats (pair with [`crate::crisis_mode`]).
    Disaster,
}

impl MeshProfile {
    /// Read from `MYCENA_MESH_PROFILE` (`default` | `dense`).
    pub fn from_env() -> Self {
        match std::env::var("MYCENA_MESH_PROFILE")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("dense") => Self::Dense,
            Some("crowd") => Self::Crowd,
            Some("disaster") => Self::Disaster,
            _ => Self::Default,
        }
    }

    pub fn params(self) -> MeshProfileParams {
        match self {
            Self::Default => MeshProfileParams {
                mesh_n: 6,
                mesh_n_low: 5,
                mesh_n_high: 12,
                mesh_outbound_min: 2,
                heartbeat_interval_secs: 1,
                history_length: 5,
                history_gossip: 3,
            },
            Self::Dense => MeshProfileParams {
                mesh_n: 8,
                mesh_n_low: 6,
                mesh_n_high: 14,
                mesh_outbound_min: 2,
                heartbeat_interval_secs: 2,
                history_length: 8,
                history_gossip: 4,
            },
            Self::Crowd => MeshProfileParams {
                mesh_n: 4,
                mesh_n_low: 3,
                mesh_n_high: 6,
                mesh_outbound_min: 1,
                heartbeat_interval_secs: 3,
                history_length: 4,
                history_gossip: 2,
            },
            Self::Disaster => MeshProfileParams {
                mesh_n: 10,
                mesh_n_low: 8,
                mesh_n_high: 16,
                mesh_outbound_min: 3,
                heartbeat_interval_secs: 1,
                history_length: 10,
                history_gossip: 5,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn dense_profile_has_wider_mesh() {
        let dense = MeshProfile::Dense.params();
        let default = MeshProfile::Default.params();
        assert!(dense.mesh_n >= default.mesh_n);
        assert!(dense.history_length >= default.history_length);
    }

    #[test]
    fn crowd_profile_is_tighter_than_dense() {
        let crowd = MeshProfile::Crowd.params();
        let dense = MeshProfile::Dense.params();
        assert!(crowd.mesh_n < dense.mesh_n);
        assert!(crowd.history_gossip <= dense.history_gossip);
    }
}
