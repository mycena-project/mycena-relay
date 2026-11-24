// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! MycenaLab CLI — spatial mesh scenario runner with CSV export.

use clap::{Parser, Subcommand};
use mycena_sim::lab::csv::{append_csv, write_csv};
use mycena_sim::lab::preset::{preset_config, LabPreset};
use mycena_sim::lab::runner::LabRunner;
use mycena_sim::run_all_sim_ci_gates;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mycenalab", about = "MycenaLab spatial mesh simulation")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a scenario preset.
    Run {
        #[arg(long, value_name = "PRESET")]
        preset: String,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 0)]
        nodes: usize,
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Use shortened duration for quick local runs.
        #[arg(long)]
        ci_mode: bool,
    },
    /// Run all CI regression gates and optionally append CSV.
    CiGate {
        #[arg(long)]
        csv: Option<PathBuf>,
    },
    /// List available presets.
    List,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run {
            preset,
            seed,
            nodes,
            csv,
            ci_mode,
        } => {
            let p = LabPreset::parse_name(&preset)
                .ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;
            let config = preset_config(p, seed, nodes, ci_mode);
            let node_count = config.node_count;
            let mut runner = LabRunner::new(config);
            let report = runner.run();
            print_report(&report);
            if let Some(path) = csv {
                append_csv(&path, &report)?;
                eprintln!("appended CSV → {}", path.display());
            }
            eprintln!("nodes={node_count} duration_ms={}", report.duration_ms);
        }
        Commands::CiGate { csv } => {
            let reports = run_all_sim_ci_gates()?;
            for report in &reports {
                print_report(report);
            }
            if let Some(path) = csv {
                write_csv(&path, &reports)?;
                eprintln!("wrote CSV → {}", path.display());
            }
            eprintln!("all {} CI gates passed", reports.len());
        }
        Commands::List => {
            for p in LabPreset::all() {
                println!("{} ({})", p.name(), p.german_alias());
            }
        }
    }
    Ok(())
}

fn print_report(r: &mycena_sim::lab::LabReport) {
    println!(
        "{} seed={} nodes={} delivery={:.1}% latency_avg={:.0}ms latency_p95={:.0}ms memory={}KiB battery={:.2}J",
        r.scenario,
        r.seed,
        r.node_count,
        r.delivery_rate * 100.0,
        r.avg_latency_ms,
        r.p95_latency_ms,
        r.memory_estimate_bytes / 1024,
        r.battery.total_joules(),
    );
}
