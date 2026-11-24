// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! **MycenaLab** — spatial mesh simulation with mobility, radio, battery, and adversaries.

pub mod battery;
pub mod csv;
pub mod gates;
pub mod mobility;
pub mod preset;
pub mod radio;
pub mod report;
pub mod rng;
pub mod runner;
pub mod transport_score;

pub use preset::LabPreset;
pub use report::LabReport;
pub use runner::{LabRunner, MobilityPreset, PresetConfig};
