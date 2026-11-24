// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! **InformationTrust** — multi-source corroboration for mesh-propagated fact claims.
//!
//! Applies to resource-map pins and SOS/crisis messages. A claim is marked confirmed
//! only after several independent, non-Sybil-suspicious sources report the same content
//! at the same location. Conflicting reports in one geo cell reduce trust for all variants.

use crate::resource_map::ResourceCategory;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Kind of mesh-propagated fact claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    Resource,
    Sos,
}

/// Corroboration lifecycle for a single fact claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorroborationStatus {
    /// Single source or all sources Sybil-suspicious.
    Unverified,
    /// Multiple sources but below confirmation threshold or weak diversity.
    Partial,
    /// Enough trusted, diverse, independent sources agree.
    Confirmed,
    /// Other contradictory claims exist at the same geo cell.
    Conflicting,
}

impl CorroborationStatus {
    /// Human-readable label for UI ("1 source, unverified" / "5 independent sources, confirmed").
    pub fn display_label(self, corroboration_count: usize, trusted_count: usize) -> String {
        match self {
            Self::Unverified => format!("{corroboration_count} source(s), unverified"),
            Self::Partial => {
                format!("{trusted_count} independent source(s), partially verified")
            }
            Self::Confirmed => format!("{trusted_count} independent sources, confirmed"),
            Self::Conflicting => format!("{corroboration_count} sources, conflicting reports"),
        }
    }
}

/// Tunables for corroboration scoring (aligned with [`crate::crisis_mode::CrisisAutoConfig`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InformationTrustConfig {
    /// Distinct trusted relay peers required before a claim is confirmed.
    pub min_trusted_sources_for_confirmed: usize,
    /// Sliding window for counting corroborating observations (ms).
    pub observation_window_ms: u64,
    /// Geohash prefix length for geo-cell conflict detection.
    pub geohash_cell_prefix_len: usize,
    /// Social diversity bucket width (ms) — aligned with mesh peer diversity tuning.
    pub social_window_ms: u64,
}

impl Default for InformationTrustConfig {
    fn default() -> Self {
        Self {
            min_trusted_sources_for_confirmed: 3,
            observation_window_ms: 15 * 60 * 1000,
            geohash_cell_prefix_len: 4,
            social_window_ms: 30 * 60 * 1000,
        }
    }
}

/// Aggregated trust metrics for one fact claim.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InformationTrust {
    pub corroboration_count: usize,
    pub trusted_corroboration_count: usize,
    /// 0–1 transport + social spread among corroborating peers.
    pub source_diversity: f32,
    /// 1.0 when no conflicting claims in the geo cell; lower when contradictions exist.
    pub geographic_consistency: f32,
    /// 1.0 when fresh; decays toward expiry / observation window.
    pub staleness_factor: f32,
    /// Composite 0–1 score for sorting and display emphasis.
    pub trust_score: f32,
    pub status: CorroborationStatus,
}

impl InformationTrust {
    pub fn display_label(&self) -> String {
        self.status
            .display_label(self.corroboration_count, self.trusted_corroboration_count)
    }
}

/// One relay observation of a fact claim (input to trust computation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceObservation {
    pub relay_peer: String,
    pub observed_at_ms: u64,
    pub trusted_independent: bool,
    pub transport_hint: String,
    pub first_encounter_ms: u64,
    /// Coarse geohash bucket for geographic corroboration diversity.
    #[serde(default)]
    pub geohash_bucket: Option<String>,
}

/// Stable key identifying a specific claim variant at a location.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FactClaimKey {
    pub geohash: String,
    pub kind: FactKind,
    pub category: Option<ResourceCategory>,
    pub content_hash: u64,
}

impl FactClaimKey {
    pub fn resource(geohash: &str, category: ResourceCategory, description: &str) -> Self {
        Self {
            geohash: geohash.trim().to_string(),
            kind: FactKind::Resource,
            category: Some(category),
            content_hash: claim_content_hash(description),
        }
    }

    pub fn sos(geohash: &str, message: &str) -> Self {
        Self {
            geohash: geohash.trim().to_string(),
            kind: FactKind::Sos,
            category: None,
            content_hash: claim_content_hash(message),
        }
    }

    pub fn geo_cell(&self, prefix_len: usize) -> String {
        geo_cell_prefix(&self.geohash, prefix_len)
    }
}

/// Normalize claim text and hash for stable deduplication.
pub fn claim_content_hash(text: &str) -> u64 {
    let normalized = normalize_claim_text(text);
    let mut hash = 0xcbf29ce484222325u64;
    for b in normalized.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn normalize_claim_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn geo_cell_prefix(geohash: &str, prefix_len: usize) -> String {
    let gh = geohash.trim();
    if prefix_len == 0 || gh.is_empty() {
        return gh.to_string();
    }
    let n = prefix_len.min(gh.len());
    gh[..n].to_string()
}

fn compute_source_diversity(observations: &[SourceObservation], social_window_ms: u64) -> f32 {
    if observations.is_empty() {
        return 0.0;
    }
    let transports: HashSet<&str> = observations
        .iter()
        .map(|o| o.transport_hint.as_str())
        .collect();
    let social_buckets: HashSet<u64> = observations
        .iter()
        .map(|o| {
            if social_window_ms == 0 {
                o.first_encounter_ms
            } else {
                o.first_encounter_ms
                    .checked_div(social_window_ms)
                    .unwrap_or(0)
            }
        })
        .collect();
    let n = observations.len() as f32;
    let transport_div = transports.len() as f32 / n;
    let social_div = social_buckets.len() as f32 / n;
    (0.6 * transport_div + 0.4 * social_div).min(1.0)
}

fn compute_staleness_factor(
    newest_observation_ms: u64,
    now_ms: u64,
    ttl_secs: u64,
    observation_window_ms: u64,
) -> f32 {
    let age_ms = now_ms.saturating_sub(newest_observation_ms);
    let ttl_ms = ttl_secs.saturating_mul(1000);
    let window = observation_window_ms.max(1);
    let ttl_factor = if ttl_ms == 0 {
        1.0
    } else {
        1.0 - (age_ms as f32 / ttl_ms as f32).min(1.0)
    };
    let window_factor = 1.0 - (age_ms as f32 / window as f32).min(1.0);
    (0.5 * ttl_factor + 0.5 * window_factor).clamp(0.0, 1.0)
}

/// Compute trust for one claim given its observations and geo-cell conflict count.
pub fn compute_information_trust(
    claim_observations: &[SourceObservation],
    conflicting_claims_in_cell: usize,
    now_ms: u64,
    ttl_secs: u64,
    config: &InformationTrustConfig,
) -> InformationTrust {
    compute_information_trust_with_diversity(
        claim_observations,
        conflicting_claims_in_cell,
        now_ms,
        ttl_secs,
        config,
        None,
    )
}

/// Like [`compute_information_trust`] but accepts a precomputed diversity score
/// (e.g. from [`PeerDiversityConfig`] at the node layer).
pub fn compute_information_trust_with_diversity(
    claim_observations: &[SourceObservation],
    conflicting_claims_in_cell: usize,
    now_ms: u64,
    ttl_secs: u64,
    config: &InformationTrustConfig,
    source_diversity_override: Option<f32>,
) -> InformationTrust {
    let window = config.observation_window_ms;
    let recent: Vec<&SourceObservation> = claim_observations
        .iter()
        .filter(|o| now_ms.saturating_sub(o.observed_at_ms) <= window)
        .collect();

    let mut distinct_peers = HashSet::new();
    let mut trusted_peers = HashSet::new();
    let mut trusted_obs = Vec::new();
    let mut newest_ms = 0u64;

    for obs in &recent {
        distinct_peers.insert(obs.relay_peer.as_str());
        if obs.observed_at_ms > newest_ms {
            newest_ms = obs.observed_at_ms;
        }
        if obs.trusted_independent && trusted_peers.insert(obs.relay_peer.as_str()) {
            trusted_obs.push((*obs).clone());
        }
    }

    let corroboration_count = distinct_peers.len();
    let trusted_corroboration_count = trusted_peers.len();
    let source_diversity = source_diversity_override
        .unwrap_or_else(|| compute_source_diversity(&trusted_obs, config.social_window_ms));

    let geographic_consistency = if conflicting_claims_in_cell <= 1 {
        1.0
    } else {
        (1.0 / conflicting_claims_in_cell as f32).clamp(0.15, 1.0)
    };

    let staleness_factor = if newest_ms == 0 {
        0.0
    } else {
        compute_staleness_factor(newest_ms, now_ms, ttl_secs, window)
    };

    let trusted_ratio = if config.min_trusted_sources_for_confirmed == 0 {
        1.0
    } else {
        (trusted_corroboration_count as f32 / config.min_trusted_sources_for_confirmed as f32)
            .min(1.0)
    };

    let mut trust_score = 0.35 * trusted_ratio
        + 0.25 * source_diversity
        + 0.25 * geographic_consistency
        + 0.15 * staleness_factor;

    let status = if conflicting_claims_in_cell > 1 {
        trust_score *= geographic_consistency;
        CorroborationStatus::Conflicting
    } else if trusted_corroboration_count >= config.min_trusted_sources_for_confirmed
        && source_diversity >= 0.33
    {
        CorroborationStatus::Confirmed
    } else if trusted_corroboration_count >= 1 {
        CorroborationStatus::Partial
    } else {
        CorroborationStatus::Unverified
    };

    if matches!(status, CorroborationStatus::Unverified) {
        trust_score = trust_score.min(0.35);
    }

    InformationTrust {
        corroboration_count,
        trusted_corroboration_count,
        source_diversity,
        geographic_consistency,
        staleness_factor,
        trust_score: trust_score.clamp(0.0, 1.0),
        status,
    }
}

/// Count distinct claim variants in each geo cell (for conflict detection).
pub fn count_conflicting_claims_in_cell(
    cell_observations: &HashMap<FactClaimKey, Vec<SourceObservation>>,
    target: &FactClaimKey,
    prefix_len: usize,
    now_ms: u64,
    window_ms: u64,
) -> usize {
    let cell = target.geo_cell(prefix_len);
    let mut variants = HashSet::new();
    for (key, obs) in cell_observations {
        if key.kind != target.kind {
            continue;
        }
        if key.category != target.category {
            continue;
        }
        if key.geo_cell(prefix_len) != cell {
            continue;
        }
        let has_recent = obs
            .iter()
            .any(|o| now_ms.saturating_sub(o.observed_at_ms) <= window_ms);
        if has_recent {
            variants.insert(key.content_hash);
        }
    }
    variants.len().max(1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn obs(peer: &str, trusted: bool, transport: &str, at_ms: u64) -> SourceObservation {
        SourceObservation {
            relay_peer: peer.to_string(),
            observed_at_ms: at_ms,
            trusted_independent: trusted,
            transport_hint: transport.to_string(),
            first_encounter_ms: at_ms.saturating_sub(86_400_000),
            geohash_bucket: None,
        }
    }

    #[test]
    fn confirmed_requires_three_trusted_diverse_sources() {
        let cfg = InformationTrustConfig::default();
        let now = 1_000_000u64;
        let observations = vec![
            obs("L1", true, "wifi", now),
            obs("L2", true, "ble", now),
            obs("L3", true, "relay", now),
        ];
        let trust = compute_information_trust(&observations, 1, now, 3600, &cfg);
        assert_eq!(trust.status, CorroborationStatus::Confirmed);
        assert_eq!(trust.trusted_corroboration_count, 3);
        assert!(trust.trust_score > 0.7);
        assert!(trust.display_label().contains("confirmed"));
    }

    #[test]
    fn sybil_flood_stays_unverified() {
        let cfg = InformationTrustConfig::default();
        let now = 2_000_000u64;
        let observations: Vec<_> = (0..50)
            .map(|i| obs(&format!("S{i:03}"), false, "sybil", now))
            .collect();
        let trust = compute_information_trust(&observations, 1, now, 3600, &cfg);
        assert_eq!(trust.status, CorroborationStatus::Unverified);
        assert_eq!(trust.trusted_corroboration_count, 0);
        assert!(trust.trust_score < 0.4);
    }

    #[test]
    fn conflicting_claims_reduce_trust() {
        let cfg = InformationTrustConfig::default();
        let now = 3_000_000u64;
        let observations = vec![obs("L1", true, "wifi", now), obs("L2", true, "ble", now)];
        let trust = compute_information_trust(&observations, 3, now, 3600, &cfg);
        assert_eq!(trust.status, CorroborationStatus::Conflicting);
        assert!(trust.geographic_consistency < 0.5);
        assert!(trust.trust_score < 0.6);
    }

    #[test]
    fn claim_content_hash_normalizes_whitespace() {
        assert_eq!(
            claim_content_hash("  Water   HERE "),
            claim_content_hash("water here")
        );
    }
}
