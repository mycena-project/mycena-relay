// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Per-node battery accounting by bearer type.

use crate::lab::radio::BearerKind;

#[derive(Debug, Clone, Default)]
pub struct BatteryTotals {
    pub ble_joules: f64,
    pub wifi_joules: f64,
    pub relay_joules: f64,
    pub idle_joules: f64,
}

impl BatteryTotals {
    pub fn total_joules(&self) -> f64 {
        self.ble_joules + self.wifi_joules + self.relay_joules + self.idle_joules
    }

    pub fn add_tx(&mut self, bearer: BearerKind, bytes: u64) {
        let j = bearer.tx_joules_per_byte() * bytes as f64;
        match bearer {
            BearerKind::Ble => self.ble_joules += j,
            BearerKind::Wifi => self.wifi_joules += j,
            BearerKind::Relay => self.relay_joules += j,
        }
    }

    pub fn add_idle(&mut self, bearers_active: &[BearerKind], dt_secs: f64) {
        for bearer in bearers_active {
            self.idle_joules += bearer.idle_joules_per_sec() * dt_secs;
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeBattery {
    pub capacity_joules: f64,
    pub remaining_joules: f64,
    pub active_bearers: Vec<BearerKind>,
}

impl NodeBattery {
    pub fn phone_default() -> Self {
        Self {
            capacity_joules: 15_000.0,
            remaining_joules: 15_000.0,
            active_bearers: Vec::new(),
        }
    }

    pub fn drain(&mut self, joules: f64) {
        self.remaining_joules = (self.remaining_joules - joules).max(0.0);
    }

    pub fn fraction_remaining(&self) -> f64 {
        if self.capacity_joules <= 0.0 {
            return 0.0;
        }
        self.remaining_joules / self.capacity_joules
    }
}
