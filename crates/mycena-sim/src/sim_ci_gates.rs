// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Unified CI regression gates — MycenaLab presets + custody / trust / bearer scenarios.
//!
//! Thresholds are documented in `docs/MYCENALAB.md` and enforced by
//! `cargo test -p mycena-sim sim_ci_gates`.

use crate::commuter_custody_scenarios::commuter_custody_comparison;
use crate::custody_scenarios::city_custody_delivery;
use crate::dense_cluster::dense_cluster_shard_containment;
use crate::eclipse_scenarios::{eclipse_relay_resilience, geohash_eclipse_relay_resilience};
use crate::geohash_custody_scenarios::geohash_custody_carrier_resilience;
use crate::lab::gates::lab_delivery_thresholds;
use crate::lab::preset::LabPreset;
use crate::lab::report::LabReport;
use crate::lab::runner::run_ci_gates;
use crate::lab::transport_score::verify_lab_transport_score_gates;
use crate::local_sybil_dense::run_local_sybil_dense_ci_gate as run_local_sybil_dense_gate;
use crate::resource_disinfo_scenarios::run_resource_disinfo_ci_gate;
use crate::sos_disinfo_scenarios::run_sos_disinfo_ci_gate;
use crate::sos_flood_scenarios::run_sos_flood_ci_gate;

/// Documented minimum delivery rate for a MycenaLab preset (seed=42, ci_mode).
#[derive(Debug, Clone, Copy)]
pub struct LabDeliveryGate {
    pub preset: LabPreset,
    pub min_delivery_rate: f64,
}

/// All spatial Lab preset gates — single source: [`crate::lab::gates::lab_delivery_thresholds`].
pub fn lab_delivery_gates() -> Vec<LabDeliveryGate> {
    lab_delivery_thresholds()
        .iter()
        .map(|(preset, min_delivery_rate)| LabDeliveryGate {
            preset: *preset,
            min_delivery_rate: *min_delivery_rate,
        })
        .collect()
}

/// Run Lab preset gates only.
pub fn run_lab_ci_gates() -> anyhow::Result<Vec<LabReport>> {
    run_ci_gates()
}

/// Custody DTN: city-scale delivery must succeed (80 nodes, seed=1).
pub fn run_custody_ci_gate() -> anyhow::Result<()> {
    let report = city_custody_delivery(1, 80);
    if !report.delivered {
        anyhow::bail!(
            "custody CI gate failed: message not delivered (steps={})",
            report.steps
        );
    }
    Ok(())
}

/// Commuter routing: delivery-probability must deliver faster than recency-only.
pub fn run_commuter_custody_ci_gate() -> anyhow::Result<()> {
    let cmp = commuter_custody_comparison(7);
    if !cmp.delivery_probability_delivered {
        anyhow::bail!("commuter custody CI gate: delivery-probability routing did not deliver");
    }
    if cmp.steps_probability > cmp.steps_recency {
        anyhow::bail!(
            "commuter custody CI gate: probability steps {} > recency {} (expected faster delivery)",
            cmp.steps_probability,
            cmp.steps_recency
        );
    }
    if cmp.latency_probability_ms > cmp.latency_recency_ms {
        anyhow::bail!(
            "commuter custody CI gate: probability latency {}ms > recency {}ms",
            cmp.latency_probability_ms,
            cmp.latency_recency_ms
        );
    }
    Ok(())
}

/// TransportScore: Internet → Wi‑Fi → BLE → custody fallback + lab bearer mapping.
pub fn run_transport_score_ci_gate() -> anyhow::Result<()> {
    verify_lab_transport_score_gates()
}

/// Eclipse / Sybil relay: diversity must preserve legit peer in every round.
pub fn run_eclipse_ci_gate() -> anyhow::Result<()> {
    let report = eclipse_relay_resilience(20);
    if report.score_only_legit_relay_rounds != 0 {
        anyhow::bail!(
            "eclipse CI gate: score-only routing picked legit peer {} times (expected 0)",
            report.score_only_legit_relay_rounds
        );
    }
    if report.diverse_legit_relay_rounds != report.traffic_rounds {
        anyhow::bail!(
            "eclipse CI gate: diverse routing missed legit peer in some rounds ({}/{})",
            report.diverse_legit_relay_rounds,
            report.traffic_rounds
        );
    }
    Ok(())
}

/// Geohash-clustered eclipse relay: diversity preserves geographically distinct legit peer.
pub fn run_geohash_eclipse_ci_gate() -> anyhow::Result<()> {
    let report = geohash_eclipse_relay_resilience(20);
    if report.score_only_legit_relay_rounds != 0 {
        anyhow::bail!(
            "geohash eclipse CI gate: score-only routing picked legit peer {} times (expected 0)",
            report.score_only_legit_relay_rounds
        );
    }
    if report.diverse_legit_relay_rounds != report.traffic_rounds {
        anyhow::bail!(
            "geohash eclipse CI gate: diverse routing missed legit peer ({}/{})",
            report.diverse_legit_relay_rounds,
            report.traffic_rounds
        );
    }
    Ok(())
}

/// Geohash-clustered custody carrier: diversity picks distant legit carrier every round.
pub fn run_geohash_custody_ci_gate() -> anyhow::Result<()> {
    let report = geohash_custody_carrier_resilience(20);
    if report.score_only_legit_rounds != 0 {
        anyhow::bail!(
            "geohash custody CI gate: score-only picked legit carrier {} times (expected 0)",
            report.score_only_legit_rounds
        );
    }
    if report.diverse_legit_rounds != report.selection_rounds {
        anyhow::bail!(
            "geohash custody CI gate: diverse routing missed legit carrier ({}/{})",
            report.diverse_legit_rounds,
            report.selection_rounds
        );
    }
    Ok(())
}

/// Local dense Sybil (200 legit + 500 co-located fakes): diversity preserves legit delivery.
pub fn run_local_sybil_dense_ci_gate() -> anyhow::Result<()> {
    run_local_sybil_dense_gate().map(|_| ())
}

/// SOS crisis-message disinformation must keep conflicting claims low-trust.
pub fn run_sos_disinfo_ci_gate_wrapper() -> anyhow::Result<()> {
    run_sos_disinfo_ci_gate().map(|_| ())
}

/// Resource-map disinformation flood must keep conflicting claims low-trust.
pub fn run_resource_disinfo_ci_gate_wrapper() -> anyhow::Result<()> {
    run_resource_disinfo_ci_gate().map(|_| ())
}

/// SOS Sybil flood must not trigger global crisis; three legit sources must.
pub fn run_sos_flood_ci_gate_wrapper() -> anyhow::Result<()> {
    run_sos_flood_ci_gate().map(|_| ())
}

/// Dense cluster gossip shard containment (520 nodes analytical model).
pub fn run_dense_cluster_ci_gate() -> anyhow::Result<()> {
    let report = dense_cluster_shard_containment(520);
    if report.shard_containment_ratio >= 0.15 {
        anyhow::bail!(
            "dense cluster CI gate: shard containment {:.2} >= 0.15 (fanout not contained)",
            report.shard_containment_ratio
        );
    }
    if !report.density_backoff_triggered {
        anyhow::bail!("dense cluster CI gate: crowd density backoff should trigger at 520 nodes");
    }
    Ok(())
}

/// Run all documented sim CI gates (Lab + custody + trust + bearer-related).
pub fn run_all_sim_ci_gates() -> anyhow::Result<Vec<LabReport>> {
    let lab_reports = run_lab_ci_gates()?;
    run_transport_score_ci_gate()?;
    run_custody_ci_gate()?;
    run_commuter_custody_ci_gate()?;
    run_eclipse_ci_gate()?;
    run_geohash_eclipse_ci_gate()?;
    run_geohash_custody_ci_gate()?;
    run_local_sybil_dense_ci_gate()?;
    run_sos_flood_ci_gate_wrapper()?;
    run_resource_disinfo_ci_gate_wrapper()?;
    run_sos_disinfo_ci_gate_wrapper()?;
    run_dense_cluster_ci_gate()?;
    Ok(lab_reports)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::lab::preset::preset_config;
    use crate::lab::runner::LabRunner;

    /// Primary CI entry point — matches `.github/workflows/ci.yml`.
    #[test]
    fn sim_ci_gates_all() {
        run_all_sim_ci_gates().expect("all sim CI gates");
    }

    #[test]
    fn mycenalab_ci_gates() {
        run_ci_gates().expect("legacy lab CI gates");
    }

    #[test]
    fn transport_score_ci_gate() {
        run_transport_score_ci_gate().expect("transport score CI gate");
    }

    #[test]
    fn lab_delivery_thresholds_match_gates_helper() {
        let from_gates: Vec<_> = lab_delivery_gates()
            .into_iter()
            .map(|g| (g.preset, g.min_delivery_rate))
            .collect();
        assert_eq!(from_gates, lab_delivery_thresholds().to_vec());
        assert_eq!(from_gates, crate::lab::runner::ci_gates());
    }

    #[test]
    #[ignore = "slow local smoke: 10k nodes (~30-60s); run with --ignored"]
    fn dense_cluster_10k_smoke() {
        let config = preset_config(LabPreset::DenseCluster, 3, 10_000, false);
        assert!(config.node_count >= 10_000);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        assert!(
            report.delivery_rate > 0.0,
            "10k dense cluster delivery {:.1}%",
            report.delivery_rate * 100.0
        );
        assert_eq!(report.node_count, 10_000);
    }

    #[test]
    fn sybil_swarm_has_half_adversaries_in_ci() {
        let config = preset_config(LabPreset::SybilSwarm, 42, 200, true);
        assert!((config.malicious_fraction - 0.50).abs() < f64::EPSILON);
        assert!(config.malicious_drop_per_mille > 0);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        assert!(
            report.delivery_rate >= 0.35,
            "sybil_swarm delivery {:.1}%",
            report.delivery_rate * 100.0
        );
    }

    #[test]
    fn earthquake_internet_outage_and_node_loss() {
        let config = preset_config(LabPreset::Earthquake, 99, 150, true);
        assert_eq!(config.events.len(), 2);
        let mut runner = LabRunner::new(config);
        let report = runner.run();
        assert!(report.delivery_rate >= 0.50);
    }
}
