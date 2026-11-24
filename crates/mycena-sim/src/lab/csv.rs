// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! CSV export for regression comparisons between commits.

use crate::lab::report::LabReport;
use std::io::Write;
use std::path::Path;

const HEADER: &str = "timestamp_ms,git_commit,scenario,seed,nodes,duration_ms,messages_attempted,delivery_rate,avg_latency_ms,p95_latency_ms,ble_joules,wifi_joules,relay_joules,idle_joules,memory_bytes,delivered,dropped_loss,dropped_offline";

pub fn format_csv_row(report: &LabReport) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "{ts},{},{},{},{},{},{},{:.4},{:.1},{:.1},{:.3},{:.3},{:.3},{:.3},{},{},{},{}",
        report.git_commit,
        report.scenario,
        report.seed,
        report.node_count,
        report.duration_ms,
        report.messages_attempted,
        report.delivery_rate,
        report.avg_latency_ms,
        report.p95_latency_ms,
        report.battery.ble_joules,
        report.battery.wifi_joules,
        report.battery.relay_joules,
        report.battery.idle_joules,
        report.memory_estimate_bytes,
        report.metrics.delivered,
        report.metrics.dropped_loss,
        report.metrics.dropped_offline,
    )
}

pub fn append_csv(path: &Path, report: &LabReport) -> anyhow::Result<()> {
    let needs_header = !path.exists();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if needs_header {
        writeln!(file, "{HEADER}")?;
    }
    writeln!(file, "{}", format_csv_row(report))?;
    Ok(())
}

pub fn write_csv(path: &Path, reports: &[LabReport]) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "{HEADER}")?;
    for report in reports {
        writeln!(file, "{}", format_csv_row(report))?;
    }
    Ok(())
}
