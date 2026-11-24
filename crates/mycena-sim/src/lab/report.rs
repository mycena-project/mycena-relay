// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Lab scenario output metrics.

use crate::lab::battery::BatteryTotals;
use crate::runner::SimulationMetrics;

#[derive(Debug, Clone)]
pub struct LabReport {
    pub scenario: String,
    pub seed: u64,
    pub node_count: usize,
    pub duration_ms: u64,
    pub messages_attempted: u64,
    pub delivery_rate: f64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub battery: BatteryTotals,
    pub memory_estimate_bytes: u64,
    pub metrics: SimulationMetrics,
    pub git_commit: String,
}

#[derive(Debug, Clone)]
pub struct LabRunSummary {
    pub scenario: String,
    pub seed: u64,
    pub node_count: usize,
    pub duration_ms: u64,
    pub attempted: u64,
    pub metrics: SimulationMetrics,
    pub battery: BatteryTotals,
    pub memory_estimate_bytes: u64,
}

impl LabReport {
    pub fn from_run(input: LabRunSummary) -> Self {
        let LabRunSummary {
            scenario,
            seed,
            node_count,
            duration_ms,
            attempted,
            metrics,
            battery,
            memory_estimate_bytes,
        } = input;
        let delivery_rate = if attempted == 0 {
            0.0
        } else {
            metrics.delivered as f64 / attempted as f64
        };
        let avg_latency_ms = if metrics.latencies_ms.is_empty() {
            0.0
        } else {
            metrics.latencies_ms.iter().sum::<u64>() as f64 / metrics.latencies_ms.len() as f64
        };
        let mut sorted = metrics.latencies_ms.clone();
        sorted.sort_unstable();
        let p95_latency_ms = if sorted.is_empty() {
            0.0
        } else {
            let idx = ((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1);
            sorted[idx] as f64
        };
        Self {
            scenario,
            seed,
            node_count,
            duration_ms,
            messages_attempted: attempted,
            delivery_rate,
            avg_latency_ms,
            p95_latency_ms,
            battery,
            memory_estimate_bytes,
            metrics,
            git_commit: git_commit_short(),
        }
    }
}

pub fn git_commit_short() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
