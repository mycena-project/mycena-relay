// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Documented MycenaLab CI delivery thresholds (single source of truth).

use crate::lab::preset::LabPreset;

/// Minimum delivery rate per preset (seed=42, `ci_mode`, ideal links).
/// Documented in `docs/MYCENALAB.md`.
pub fn lab_delivery_thresholds() -> &'static [(LabPreset, f64)] {
    &[
        (LabPreset::CityWalk, 0.85),
        (LabPreset::Stadium, 0.70),
        (LabPreset::Earthquake, 0.50),
        (LabPreset::NetworkSplit, 0.60),
        (LabPreset::DenseCluster, 0.55),
        (LabPreset::SybilSwarm, 0.35),
    ]
}
