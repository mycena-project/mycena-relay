// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! SOS reputation gating and multi-source crisis activation (SD-032 extension).
//!
//! Individual SOS posts remain locally prioritized ([`TrafficClass::Essential`]) even when
//! they do not count toward network-wide crisis mode. Global crisis auto-activation requires
//! several independent, non-Sybil-suspicious relay peers in the same geohash area.

use crate::ingest_guard::IngestGuard;
use crate::peer_trust::PeerTrust;
use mycena_core::crisis_mode::{geohash_nearby, CrisisAutoConfig};
use std::collections::{HashSet, VecDeque};

/// Per-peer SOS rate for established / trusted mesh peers.
pub const TRUSTED_SOS_RATE_PER_MINUTE: u32 = 6;
/// Stricter cap for young or otherwise Sybil-suspicious SOS relays.
pub const SUSPICIOUS_SOS_RATE_PER_MINUTE: u32 = 2;
/// Cold / unseen peers may relay at most one SOS per minute toward crisis ingest.
pub const COLD_SOS_RATE_PER_MINUTE: u32 = 1;

/// Tunables for SOS ingest reputation and crisis-source counting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SosSecurityConfig {
    /// Peers younger than this with no mutual contacts are Sybil-suspicious for SOS crisis counting.
    pub young_peer_max_age_ms: u64,
    /// Minimum age before a peer without mutual contacts may count toward crisis activation.
    pub min_crisis_source_age_ms: u64,
    pub max_spam_score_for_crisis: f32,
    pub min_reliability_for_crisis: f32,
}

impl Default for SosSecurityConfig {
    fn default() -> Self {
        Self {
            young_peer_max_age_ms: 2 * 3_600_000,
            min_crisis_source_age_ms: 24 * 3_600_000,
            max_spam_score_for_crisis: 0.18,
            min_reliability_for_crisis: 0.45,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SosIngestReject {
    RateLimit,
    ThrottledPeer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SosObservation {
    relay_peer: String,
    geohash: String,
    observed_ms: u64,
    counts_for_crisis: bool,
}

/// Sliding window of observed SOS relays for crisis auto-activation.
#[derive(Debug, Clone)]
pub struct SosCrisisTracker {
    auto_config: CrisisAutoConfig,
    security_config: SosSecurityConfig,
    observations: VecDeque<SosObservation>,
}

impl Default for SosCrisisTracker {
    fn default() -> Self {
        Self::new(CrisisAutoConfig::default(), SosSecurityConfig::default())
    }
}

impl SosCrisisTracker {
    pub fn new(auto_config: CrisisAutoConfig, security_config: SosSecurityConfig) -> Self {
        Self {
            auto_config,
            security_config,
            observations: VecDeque::new(),
        }
    }

    pub fn auto_config(&self) -> &CrisisAutoConfig {
        &self.auto_config
    }

    pub fn security_config(&self) -> &SosSecurityConfig {
        &self.security_config
    }

    pub fn record_observation(
        &mut self,
        relay_peer: &str,
        geohash: &str,
        trust: &PeerTrust,
        now_ms: u64,
    ) {
        self.prune(now_ms);
        let counts_for_crisis =
            is_sos_source_trusted_for_crisis(trust, now_ms, &self.security_config);
        self.observations.push_back(SosObservation {
            relay_peer: relay_peer.to_string(),
            geohash: geohash.trim().to_string(),
            observed_ms: now_ms,
            counts_for_crisis,
        });
    }

    /// Distinct trusted relay peers with SOS in the local geohash area within the activity window.
    pub fn trusted_independent_sources_nearby(&self, local_prefix: &str, now_ms: u64) -> usize {
        let window = self.auto_config.sos_activity_window_ms;
        let mut peers = HashSet::new();
        for obs in &self.observations {
            if now_ms.saturating_sub(obs.observed_ms) > window {
                continue;
            }
            if !obs.counts_for_crisis {
                continue;
            }
            if !geohash_nearby(
                local_prefix,
                &obs.geohash,
                self.auto_config.geohash_prefix_len,
            ) {
                continue;
            }
            peers.insert(obs.relay_peer.as_str());
        }
        peers.len()
    }

    fn prune(&mut self, now_ms: u64) {
        let window = self.auto_config.sos_activity_window_ms;
        while self
            .observations
            .front()
            .is_some_and(|o| now_ms.saturating_sub(o.observed_ms) > window)
        {
            self.observations.pop_front();
        }
    }
}

/// True when this peer may count toward crisis auto-activation (independent, non-Sybil-suspicious).
pub fn is_sos_source_trusted_for_crisis(
    trust: &PeerTrust,
    now_ms: u64,
    config: &SosSecurityConfig,
) -> bool {
    if is_sos_source_sybil_suspicious(trust, now_ms, config) {
        return false;
    }
    if trust.throttled_until_ms > now_ms || trust.strike_count > 0 {
        return false;
    }
    if trust.spam_score > config.max_spam_score_for_crisis {
        return false;
    }
    if trust.reliability < config.min_reliability_for_crisis {
        return false;
    }
    trust.mutual_contacts > 0 || trust.age_ms(now_ms) >= config.min_crisis_source_age_ms
}

/// Young / unproven peers with poor signals — may relay SOS locally but must not flip crisis mode.
pub fn is_sos_source_sybil_suspicious(
    trust: &PeerTrust,
    now_ms: u64,
    config: &SosSecurityConfig,
) -> bool {
    if trust.throttled_until_ms > now_ms || trust.strike_count > 0 {
        return true;
    }
    if trust.spam_score > config.max_spam_score_for_crisis {
        return true;
    }
    trust.mutual_contacts == 0 && trust.age_ms(now_ms) < config.young_peer_max_age_ms
}

/// Tiered SOS ingest rate based on relay reputation.
pub fn sos_rate_limit_for_peer(trust: &PeerTrust, now_ms: u64, config: &SosSecurityConfig) -> u32 {
    if is_sos_source_sybil_suspicious(trust, now_ms, config) {
        SUSPICIOUS_SOS_RATE_PER_MINUTE
    } else if is_sos_source_trusted_for_crisis(trust, now_ms, config) {
        TRUSTED_SOS_RATE_PER_MINUTE
    } else if trust.messages_accepted == 0 && trust.relay_success_count == 0 {
        COLD_SOS_RATE_PER_MINUTE
    } else {
        SUSPICIOUS_SOS_RATE_PER_MINUTE
    }
}

/// SOS ingest with reputation-tiered rate limits (geohash aggregate cap unchanged).
pub fn check_sos_ingest(
    guard: &mut IngestGuard,
    relay_peer: &str,
    geohash: Option<&str>,
    trust: &PeerTrust,
    now_ms: u64,
    config: &SosSecurityConfig,
) -> Result<(), SosIngestReject> {
    if trust.throttled_until_ms > now_ms {
        return Err(SosIngestReject::ThrottledPeer);
    }
    let limit = sos_rate_limit_for_peer(trust, now_ms, config);
    guard
        .check_sos_gossip_with_limit(relay_peer, geohash, limit, now_ms)
        .map_err(|_| SosIngestReject::RateLimit)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ingest_guard::IngestGuard;

    fn established_trust(now_ms: u64) -> PeerTrust {
        PeerTrust {
            first_seen_ms: now_ms.saturating_sub(30 * 86_400_000),
            reliability: 0.82,
            spam_score: 0.02,
            relay_success_rate: 0.80,
            relay_success_count: 80,
            relay_failure_count: 4,
            mutual_contacts: 2,
            messages_accepted: 200,
            messages_rejected: 1,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: Some("mesh-wifi".into()),
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    fn sybil_trust(now_ms: u64, idx: u64) -> PeerTrust {
        PeerTrust {
            first_seen_ms: now_ms.saturating_sub(40 * 60_000 + idx * 1000),
            reliability: 0.99,
            spam_score: 0.01,
            relay_success_rate: 0.99,
            relay_success_count: 400,
            relay_failure_count: 0,
            mutual_contacts: 0,
            messages_accepted: 1000,
            messages_rejected: 0,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: Some("sybil-local".into()),
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    #[test]
    fn sybil_sos_not_trusted_for_crisis() {
        let now = 1_700_000_000_000u64;
        let cfg = SosSecurityConfig::default();
        assert!(!is_sos_source_trusted_for_crisis(
            &sybil_trust(now, 0),
            now,
            &cfg
        ));
        assert!(is_sos_source_sybil_suspicious(
            &sybil_trust(now, 0),
            now,
            &cfg
        ));
    }

    #[test]
    fn crisis_requires_three_trusted_sources() {
        let now = 1_700_000_000_000u64;
        let mut tracker = SosCrisisTracker::default();
        let legit = established_trust(now);

        tracker.record_observation("L001", "u4pruyd", &legit, now);
        assert_eq!(tracker.trusted_independent_sources_nearby("u4pr", now), 1);

        for i in 0..200 {
            tracker.record_observation(
                &format!("S{i:03}"),
                "u4prxyz",
                &sybil_trust(now, i as u64),
                now + i as u64,
            );
        }
        assert_eq!(tracker.trusted_independent_sources_nearby("u4pr", now), 1);

        tracker.record_observation("L002", "u4prabc", &legit, now + 1);
        tracker.record_observation("L003", "u4prdef", &legit, now + 2);
        assert_eq!(
            tracker.trusted_independent_sources_nearby("u4pr", now + 2),
            3
        );
    }

    #[test]
    fn suspicious_sos_rate_limit_stricter() {
        let now = 5_000_000u64;
        let mut guard = IngestGuard::new();
        let cfg = SosSecurityConfig::default();
        let sybil = sybil_trust(now, 0);
        for i in 0..SUSPICIOUS_SOS_RATE_PER_MINUTE {
            assert!(
                check_sos_ingest(&mut guard, "sybil", Some("u4pru"), &sybil, now, &cfg).is_ok(),
                "i={i}"
            );
        }
        assert_eq!(
            check_sos_ingest(&mut guard, "sybil", Some("u4pru"), &sybil, now, &cfg),
            Err(SosIngestReject::RateLimit)
        );
    }
}
