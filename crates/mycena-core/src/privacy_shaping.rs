// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! SD-070: Envelope padding, outbound batching, scope bucketing, and cover-traffic helpers.
//!
//! **Default off** — see [`docs/PRIVACY_SHAPING_BENCHMARK.md`](../../docs/PRIVACY_SHAPING_BENCHMARK.md)
//! for measured bandwidth/battery overhead before enabling by default.

use crate::data::{Envelope, Priority};
use crate::energy::NodeState;
use crate::transport::DirectMessage;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Fixed wire-size buckets (bytes). Payload is padded up to the smallest bucket that fits.
pub const PADDING_BUCKETS: [usize; 4] = [256, 1024, 4096, 16_384];

const PAD_MAGIC: &[u8; 4] = b"MPAD";

/// Body marker for optional cover-traffic cells (dropped silently on ingest).
pub const COVER_BODY: &str = "[shade:cover:v1]";

/// User-facing trade-off profile (maps to batch interval bounds).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Default)]
pub enum PrivacyShapingProfile {
    /// Minimal delay (~0.5–2 s batch window).
    Fast,
    /// Default R&D starting point (~2–8 s).
    #[default]
    Balanced,
    /// Stronger timing resistance (~5–30 s).
    Paranoid,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PrivacyShapingConfig {
    /// Master switch — padding, batching, and scope bucketing apply when true.
    pub enabled: bool,
    pub profile: PrivacyShapingProfile,
    /// Lower bound of randomized batch flush window (milliseconds).
    pub batch_min_ms: u64,
    /// Upper bound of randomized batch flush window (milliseconds).
    pub batch_max_ms: u64,
    /// Pad `Envelope::payload` to the next size bucket before send.
    pub padding_enabled: bool,
    /// Collapse exact scope paths in `ScopeAnnounce` to parent wildcards.
    pub scope_bucketing_enabled: bool,
    /// Emit signed dummy direct messages during idle periods (energy-gated).
    pub cover_traffic_enabled: bool,
    /// Base interval between cover cells when [`NodeState::Active`].
    pub cover_interval_secs: u64,
}

impl Default for PrivacyShapingConfig {
    fn default() -> Self {
        Self::from_profile(PrivacyShapingProfile::Balanced)
    }
}

impl PrivacyShapingConfig {
    pub fn from_profile(profile: PrivacyShapingProfile) -> Self {
        let (batch_min_ms, batch_max_ms) = match profile {
            PrivacyShapingProfile::Fast => (500, 2_000),
            PrivacyShapingProfile::Balanced => (2_000, 8_000),
            PrivacyShapingProfile::Paranoid => (5_000, 30_000),
        };
        Self {
            enabled: false,
            profile,
            batch_min_ms,
            batch_max_ms,
            padding_enabled: true,
            scope_bucketing_enabled: true,
            cover_traffic_enabled: false,
            cover_interval_secs: 60,
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PrivacyShapingStats {
    pub messages_batched: u64,
    pub batches_flushed: u64,
    pub bytes_padded_sent: u64,
    pub bytes_padded_received: u64,
    pub cover_packets_sent: u64,
    pub cover_bytes_sent: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyShapingOverheadEstimate {
    pub padding_overhead_pct_typical: f64,
    pub batch_latency_ms_min: u64,
    pub batch_latency_ms_max: u64,
    pub cover_bytes_per_hour_active: u64,
    pub cover_bytes_per_hour_intermittent: u64,
    pub battery_impact_summary: String,
}

impl PrivacyShapingOverheadEstimate {
    pub fn from_config(config: &PrivacyShapingConfig) -> Self {
        let typical_payload = 180usize;
        let bucket = bucket_size(typical_payload + 8).unwrap_or(typical_payload);
        let padding_overhead_pct_typical = if config.padding_enabled {
            ((bucket.saturating_sub(typical_payload)) as f64 / typical_payload as f64) * 100.0
        } else {
            0.0
        };
        let cover_bucket = PADDING_BUCKETS[0];
        let cover_bytes_per_hour_active = if config.cover_traffic_enabled && config.enabled {
            let interval = config.cover_interval_secs.max(1);
            3600 / interval * cover_bucket as u64
        } else {
            0
        };
        let cover_bytes_per_hour_intermittent = if config.cover_traffic_enabled && config.enabled {
            let interval = config
                .cover_interval_secs
                .saturating_mul(NodeState::Intermittent.cover_traffic_interval_multiplier())
                .max(1);
            3600 / interval * cover_bucket as u64
        } else {
            0
        };
        let battery_impact_summary = if !config.enabled {
            "Off — no extra radio time.".to_string()
        } else if config.cover_traffic_enabled {
            format!(
                "Batching adds {}–{} ms latency; cover traffic ~{}–{} KiB/h on Active/Intermittent.",
                config.batch_min_ms,
                config.batch_max_ms,
                cover_bytes_per_hour_active / 1024,
                cover_bytes_per_hour_intermittent / 1024,
            )
        } else {
            format!(
                "Batching adds {}–{} ms latency; padding ~{:.0}% on typical chat payloads.",
                config.batch_min_ms, config.batch_max_ms, padding_overhead_pct_typical,
            )
        };
        Self {
            padding_overhead_pct_typical,
            batch_latency_ms_min: config.batch_min_ms,
            batch_latency_ms_max: config.batch_max_ms,
            cover_bytes_per_hour_active,
            cover_bytes_per_hour_intermittent,
            battery_impact_summary,
        }
    }
}

/// Smallest bucket that fits `len` bytes (including padding trailer).
pub fn bucket_size(len: usize) -> Option<usize> {
    PADDING_BUCKETS.iter().find(|&&b| len <= b).copied()
}

pub fn padding_overhead_bytes(original_len: usize) -> usize {
    let Some(bucket) = bucket_size(original_len + 8) else {
        return 0;
    };
    bucket.saturating_sub(original_len)
}

pub fn pad_payload(payload: &mut Vec<u8>) -> bool {
    let original_len = payload.len();
    let Some(target) = bucket_size(original_len + 8) else {
        return false;
    };
    let pad_needed = target.saturating_sub(original_len + 8);
    if pad_needed > 0 {
        let mut rng = rand::thread_rng();
        let mut pad = vec![0u8; pad_needed];
        rng.fill(&mut pad[..]);
        payload.extend_from_slice(&pad);
    }
    payload.extend_from_slice(PAD_MAGIC);
    payload.extend_from_slice(&(original_len as u32).to_le_bytes());
    true
}

pub fn unpad_payload(payload: &mut Vec<u8>) -> bool {
    let len = payload.len();
    if bucket_size(len) != Some(len) || len < 8 {
        return false;
    }
    if &payload[len - 8..len - 4] != PAD_MAGIC {
        return false;
    }
    let orig_len = match payload[len - 4..].try_into() {
        Ok(trailer) => u32::from_le_bytes(trailer) as usize,
        Err(_) => return false,
    };
    if orig_len + 8 > len {
        return false;
    }
    payload.truncate(orig_len);
    true
}

pub fn pad_direct_message(message: &mut DirectMessage) -> usize {
    let original_len = message.envelope.payload.len();
    if !pad_payload(&mut message.envelope.payload) {
        return 0;
    }
    message.envelope.payload.len().saturating_sub(original_len)
}

pub fn unpad_direct_message(message: &mut DirectMessage) -> bool {
    unpad_payload(&mut message.envelope.payload)
}

/// SD-004: announce parent wildcard instead of exact leaf scope when depth ≥ 3.
pub fn bucket_scope_for_announce(scope: &str) -> String {
    if scope.ends_with("/*") {
        return scope.to_string();
    }
    let parts: Vec<&str> = scope.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return scope.to_string();
    }
    let parent = parts[..parts.len() - 1].join("/");
    format!("{parent}/*")
}

pub fn bucket_scopes_for_announce(scopes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: HashSet<String> = HashSet::new();
    for scope in scopes {
        out.insert(bucket_scope_for_announce(&scope));
    }
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort();
    v
}

#[derive(Debug, Clone)]
struct BatchedItem {
    to_peer: String,
    message: DirectMessage,
}

#[derive(Debug, Default)]
pub struct BatchingScheduler {
    queue: VecDeque<BatchedItem>,
    flush_at: Option<Instant>,
}

impl BatchingScheduler {
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn has_deadline(&self) -> bool {
        self.flush_at.is_some()
    }

    pub fn push(&mut self, to_peer: String, message: DirectMessage) {
        self.queue.push_back(BatchedItem { to_peer, message });
    }

    pub fn reschedule_flush(&mut self, config: &PrivacyShapingConfig) {
        let min = config.batch_min_ms.max(1);
        let max = config.batch_max_ms.max(min);
        let delay_ms = if min == max {
            min
        } else {
            rand::thread_rng().gen_range(min..=max)
        };
        self.flush_at = Some(Instant::now() + Duration::from_millis(delay_ms));
    }

    pub fn is_due(&self, now: Instant) -> bool {
        self.flush_at.is_some_and(|t| now >= t)
    }

    pub fn drain(&mut self) -> Vec<(String, DirectMessage)> {
        self.flush_at = None;
        self.queue
            .drain(..)
            .map(|item| (item.to_peer, item.message))
            .collect()
    }
}

pub fn should_batch(priority: Priority, config: &PrivacyShapingConfig) -> bool {
    config.enabled && !matches!(priority, Priority::High)
}

pub fn cover_interval_for_state(
    config: &PrivacyShapingConfig,
    state: NodeState,
) -> Option<Duration> {
    if !config.enabled || !config.cover_traffic_enabled {
        return None;
    }
    if !state.allows_cover_traffic() {
        return None;
    }
    let mult = state.cover_traffic_interval_multiplier();
    if mult == 0 {
        return None;
    }
    Some(Duration::from_secs(
        config.cover_interval_secs.saturating_mul(mult).max(1),
    ))
}

pub fn build_cover_payload() -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let raw_len = rng.gen_range(8..64);
    let mut payload = vec![0u8; raw_len];
    rng.fill(&mut payload[..]);
    let _ = pad_payload(&mut payload);
    payload
}

pub fn build_cover_envelope(from_peer: &str, to_peer: &str) -> Envelope {
    Envelope::new(
        from_peer.to_string(),
        Some(to_peer.to_string()),
        build_cover_payload(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn pad_unpad_roundtrip() {
        for original in [1usize, 50, 200, 800, 3000, 10_000] {
            let mut payload = vec![7u8; original];
            assert!(pad_payload(&mut payload), "len={original}");
            assert_eq!(bucket_size(payload.len()), Some(payload.len()));
            assert!(unpad_payload(&mut payload));
            assert_eq!(payload.len(), original);
        }
    }

    #[test]
    fn unpadded_payload_unchanged() {
        let mut payload = b"hello".to_vec();
        assert!(!unpad_payload(&mut payload));
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn bucket_scope_collapses_deep_paths() {
        assert_eq!(
            bucket_scope_for_announce("mycena/bulletin/berlin/mitte"),
            "mycena/bulletin/berlin/*"
        );
        assert_eq!(bucket_scope_for_announce("mycena/chat"), "mycena/chat");
        assert_eq!(
            bucket_scope_for_announce("mycena/bulletin/*"),
            "mycena/bulletin/*"
        );
    }

    #[test]
    fn batching_scheduler_respects_window() {
        let config = PrivacyShapingConfig {
            enabled: true,
            batch_min_ms: 10,
            batch_max_ms: 10,
            ..PrivacyShapingConfig::default()
        };
        let mut sched = BatchingScheduler::default();
        sched.reschedule_flush(&config);
        assert!(!sched.is_due(Instant::now()));
        std::thread::sleep(Duration::from_millis(15));
        assert!(sched.is_due(Instant::now()));
    }

    /// Run with `cargo test privacy_shaping_benchmark -- --nocapture` to refresh numbers in
    /// `docs/PRIVACY_SHAPING_BENCHMARK.md`.
    #[test]
    fn privacy_shaping_benchmark() {
        const SAMPLES: usize = 10_000;
        let sizes = [64usize, 180, 512, 2048];
        let mut total_orig = 0usize;
        let mut total_padded = 0usize;
        let start = Instant::now();
        for &size in &sizes {
            for _ in 0..SAMPLES {
                let mut payload = vec![0u8; size];
                total_orig += payload.len();
                pad_payload(&mut payload);
                total_padded += payload.len();
                assert!(unpad_payload(&mut payload));
            }
        }
        let elapsed = start.elapsed();
        let overhead_pct =
            (total_padded.saturating_sub(total_orig) as f64 / total_orig as f64) * 100.0;
        let ops_per_sec = (SAMPLES * sizes.len()) as f64 / elapsed.as_secs_f64();
        println!("privacy_shaping_benchmark:");
        println!("  samples_per_size: {SAMPLES}");
        println!("  payload_sizes: {sizes:?}");
        println!("  total_original_bytes: {total_orig}");
        println!("  total_padded_bytes: {total_padded}");
        println!("  aggregate_padding_overhead_pct: {overhead_pct:.2}");
        println!("  pad_unpad_ops_per_sec: {ops_per_sec:.0}");
        let config = PrivacyShapingConfig {
            enabled: true,
            cover_traffic_enabled: true,
            cover_interval_secs: 60,
            ..PrivacyShapingConfig::default()
        };
        let est = PrivacyShapingOverheadEstimate::from_config(&config);
        println!(
            "  cover_bytes_per_hour_active: {}",
            est.cover_bytes_per_hour_active
        );
        println!(
            "  cover_bytes_per_hour_intermittent: {}",
            est.cover_bytes_per_hour_intermittent
        );
    }
}
