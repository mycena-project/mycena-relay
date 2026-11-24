// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Realistic BLE / Wi‑Fi / relay radio models and link profiles.

use crate::scheduler::LinkProfile;

/// Typical urban ranges (metres).
#[derive(Debug, Clone, Copy)]
pub struct RadioConfig {
    pub ble_range_m: f64,
    pub wifi_range_m: f64,
    pub internet_relay_available: bool,
}

impl Default for RadioConfig {
    fn default() -> Self {
        Self {
            ble_range_m: 35.0,
            wifi_range_m: 120.0,
            internet_relay_available: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum BearerKind {
    Ble,
    Wifi,
    Relay,
}

impl BearerKind {
    pub fn link_profile(self) -> LinkProfile {
        match self {
            Self::Ble => LinkProfile {
                latency_ms: 40,
                loss_per_mille: 15,
                bandwidth_bytes_per_sec: 8 * 1024,
            },
            Self::Wifi => LinkProfile {
                latency_ms: 8,
                loss_per_mille: 5,
                bandwidth_bytes_per_sec: 256 * 1024,
            },
            Self::Relay => LinkProfile {
                latency_ms: 60,
                loss_per_mille: 2,
                bandwidth_bytes_per_sec: 512 * 1024,
            },
        }
    }

    pub fn tx_joules_per_byte(self) -> f64 {
        match self {
            Self::Ble => 8e-7,
            Self::Wifi => 2e-7,
            Self::Relay => 1e-7,
        }
    }

    pub fn idle_joules_per_sec(self) -> f64 {
        match self {
            Self::Ble => 0.015,
            Self::Wifi => 0.05,
            Self::Relay => 0.02,
        }
    }
}

/// Legacy distance-only bearer pick (BLE → Wi‑Fi → relay).
///
/// Prefer [`crate::lab::transport_score::bearer_for_link_scored`] in MycenaLab;
/// production nodes use [`mycena_core::transport_score::decide_transport_path`].
#[deprecated(
    note = "use lab/transport_score::bearer_for_link_scored or core transport_score in new code"
)]
pub fn bearer_for_link(
    distance_m: f64,
    radio: &RadioConfig,
    a_online: bool,
    b_online: bool,
    a_internet: bool,
    b_internet: bool,
    same_cluster: bool,
) -> Option<BearerKind> {
    if !a_online || !b_online || !same_cluster {
        return None;
    }
    if distance_m <= radio.ble_range_m {
        return Some(BearerKind::Ble);
    }
    if distance_m <= radio.wifi_range_m {
        return Some(BearerKind::Wifi);
    }
    if radio.internet_relay_available && a_internet && b_internet {
        return Some(BearerKind::Relay);
    }
    None
}
