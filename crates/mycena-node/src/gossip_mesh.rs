// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Runtime gossipsub mesh parameter hot-swap (AR-06 P0).

use libp2p::gossipsub::{self, TopicHash, TopicMeshConfig};
use mycena_core::mesh_profile::{MeshProfile, MeshProfileParams};

pub fn topic_mesh_config(params: &MeshProfileParams) -> TopicMeshConfig {
    TopicMeshConfig {
        mesh_n: params.mesh_n,
        mesh_n_low: params.mesh_n_low,
        mesh_n_high: params.mesh_n_high,
        mesh_outbound_min: params.mesh_outbound_min,
    }
}

/// Apply mesh profile to gossipsub default params and all currently subscribed topics.
pub fn apply_mesh_profile(gossip: &mut gossipsub::Behaviour, profile: MeshProfile) {
    let params = profile.params();
    let mesh = topic_mesh_config(&params);
    gossip.set_default_mesh_params(mesh.clone());
    let topics: Vec<TopicHash> = gossip.topics().cloned().collect();
    for topic in topics {
        gossip.set_topic_mesh_params(topic, mesh.clone());
    }
    tracing::info!(
        "gossip mesh hot-swap: profile={profile:?} mesh_n={} mesh_n_low={} mesh_n_high={}",
        params.mesh_n,
        params.mesh_n_low,
        params.mesh_n_high,
    );
}

/// Density backoff uses [`MeshProfile::Crowd`] unless manual crowd mode already selected it at init.
pub fn density_backoff_profile(baseline: MeshProfile, manual_crowd_mode: bool) -> MeshProfile {
    if manual_crowd_mode {
        baseline
    } else {
        MeshProfile::Crowd
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn density_backoff_uses_crowd_when_not_manual() {
        assert_eq!(
            density_backoff_profile(MeshProfile::Default, false),
            MeshProfile::Crowd
        );
        assert_eq!(
            density_backoff_profile(MeshProfile::Crowd, true),
            MeshProfile::Crowd
        );
    }

    #[test]
    fn crowd_params_tighter_than_default() {
        let crowd = topic_mesh_config(&MeshProfile::Crowd.params());
        let default = topic_mesh_config(&MeshProfile::Default.params());
        assert!(crowd.mesh_n < default.mesh_n);
        assert!(crowd.mesh_n_high < default.mesh_n_high);
    }
}
