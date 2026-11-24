// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Rolling transport quality scores for automatic bearer selection (no user settings).

use std::collections::{HashMap, VecDeque};

/// Physical / logical transport bearers (extensible for LoRa).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TransportBearer {
    InternetRelay = 0,
    WifiDirect = 1,
    Ble = 2,
    LoRa = 3,
    /// Wi‑Fi Aware / NSD discovery (small announcements only).
    WifiAware = 4,
    /// WebRTC RTCDataChannel (direct when ICE succeeds).
    WebRtc = 5,
}

impl TransportBearer {
    pub fn all() -> &'static [Self] {
        &[
            Self::InternetRelay,
            Self::WifiDirect,
            Self::Ble,
            Self::LoRa,
            Self::WifiAware,
            Self::WebRtc,
        ]
    }

    fn cold_start_prior(self) -> TransportScore {
        match self {
            Self::InternetRelay => TransportScore {
                avg_latency_ms: 60.0,
                packet_loss_rate: 0.002,
                avg_throughput_bps: 512.0 * 1024.0,
                energy_joules_per_byte: 1e-7,
                sample_count: 0,
                last_observed_ms: 0,
            },
            Self::WifiDirect => TransportScore {
                avg_latency_ms: 8.0,
                packet_loss_rate: 0.005,
                avg_throughput_bps: 256.0 * 1024.0,
                energy_joules_per_byte: 2e-7,
                sample_count: 0,
                last_observed_ms: 0,
            },
            Self::Ble => TransportScore {
                avg_latency_ms: 40.0,
                packet_loss_rate: 0.015,
                avg_throughput_bps: 8.0 * 1024.0,
                energy_joules_per_byte: 8e-7,
                sample_count: 0,
                last_observed_ms: 0,
            },
            Self::LoRa => TransportScore {
                avg_latency_ms: 800.0,
                packet_loss_rate: 0.05,
                avg_throughput_bps: 256.0,
                energy_joules_per_byte: 5e-9,
                sample_count: 0,
                last_observed_ms: 0,
            },
            Self::WifiAware => TransportScore {
                avg_latency_ms: 25.0,
                packet_loss_rate: 0.01,
                avg_throughput_bps: 4.0 * 1024.0,
                // Much lower than continuous BLE scanning at comparable range.
                energy_joules_per_byte: 1.5e-7,
                sample_count: 0,
                last_observed_ms: 0,
            },
            Self::WebRtc => TransportScore {
                avg_latency_ms: 15.0,
                packet_loss_rate: 0.008,
                avg_throughput_bps: 384.0 * 1024.0,
                energy_joules_per_byte: 3e-8,
                sample_count: 0,
                last_observed_ms: 0,
            },
        }
    }
}

/// One observed transmission on a bearer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportScoreSample {
    pub latency_ms: u64,
    pub bytes: u32,
    pub success: bool,
    pub at_ms: u64,
}

/// Rolling quality estimate for a single bearer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportScore {
    pub avg_latency_ms: f64,
    pub packet_loss_rate: f64,
    pub avg_throughput_bps: f64,
    pub energy_joules_per_byte: f64,
    pub sample_count: u32,
    pub last_observed_ms: u64,
}

/// Which path to use for an outbound direct message (automatic, no UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPath {
    /// libp2p relay / circuit when internet is reachable.
    InternetRelay,
    /// WebRTC data channel (ICE/STUN direct path for desktop/browser peers).
    WebRtc,
    /// Wi‑Fi Direct bulk upgrade while BLE stays up for discovery.
    WifiDirect,
    /// BLE GATT trickle (small frames, low duty).
    BleTrickle,
    /// No live path — enqueue DTN custody only.
    CustodyQueue,
}

/// What bearers are currently reachable toward a peer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportAvailability {
    pub internet: bool,
    pub wifi_direct: bool,
    pub ble: bool,
    pub lora: bool,
    pub wifi_aware: bool,
    /// Established RTCDataChannel toward peer (signaling already done via relay).
    pub webrtc: bool,
}

const DEFAULT_SAMPLE_WINDOW: usize = 32;
const EWMA_ALPHA: f64 = 0.25;

/// Device-local rolling averages from observed transfers.
#[derive(Debug, Clone)]
pub struct TransportScoreStore {
    scores: HashMap<TransportBearer, TransportScore>,
    recent: HashMap<TransportBearer, VecDeque<TransportScoreSample>>,
    max_samples: usize,
}

impl Default for TransportScoreStore {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLE_WINDOW)
    }
}

impl TransportScoreStore {
    pub fn new(max_samples: usize) -> Self {
        Self {
            scores: HashMap::new(),
            recent: HashMap::new(),
            max_samples: max_samples.max(4),
        }
    }

    pub fn score(&self, bearer: TransportBearer) -> TransportScore {
        self.scores
            .get(&bearer)
            .copied()
            .unwrap_or_else(|| bearer.cold_start_prior())
    }

    pub fn record(&mut self, bearer: TransportBearer, sample: TransportScoreSample) {
        let q = self.recent.entry(bearer).or_default();
        q.push_back(sample);
        while q.len() > self.max_samples {
            q.pop_front();
        }
        let updated = Self::aggregate_from_samples(bearer, q);
        self.scores.insert(bearer, updated);
    }

    fn aggregate_from_samples(
        bearer: TransportBearer,
        samples: &VecDeque<TransportScoreSample>,
    ) -> TransportScore {
        if samples.is_empty() {
            return bearer.cold_start_prior();
        }
        let mut prior = bearer.cold_start_prior();
        let mut loss_events = 0u32;
        let mut total = 0u32;
        let mut throughput_sum = 0.0f64;
        let mut throughput_n = 0u32;
        let mut last_at = 0u64;

        for s in samples {
            total += 1;
            if !s.success {
                loss_events += 1;
            }
            let lat = s.latency_ms.max(1) as f64;
            prior.avg_latency_ms = prior.avg_latency_ms * (1.0 - EWMA_ALPHA) + lat * EWMA_ALPHA;
            if s.bytes > 0 && s.latency_ms > 0 {
                let bps = s.bytes as f64 * 1000.0 / lat;
                throughput_sum += bps;
                throughput_n += 1;
            }
            let energy = bearer.cold_start_prior().energy_joules_per_byte * s.bytes as f64;
            prior.energy_joules_per_byte = prior.energy_joules_per_byte * (1.0 - EWMA_ALPHA)
                + (energy / s.bytes.max(1) as f64) * EWMA_ALPHA;
            last_at = last_at.max(s.at_ms);
        }

        prior.packet_loss_rate = loss_events as f64 / total as f64;
        if throughput_n > 0 {
            let mean = throughput_sum / f64::from(throughput_n);
            prior.avg_throughput_bps =
                prior.avg_throughput_bps * (1.0 - EWMA_ALPHA) + mean * EWMA_ALPHA;
        }
        prior.sample_count = total;
        prior.last_observed_ms = last_at;
        prior
    }

    /// Combined suitability score (0..1, higher is better) for a payload size.
    pub fn suitability(&self, bearer: TransportBearer, payload_bytes: usize) -> f64 {
        let s = self.score(bearer);
        let payload = payload_bytes.max(1) as f64;
        let send_secs = payload / s.avg_throughput_bps.max(1.0);
        let throughput_norm = (1.0 / (1.0 + send_secs)).clamp(0.0, 1.0);
        let latency_norm = (1.0 / (1.0 + s.avg_latency_ms / 500.0)).clamp(0.0, 1.0);
        let loss_norm = (1.0 - s.packet_loss_rate).clamp(0.0, 1.0);
        let energy_norm =
            (1.0 / (1.0 + s.energy_joules_per_byte * payload * 1_000_000.0)).clamp(0.0, 1.0);
        (0.35 * throughput_norm + 0.25 * latency_norm + 0.25 * loss_norm + 0.15 * energy_norm)
            .clamp(0.0, 1.0)
    }

    pub fn prefer_wifi_over_ble(&self, payload_bytes: usize, force_bulk: bool, now: u64) -> bool {
        if force_bulk {
            return true;
        }
        let wifi = self.suitability(TransportBearer::WifiDirect, payload_bytes);
        let ble = self.suitability(TransportBearer::Ble, payload_bytes);
        let wifi_score = wifi + recency_bonus(self.score(TransportBearer::WifiDirect), now);
        let mut ble_score = ble + recency_bonus(self.score(TransportBearer::Ble), now);
        if payload_bytes <= 4096 {
            ble_score += 0.12;
        }
        wifi_score > ble_score
    }

    pub fn best_available_mesh_bearer(
        &self,
        availability: &TransportAvailability,
        payload_bytes: usize,
        force_bulk: bool,
        now: u64,
    ) -> Option<TransportBearer> {
        let mut candidates: Vec<(TransportBearer, f64)> = Vec::new();
        if availability.wifi_direct {
            let mut s = self.suitability(TransportBearer::WifiDirect, payload_bytes);
            s += recency_bonus(self.score(TransportBearer::WifiDirect), now);
            if force_bulk {
                s += 0.2;
            }
            candidates.push((TransportBearer::WifiDirect, s));
        }
        if availability.ble {
            let mut s = self.suitability(TransportBearer::Ble, payload_bytes);
            s += recency_bonus(self.score(TransportBearer::Ble), now);
            candidates.push((TransportBearer::Ble, s));
        }
        if availability.lora {
            let mut s = self.suitability(TransportBearer::LoRa, payload_bytes);
            s += recency_bonus(self.score(TransportBearer::LoRa), now);
            candidates.push((TransportBearer::LoRa, s));
        }
        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| (a.0 as u8).cmp(&(b.0 as u8)))
        });
        candidates.first().map(|(b, _)| *b)
    }

    pub fn estimated_latency_ms(&self, bearer: TransportBearer, payload_bytes: usize) -> u64 {
        let s = self.score(bearer);
        let transfer_ms = (payload_bytes as f64 / s.avg_throughput_bps.max(1.0) * 1000.0) as u64;
        s.avg_latency_ms as u64 + transfer_ms
    }
}

fn recency_bonus(score: TransportScore, now: u64) -> f64 {
    if score.last_observed_ms == 0 || now <= score.last_observed_ms {
        return 0.0;
    }
    let age_ms = now.saturating_sub(score.last_observed_ms);
    if age_ms > 7 * 24 * 60 * 60 * 1000 {
        return 0.0;
    }
    (0.05 * (1.0 - age_ms as f64 / (7.0 * 24.0 * 60.0 * 60.0 * 1000.0))).clamp(0.0, 0.05)
}

/// Automatic path selection: WebRTC → Internet → Wi‑Fi Direct → BLE trickle → custody queue.
pub fn decide_transport_path(
    availability: &TransportAvailability,
    scores: &TransportScoreStore,
    payload_bytes: usize,
    force_bulk: bool,
    now: u64,
) -> TransportPath {
    if availability.webrtc {
        return TransportPath::WebRtc;
    }
    if availability.internet {
        return TransportPath::InternetRelay;
    }
    if availability.wifi_direct && scores.prefer_wifi_over_ble(payload_bytes, force_bulk, now) {
        return TransportPath::WifiDirect;
    }
    if availability.ble {
        return TransportPath::BleTrickle;
    }
    if availability.lora {
        return TransportPath::BleTrickle;
    }
    TransportPath::CustodyQueue
}

/// Discovery bearer selection (separate from data-path [`decide_transport_path`]).
pub fn decide_discovery_bearer(
    wifi_aware_available: bool,
    ble_available: bool,
    scores: &TransportScoreStore,
    now: u64,
) -> Option<TransportBearer> {
    let mut candidates: Vec<(TransportBearer, f64)> = Vec::new();
    if wifi_aware_available {
        let mut s = scores.suitability(TransportBearer::WifiAware, 256);
        s += recency_bonus(scores.score(TransportBearer::WifiAware), now);
        s += 0.18;
        candidates.push((TransportBearer::WifiAware, s));
    }
    if ble_available {
        let mut s = scores.suitability(TransportBearer::Ble, 256);
        s += recency_bonus(scores.score(TransportBearer::Ble), now);
        candidates.push((TransportBearer::Ble, s));
    }
    candidates.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (a.0 as u8).cmp(&(b.0 as u8)))
    });
    candidates.first().map(|(b, _)| *b)
}

pub fn prefer_wifi_aware_for_discovery(
    wifi_aware_available: bool,
    ble_available: bool,
    scores: &TransportScoreStore,
    now: u64,
) -> bool {
    decide_discovery_bearer(wifi_aware_available, ble_available, scores, now)
        == Some(TransportBearer::WifiAware)
}

/// Regression invariants shared by unit tests and mycena-sim CI gates.
pub fn verify_transport_score_invariants() -> Result<(), String> {
    let store = TransportScoreStore::default();
    let now = 1_700_000_000_000u64;
    let all_up = TransportAvailability {
        internet: true,
        wifi_direct: true,
        ble: true,
        lora: false,
        wifi_aware: false,
        webrtc: false,
    };
    if decide_transport_path(&all_up, &store, 4096, false, now) != TransportPath::InternetRelay {
        return Err("internet should win when relay is available".into());
    }
    let webrtc_up = TransportAvailability {
        internet: true,
        wifi_direct: true,
        ble: true,
        lora: false,
        wifi_aware: false,
        webrtc: true,
    };
    if decide_transport_path(&webrtc_up, &store, 4096, false, now) != TransportPath::WebRtc {
        return Err("WebRTC should bypass relay when data channel is ready".into());
    }
    let mesh_only = TransportAvailability {
        internet: false,
        wifi_direct: true,
        ble: true,
        lora: false,
        wifi_aware: false,
        webrtc: false,
    };
    if decide_transport_path(&mesh_only, &store, 64 * 1024, false, now) != TransportPath::WifiDirect
    {
        return Err("Wi-Fi Direct expected for bulk when internet is down".into());
    }
    if decide_transport_path(
        &TransportAvailability {
            internet: false,
            wifi_direct: false,
            ble: true,
            lora: false,
            wifi_aware: false,
            webrtc: false,
        },
        &store,
        512,
        false,
        now,
    ) != TransportPath::BleTrickle
    {
        return Err("BLE trickle expected when only BLE is in range".into());
    }
    if decide_transport_path(
        &TransportAvailability {
            internet: false,
            wifi_direct: false,
            ble: false,
            lora: false,
            wifi_aware: false,
            webrtc: false,
        },
        &store,
        512,
        false,
        now,
    ) != TransportPath::CustodyQueue
    {
        return Err("custody queue expected when no live bearer is available".into());
    }

    let mut observed = TransportScoreStore::new(16);
    let base = now;
    for i in 0..6 {
        observed.record(
            TransportBearer::WifiDirect,
            TransportScoreSample {
                latency_ms: 10,
                bytes: 32 * 1024,
                success: true,
                at_ms: base + i,
            },
        );
        observed.record(
            TransportBearer::Ble,
            TransportScoreSample {
                latency_ms: 120,
                bytes: 512,
                success: true,
                at_ms: base + i,
            },
        );
    }
    if !observed.prefer_wifi_over_ble(32 * 1024, false, base + 100) {
        return Err("observed Wi-Fi scores should beat BLE for bulk payloads".into());
    }
    Ok(())
}

pub fn bearer_medium_to_transport(
    medium: super::bearer_transport::BearerMedium,
) -> TransportBearer {
    match medium {
        super::bearer_transport::BearerMedium::Ble => TransportBearer::Ble,
        super::bearer_transport::BearerMedium::WifiDirect => TransportBearer::WifiDirect,
        super::bearer_transport::BearerMedium::WifiAware => TransportBearer::WifiAware,
        super::bearer_transport::BearerMedium::WebRtc => TransportBearer::WebRtc,
    }
}

pub fn transport_bearer_to_medium(
    bearer: TransportBearer,
) -> Option<super::bearer_transport::BearerMedium> {
    match bearer {
        TransportBearer::Ble => Some(super::bearer_transport::BearerMedium::Ble),
        TransportBearer::WifiDirect => Some(super::bearer_transport::BearerMedium::WifiDirect),
        TransportBearer::WifiAware => Some(super::bearer_transport::BearerMedium::WifiAware),
        TransportBearer::WebRtc => Some(super::bearer_transport::BearerMedium::WebRtc),
        TransportBearer::InternetRelay | TransportBearer::LoRa => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn sample(at: u64, latency: u64, bytes: u32, ok: bool) -> TransportScoreSample {
        TransportScoreSample {
            latency_ms: latency,
            bytes,
            success: ok,
            at_ms: at,
        }
    }

    #[test]
    fn rolling_average_tracks_loss_and_throughput() {
        let mut store = TransportScoreStore::new(16);
        let base = 1_700_000_000_000u64;
        for i in 0..8 {
            store.record(
                TransportBearer::Ble,
                sample(base + i * 1000, 80, 512, i % 5 != 0),
            );
        }
        let s = store.score(TransportBearer::Ble);
        assert!(s.packet_loss_rate > 0.0);
        assert!(s.sample_count == 8);
        assert!(s.avg_throughput_bps > 0.0);
    }

    #[test]
    fn fallback_order_internet_wifi_ble_custody() {
        verify_transport_score_invariants().expect("transport score invariants");
    }

    #[test]
    fn wifi_preferred_for_bulk_after_observations() {
        let mut store = TransportScoreStore::new(16);
        let base = 1_700_000_000_000u64;
        for i in 0..6 {
            store.record(
                TransportBearer::WifiDirect,
                sample(base + i, 10, 32 * 1024, true),
            );
            store.record(TransportBearer::Ble, sample(base + i, 120, 512, true));
        }
        assert!(store.prefer_wifi_over_ble(32 * 1024, false, base + 100));
        assert!(!store.prefer_wifi_over_ble(256, false, base + 100));
    }

    #[test]
    fn wifi_aware_preferred_for_discovery_when_available() {
        let store = TransportScoreStore::default();
        let now = 1_700_000_000_000u64;
        assert!(prefer_wifi_aware_for_discovery(true, true, &store, now));
        assert!(!prefer_wifi_aware_for_discovery(false, true, &store, now));
    }

    #[test]
    fn force_bulk_prefers_wifi_when_available() {
        let store = TransportScoreStore::default();
        let path = decide_transport_path(
            &TransportAvailability {
                internet: false,
                wifi_direct: true,
                ble: true,
                lora: false,
                wifi_aware: false,
                webrtc: false,
            },
            &store,
            64,
            true,
            0,
        );
        assert_eq!(path, TransportPath::WifiDirect);
    }
}
