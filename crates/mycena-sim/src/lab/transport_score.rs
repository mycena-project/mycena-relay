// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Score-aware bearer selection for lab simulations.

use crate::lab::radio::{BearerKind, RadioConfig};
use mycena_core::transport_score::{
    decide_transport_path, TransportAvailability, TransportBearer, TransportPath,
    TransportScoreStore,
};

/// Inputs for score-aware lab bearer selection.
#[derive(Debug, Clone, Copy)]
pub struct LabBearerQuery<'a> {
    pub distance_m: f64,
    pub radio: &'a RadioConfig,
    pub a_online: bool,
    pub b_online: bool,
    pub a_internet: bool,
    pub b_internet: bool,
    pub same_cluster: bool,
    pub payload_bytes: usize,
    pub now: u64,
}

/// Pick bearer using the same fallback order as production nodes.
pub fn bearer_for_link_scored(
    query: &LabBearerQuery<'_>,
    scores: &TransportScoreStore,
) -> Option<BearerKind> {
    let LabBearerQuery {
        distance_m,
        radio,
        a_online,
        b_online,
        a_internet,
        b_internet,
        same_cluster,
        payload_bytes,
        now,
    } = query;
    if !a_online || !b_online || !same_cluster {
        return None;
    }
    let availability = TransportAvailability {
        internet: radio.internet_relay_available && *a_internet && *b_internet,
        wifi_direct: *distance_m <= radio.wifi_range_m,
        ble: *distance_m <= radio.ble_range_m,
        lora: false,
        wifi_aware: false,
        webrtc: false,
    };
    match decide_transport_path(&availability, scores, *payload_bytes, false, *now) {
        TransportPath::InternetRelay => Some(BearerKind::Relay),
        TransportPath::WebRtc => Some(BearerKind::Relay),
        TransportPath::WifiDirect => Some(BearerKind::Wifi),
        TransportPath::BleTrickle => Some(BearerKind::Ble),
        TransportPath::CustodyQueue => None,
    }
}

pub fn record_lab_transfer(
    scores: &mut TransportScoreStore,
    bearer: BearerKind,
    latency_ms: u64,
    bytes: u32,
    success: bool,
    at_ms: u64,
) {
    let tb = match bearer {
        BearerKind::Ble => TransportBearer::Ble,
        BearerKind::Wifi => TransportBearer::WifiDirect,
        BearerKind::Relay => TransportBearer::InternetRelay,
    };
    scores.record(
        tb,
        mycena_core::transport_score::TransportScoreSample {
            latency_ms,
            bytes,
            success,
            at_ms,
        },
    );
}

/// CI gate: production fallback order + score-based Wi‑Fi upgrade (see `verify_transport_score_invariants`).
pub fn verify_lab_transport_score_gates() -> anyhow::Result<()> {
    mycena_core::transport_score::verify_transport_score_invariants()
        .map_err(anyhow::Error::msg)?;

    let radio = RadioConfig::default();
    let scores = TransportScoreStore::default();
    let relay_q = LabBearerQuery {
        distance_m: 200.0,
        radio: &radio,
        a_online: true,
        b_online: true,
        a_internet: true,
        b_internet: true,
        same_cluster: true,
        payload_bytes: 4096,
        now: 0,
    };
    if bearer_for_link_scored(&relay_q, &scores) != Some(BearerKind::Relay) {
        anyhow::bail!("lab bearer gate: expected relay when both peers have internet");
    }
    let ble_q = LabBearerQuery {
        distance_m: 10.0,
        radio: &radio,
        a_online: true,
        b_online: true,
        a_internet: false,
        b_internet: false,
        same_cluster: true,
        payload_bytes: 512,
        now: 0,
    };
    if bearer_for_link_scored(&ble_q, &scores) != Some(BearerKind::Ble) {
        anyhow::bail!("lab bearer gate: expected BLE when offline and in range");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn scored_bearer_prefers_relay_when_internet_up() {
        let radio = RadioConfig::default();
        let scores = TransportScoreStore::default();
        let q = LabBearerQuery {
            distance_m: 200.0,
            radio: &radio,
            a_online: true,
            b_online: true,
            a_internet: true,
            b_internet: true,
            same_cluster: true,
            payload_bytes: 4096,
            now: 0,
        };
        let b = bearer_for_link_scored(&q, &scores);
        assert_eq!(b, Some(BearerKind::Relay));
    }

    #[test]
    fn scored_bearer_falls_back_to_ble_in_range() {
        let radio = RadioConfig::default();
        let scores = TransportScoreStore::default();
        let q = LabBearerQuery {
            distance_m: 10.0,
            radio: &radio,
            a_online: true,
            b_online: true,
            a_internet: false,
            b_internet: false,
            same_cluster: true,
            payload_bytes: 512,
            now: 0,
        };
        let b = bearer_for_link_scored(&q, &scores);
        assert_eq!(b, Some(BearerKind::Ble));
    }
}
