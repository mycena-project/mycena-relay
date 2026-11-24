// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Network-layer security helpers (SD-030, SD-031).

use libp2p::PeerId;
use mycena_core::information_trust::SourceObservation;
use mycena_core::transport::DirectMessage;
use std::collections::{HashMap, HashSet};

pub use crate::peer_trust::{PeerReputationSnapshot, PeerTrust, PeerTrustStore};

/// Unsigned `WireMessage::Data` grace deadline (ms). `0` = disabled — unsigned messages are always rejected (SD-030).
pub const UNSIGNED_GRACE_PERIOD_UNTIL_MS: u64 = 0;

/// Per-peer routing metadata for eclipse-resistant relay / custody selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRoutingMeta {
    /// Bearer or relay bucket (`ble`, `tcp`, `relay`, operator-specific label).
    pub transport_hint: String,
    /// First encounter epoch ms (encounter log or trust `first_seen_ms`).
    pub first_encounter_ms: u64,
    /// Coarse geohash bucket for local mesh geographic diversity (SOS gossip or co-presence).
    pub geohash_bucket: Option<String>,
}

impl Default for PeerRoutingMeta {
    fn default() -> Self {
        Self {
            transport_hint: "unknown".into(),
            first_encounter_ms: 0,
            geohash_bucket: None,
        }
    }
}

/// Tunables for transport, age, social, and geographic (geohash) diversity during peer selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeerDiversityConfig {
    /// Reserve at least this many established peers when available.
    pub min_established_slots: usize,
    /// Peers older than this count as established for age diversity.
    pub established_age_ms: u64,
    /// Peers younger than this are treated as freshly appeared.
    pub young_peer_max_age_ms: u64,
    /// Max fraction of the fanout that may be young peers when established alternatives exist.
    pub max_young_peer_fraction: f32,
    /// Max fraction of the fanout that may share one transport bucket when alternatives exist.
    pub max_same_transport_fraction: f32,
    /// When true, avoid picking all peers from the same first-encounter time window.
    pub require_social_spread: bool,
    /// Width of social diversity buckets (first encounter ms / window).
    pub social_window_ms: u64,
    /// Geohash prefix length for spatial buckets (`0` disables geographic diversity).
    pub geohash_bucket_chars: usize,
    /// Max fraction of fanout from one geohash bucket when spatial alternatives exist.
    pub max_same_geohash_fraction: f32,
    /// When true, avoid an all-same-bucket fanout when geohash alternatives exist.
    pub require_geographic_spread: bool,
}

impl Default for PeerDiversityConfig {
    fn default() -> Self {
        Self {
            min_established_slots: 1,
            established_age_ms: 24 * 3_600_000,
            young_peer_max_age_ms: 3_600_000,
            max_young_peer_fraction: 0.66,
            max_same_transport_fraction: 0.66,
            require_social_spread: true,
            social_window_ms: 30 * 60 * 1000,
            geohash_bucket_chars: mycena_core::sos::GEOHASH_DIVERSITY_PREFIX_LEN,
            max_same_geohash_fraction: 0.66,
            require_geographic_spread: true,
        }
    }
}

impl PeerDiversityConfig {
    /// Score-only selection (no diversity constraints) — baseline for regression / attack sims.
    pub const fn score_only() -> Self {
        Self {
            min_established_slots: 0,
            established_age_ms: 0,
            young_peer_max_age_ms: 0,
            max_young_peer_fraction: 1.0,
            max_same_transport_fraction: 1.0,
            require_social_spread: false,
            social_window_ms: 0,
            geohash_bucket_chars: 0,
            max_same_geohash_fraction: 1.0,
            require_geographic_spread: false,
        }
    }

    /// Dense local Sybil flood: many co-located fake devices sharing one transport bucket.
    /// Stricter caps than [`Default`] so established / varied-transport peers keep relay slots.
    pub const fn dense_local_sybil() -> Self {
        Self {
            min_established_slots: 2,
            established_age_ms: 24 * 3_600_000,
            young_peer_max_age_ms: 2 * 3_600_000,
            max_young_peer_fraction: 0.34,
            max_same_transport_fraction: 0.34,
            require_social_spread: true,
            social_window_ms: 15 * 60 * 1000,
            geohash_bucket_chars: mycena_core::sos::GEOHASH_DIVERSITY_PREFIX_LEN,
            max_same_geohash_fraction: 0.34,
            require_geographic_spread: true,
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.min_established_slots == 0
            && self.max_young_peer_fraction >= 1.0
            && self.max_same_transport_fraction >= 1.0
            && !self.require_social_spread
            && (self.geohash_bucket_chars == 0 || self.max_same_geohash_fraction >= 1.0)
            && !self.require_geographic_spread
    }
}

/// Build routing metadata for a peer from trust, optional encounter, and live transport hint.
pub fn peer_routing_meta(
    trust: &PeerTrust,
    encounter_first_seen_ms: Option<u64>,
    transport_hint: Option<&str>,
) -> PeerRoutingMeta {
    PeerRoutingMeta {
        transport_hint: transport_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| trust.transport_hint.clone())
            .unwrap_or_else(|| "unknown".into()),
        first_encounter_ms: encounter_first_seen_ms.unwrap_or(trust.first_seen_ms),
        geohash_bucket: trust.last_geohash_bucket.clone(),
    }
}

pub fn build_routing_meta_map(
    peers: &[String],
    peer_trust: &HashMap<String, PeerTrust>,
    encounter_first_seen: &HashMap<String, u64>,
    transport_hints: &HashMap<String, String>,
) -> HashMap<String, PeerRoutingMeta> {
    peers
        .iter()
        .map(|p| {
            let trust = peer_trust
                .get(p)
                .cloned()
                .unwrap_or_else(|| PeerTrust::cold_unknown(0));
            let meta = peer_routing_meta(
                &trust,
                encounter_first_seen.get(p).copied(),
                transport_hints.get(p).map(String::as_str),
            );
            (p.clone(), meta)
        })
        .collect()
}

fn is_established(trust: &PeerTrust, now_ms: u64, config: &PeerDiversityConfig) -> bool {
    trust.mutual_contacts > 0 || trust.age_ms(now_ms) >= config.established_age_ms
}

fn is_young(trust: &PeerTrust, now_ms: u64, config: &PeerDiversityConfig) -> bool {
    trust.mutual_contacts == 0 && trust.age_ms(now_ms) < config.young_peer_max_age_ms
}

fn social_bucket(meta: &PeerRoutingMeta, config: &PeerDiversityConfig) -> u64 {
    if config.social_window_ms == 0 {
        return meta.first_encounter_ms;
    }
    meta.first_encounter_ms
        .checked_div(config.social_window_ms)
        .unwrap_or(0)
}

fn max_same_transport_slots(max_fanout: usize, fraction: f32) -> usize {
    if fraction >= 1.0 {
        return max_fanout;
    }
    ((max_fanout as f32 * fraction).ceil() as usize).max(1)
}

fn max_young_slots(max_fanout: usize, fraction: f32) -> usize {
    if fraction >= 1.0 {
        return max_fanout;
    }
    (max_fanout as f32 * fraction).floor() as usize
}

fn max_same_geohash_slots(max_fanout: usize, fraction: f32) -> usize {
    if fraction >= 1.0 {
        return max_fanout;
    }
    ((max_fanout as f32 * fraction).ceil() as usize).max(1)
}

fn geo_bucket(meta: &PeerRoutingMeta) -> Option<&str> {
    meta.geohash_bucket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn pool_geohash_alternatives(
    scored: &[(String, f64)],
    meta_for: &impl Fn(&str) -> PeerRoutingMeta,
) -> HashSet<String> {
    scored
        .iter()
        .filter_map(|(p, _)| geo_bucket(&meta_for(p)).map(str::to_string))
        .collect()
}

fn young_count(
    selected: &[String],
    trust_for: &impl Fn(&str) -> PeerTrust,
    now_ms: u64,
    config: &PeerDiversityConfig,
) -> usize {
    selected
        .iter()
        .filter(|p| is_young(&trust_for(p), now_ms, config))
        .count()
}

fn social_buckets_in(
    selected: &[String],
    meta_for: &impl Fn(&str) -> PeerRoutingMeta,
    config: &PeerDiversityConfig,
) -> HashSet<u64> {
    selected
        .iter()
        .map(|p| social_bucket(&meta_for(p), config))
        .collect()
}

fn pool_has_established(
    scored: &[(String, f64)],
    trust_for: &impl Fn(&str) -> PeerTrust,
    now_ms: u64,
    config: &PeerDiversityConfig,
) -> bool {
    scored
        .iter()
        .any(|(p, _)| is_established(&trust_for(p), now_ms, config))
}

fn pool_transport_alternatives(
    scored: &[(String, f64)],
    meta_for: &impl Fn(&str) -> PeerRoutingMeta,
) -> HashSet<String> {
    scored
        .iter()
        .map(|(p, _)| meta_for(p).transport_hint)
        .collect()
}

fn pool_social_alternatives(
    scored: &[(String, f64)],
    meta_for: &impl Fn(&str) -> PeerRoutingMeta,
    config: &PeerDiversityConfig,
) -> HashSet<u64> {
    scored
        .iter()
        .map(|(p, _)| social_bucket(&meta_for(p), config))
        .collect()
}

struct DiversityTrial<'a> {
    selected: &'a [String],
    candidate: &'a str,
    scored: &'a [(String, f64)],
    routing_meta: &'a HashMap<String, PeerRoutingMeta>,
    peer_trust: &'a HashMap<String, PeerTrust>,
    now_ms: u64,
    max_fanout: usize,
    config: &'a PeerDiversityConfig,
}

fn adding_violates_diversity(trial: &DiversityTrial<'_>) -> bool {
    let config = trial.config;
    if config.is_disabled() {
        return false;
    }
    let meta_for = |p: &str| trial.routing_meta.get(p).cloned().unwrap_or_default();
    let trust_for = |p: &str| {
        trial
            .peer_trust
            .get(p)
            .cloned()
            .unwrap_or_else(|| PeerTrust::cold_unknown(trial.now_ms))
    };
    let peer = trial.candidate;

    let mut trial_peers = trial.selected.to_vec();
    trial_peers.push(peer.to_string());
    let trial_len = trial_peers.len();

    let transports = pool_transport_alternatives(trial.scored, &meta_for);
    if transports.len() > 1 {
        let max_same =
            max_same_transport_slots(trial.max_fanout, config.max_same_transport_fraction);
        let hint = meta_for(peer).transport_hint;
        let same = trial_peers
            .iter()
            .filter(|p| meta_for(p).transport_hint == hint)
            .count();
        if same > max_same {
            let has_alt = trial
                .scored
                .iter()
                .any(|(p, _)| !trial_peers.contains(p) && meta_for(p).transport_hint != hint);
            if has_alt {
                return true;
            }
        }
    }

    if pool_has_established(trial.scored, &trust_for, trial.now_ms, config) {
        let max_young = max_young_slots(trial.max_fanout, config.max_young_peer_fraction);
        if is_young(&trust_for(peer), trial.now_ms, config) {
            let young = young_count(&trial_peers, &trust_for, trial.now_ms, config);
            if young > max_young {
                let has_old = trial.scored.iter().any(|(p, _)| {
                    !trial_peers.contains(p) && !is_young(&trust_for(p), trial.now_ms, config)
                });
                if has_old {
                    return true;
                }
            }
        }
    }

    if config.require_social_spread && config.social_window_ms > 0 {
        let buckets = pool_social_alternatives(trial.scored, &meta_for, config);
        if buckets.len() > 1 && trial_len >= 2 {
            let trial_buckets = social_buckets_in(&trial_peers, &meta_for, config);
            if trial_buckets.len() == 1 && trial_len == trial.max_fanout.min(trial.scored.len()) {
                let only_bucket = trial_buckets.iter().next();
                let has_alt = trial.scored.iter().any(|(p, _)| {
                    !trial_peers.contains(p)
                        && only_bucket.is_some_and(|b| social_bucket(&meta_for(p), config) != *b)
                });
                if has_alt {
                    return true;
                }
            }
        }
    }

    if config.geohash_bucket_chars > 0 {
        let geo_alts = pool_geohash_alternatives(trial.scored, &meta_for);
        if geo_alts.len() > 1 {
            let max_same =
                max_same_geohash_slots(trial.max_fanout, config.max_same_geohash_fraction);
            if let Some(hint) = geo_bucket(&meta_for(peer)) {
                let same = trial_peers
                    .iter()
                    .filter(|p| geo_bucket(&meta_for(p)) == Some(hint))
                    .count();
                if same > max_same {
                    let has_alt = trial.scored.iter().any(|(p, _)| {
                        !trial_peers.contains(p)
                            && geo_bucket(&meta_for(p)).is_some_and(|b| b != hint)
                    });
                    if has_alt {
                        return true;
                    }
                }
            }
        }

        if config.require_geographic_spread && trial_len >= 2 {
            let geo_alts = pool_geohash_alternatives(trial.scored, &meta_for);
            if geo_alts.len() > 1 && trial_len == trial.max_fanout.min(trial.scored.len()) {
                let trial_buckets: HashSet<String> = trial_peers
                    .iter()
                    .filter_map(|p| geo_bucket(&meta_for(p)).map(str::to_string))
                    .collect();
                if trial_buckets.len() == 1 {
                    let only = trial_buckets.iter().next().cloned();
                    let has_alt = trial.scored.iter().any(|(p, _)| {
                        !trial_peers.contains(p)
                            && geo_bucket(&meta_for(p)).is_some_and(|b| only.as_deref() != Some(b))
                    });
                    if has_alt {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Apply transport / age / social / geographic diversity on a score-sorted candidate list.
pub fn apply_peer_diversity(
    scored: &[(String, f64)],
    peer_trust: &HashMap<String, PeerTrust>,
    routing_meta: &HashMap<String, PeerRoutingMeta>,
    now_ms: u64,
    max_fanout: usize,
    config: &PeerDiversityConfig,
) -> Vec<String> {
    if max_fanout == 0 || scored.is_empty() {
        return Vec::new();
    }
    if config.is_disabled() {
        return scored
            .iter()
            .take(max_fanout)
            .map(|(p, _)| p.clone())
            .collect();
    }

    let meta_for = |p: &str| -> PeerRoutingMeta {
        routing_meta.get(p).cloned().unwrap_or_else(|| {
            let trust = peer_trust
                .get(p)
                .cloned()
                .unwrap_or_else(|| PeerTrust::cold_unknown(now_ms));
            peer_routing_meta(&trust, None, None)
        })
    };
    let trust_for = |p: &str| -> PeerTrust {
        peer_trust
            .get(p)
            .cloned()
            .unwrap_or_else(|| PeerTrust::cold_unknown(now_ms))
    };

    let mut selected = Vec::new();
    let mut used = HashSet::new();

    if config.min_established_slots > 0 && pool_has_established(scored, &trust_for, now_ms, config)
    {
        for (peer, _) in scored {
            if is_established(&trust_for(peer), now_ms, config) {
                selected.push(peer.clone());
                used.insert(peer.clone());
                break;
            }
        }
    }

    let mut deferred = Vec::new();
    for (peer, _) in scored {
        if selected.len() >= max_fanout {
            break;
        }
        if used.contains(peer) {
            continue;
        }
        if is_young(&trust_for(peer), now_ms, config)
            && scored
                .iter()
                .any(|(p, _)| !used.contains(p) && is_established(&trust_for(p), now_ms, config))
        {
            deferred.push(peer.clone());
            continue;
        }
        if adding_violates_diversity(&DiversityTrial {
            selected: &selected,
            candidate: peer,
            scored,
            routing_meta,
            peer_trust,
            now_ms,
            max_fanout,
            config,
        }) {
            deferred.push(peer.clone());
            continue;
        }
        selected.push(peer.clone());
        used.insert(peer.clone());
    }

    if selected.len() < max_fanout {
        for peer in deferred {
            if selected.len() >= max_fanout {
                break;
            }
            if used.contains(&peer) {
                continue;
            }
            if is_young(&trust_for(&peer), now_ms, config)
                && scored.iter().any(|(p, _)| {
                    !used.contains(p) && is_established(&trust_for(p), now_ms, config)
                })
            {
                continue;
            }
            let transports = pool_transport_alternatives(scored, &meta_for);
            if transports.len() > 1 {
                let max_same =
                    max_same_transport_slots(max_fanout, config.max_same_transport_fraction);
                let hint = meta_for(&peer).transport_hint;
                let same = selected
                    .iter()
                    .filter(|p| meta_for(p).transport_hint == hint)
                    .count()
                    + 1;
                if same > max_same {
                    continue;
                }
            }
            if config.geohash_bucket_chars > 0 {
                let geo_alts = pool_geohash_alternatives(scored, &meta_for);
                if geo_alts.len() > 1 {
                    let max_same =
                        max_same_geohash_slots(max_fanout, config.max_same_geohash_fraction);
                    if let Some(hint) = geo_bucket(&meta_for(&peer)) {
                        let same = selected
                            .iter()
                            .filter(|p| geo_bucket(&meta_for(p)) == Some(hint))
                            .count()
                            + 1;
                        if same > max_same {
                            continue;
                        }
                    }
                }
            }
            selected.push(peer.clone());
            used.insert(peer);
        }
    }

    if selected.len() < max_fanout {
        for (peer, _) in scored {
            if selected.len() >= max_fanout {
                break;
            }
            if used.contains(peer) {
                continue;
            }
            if is_young(&trust_for(peer), now_ms, config)
                && scored.iter().any(|(p, _)| {
                    !used.contains(p) && is_established(&trust_for(p), now_ms, config)
                })
            {
                continue;
            }
            selected.push(peer.clone());
            used.insert(peer.clone());
        }
    }

    if selected.len() < max_fanout {
        for (peer, _) in scored {
            if selected.len() >= max_fanout {
                break;
            }
            if !used.contains(peer) {
                selected.push(peer.clone());
                used.insert(peer.clone());
            }
        }
    }

    selected
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sd030DropReason {
    NoSignature,
    InvalidSignature,
    UnparseableAuthor,
}

/// Gossip relay bodies are unsigned by design: inner payload carries its own auth
/// (E2E group, signed store listing, enc1 bulletin). Only these exact tags bypass SD-030.
pub fn is_gossip_relay_body(body: &str) -> bool {
    body == "[mycena:group]" || body == "[appstore]" || body.starts_with("[bulletin:")
}

/// Returns `Ok(())` when the message may be processed; `Err` when it must be dropped.
pub fn validate_data_message_signature(
    message: &DirectMessage,
    _now_ms: u64,
) -> Result<(), Sd030DropReason> {
    if is_gossip_relay_body(&message.body) {
        return Ok(());
    }
    match &message.envelope.signature {
        None => Err(Sd030DropReason::NoSignature),
        Some(_) => {
            let author = message.envelope.signature_author_peer();
            let Ok(author_id) = author.parse::<PeerId>() else {
                return Err(Sd030DropReason::UnparseableAuthor);
            };
            if message.envelope.verify(&author_id) {
                Ok(())
            } else {
                Err(Sd030DropReason::InvalidSignature)
            }
        }
    }
}

/// Relay selection inputs (SD-031 + optional diversity metadata).
#[derive(Clone, Copy)]
pub struct RelaySelectionRequest<'a> {
    pub peers: &'a [String],
    pub to_peer: &'a str,
    pub peer_trust: &'a HashMap<String, PeerTrust>,
    pub endorsement_boost: Option<&'a HashMap<String, f32>>,
    pub now_ms: u64,
    pub max_fanout: usize,
    pub routing_meta: Option<&'a HashMap<String, PeerRoutingMeta>>,
    pub diversity: Option<&'a PeerDiversityConfig>,
}

/// SD-031: pick up to `max_fanout` relay peers by weighted trust score (Sybil-resistant).
///
/// `endorsement_boost`: local social trust from [`UserTrustStore`](../../mycena-coin/src/user_trust.rs)
/// (0..1 per peer). Combined with [`PeerTrust::relay_selection_score`] — separate attack surfaces.
pub fn select_relay_candidates(
    peers: &[String],
    to_peer: &str,
    peer_trust: &HashMap<String, PeerTrust>,
    endorsement_boost: Option<&HashMap<String, f32>>,
    now_ms: u64,
    max_fanout: usize,
) -> Vec<String> {
    select_relay_candidates_with_diversity(&RelaySelectionRequest {
        peers,
        to_peer,
        peer_trust,
        endorsement_boost,
        now_ms,
        max_fanout,
        routing_meta: None,
        diversity: None,
    })
}

/// Like [`select_relay_candidates`] with explicit routing metadata and diversity tunables.
pub fn select_relay_candidates_with_diversity(req: &RelaySelectionRequest<'_>) -> Vec<String> {
    const ENDORSEMENT_WEIGHT: f64 = 0.15;
    let config = req.diversity.copied().unwrap_or_default();
    let meta_map = req.routing_meta.cloned().unwrap_or_else(|| {
        build_routing_meta_map(req.peers, req.peer_trust, &HashMap::new(), &HashMap::new())
    });

    let mut scored: Vec<(String, f64)> = req
        .peers
        .iter()
        .filter(|p| *p != req.to_peer)
        .map(|p| {
            let trust = req
                .peer_trust
                .get(p)
                .cloned()
                .unwrap_or_else(|| PeerTrust::cold_unknown(req.now_ms));
            let mut score = trust.relay_selection_score(req.now_ms);
            if let Some(boosts) = req.endorsement_boost {
                if let Some(b) = boosts.get(p) {
                    score += ENDORSEMENT_WEIGHT * f64::from(*b);
                }
            }
            (p.clone(), score)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    apply_peer_diversity(
        &scored,
        req.peer_trust,
        &meta_map,
        req.now_ms,
        req.max_fanout,
        &config,
    )
}

/// Score 0–1: how well trusted corroborating peers satisfy [`PeerDiversityConfig`].
pub fn corroboration_source_diversity(
    trusted_obs: &[SourceObservation],
    peer_trust: &HashMap<String, PeerTrust>,
    now_ms: u64,
    config: &PeerDiversityConfig,
) -> f32 {
    if trusted_obs.is_empty() {
        return 0.0;
    }
    let n = trusted_obs.len();
    let transports: HashSet<&str> = trusted_obs
        .iter()
        .map(|o| o.transport_hint.as_str())
        .collect();
    let social_buckets: HashSet<u64> = trusted_obs
        .iter()
        .map(|o| social_bucket_from_ms(o.first_encounter_ms, config))
        .collect();
    let geo_buckets: HashSet<&str> = trusted_obs
        .iter()
        .filter_map(|o| o.geohash_bucket.as_deref())
        .collect();

    let mut score = if geo_buckets.is_empty() {
        0.5 * (transports.len() as f32 / n as f32) + 0.5 * (social_buckets.len() as f32 / n as f32)
    } else {
        0.34 * (transports.len() as f32 / n as f32)
            + 0.33 * (social_buckets.len() as f32 / n as f32)
            + 0.33 * (geo_buckets.len() as f32 / n as f32)
    };

    if config.require_social_spread && n >= 2 && social_buckets.len() == 1 {
        score *= 0.5;
    }
    if config.require_geographic_spread
        && config.geohash_bucket_chars > 0
        && n >= 2
        && geo_buckets.len() == 1
        && trusted_obs.iter().any(|o| o.geohash_bucket.is_some())
    {
        score *= 0.5;
    }

    let young_count = trusted_obs
        .iter()
        .filter(|o| {
            peer_trust
                .get(&o.relay_peer)
                .map(|t| is_young(t, now_ms, config))
                .unwrap_or(true)
        })
        .count();
    if n > 0 && config.max_young_peer_fraction < 1.0 {
        let young_frac = young_count as f32 / n as f32;
        if young_frac > config.max_young_peer_fraction {
            let over = young_frac - config.max_young_peer_fraction;
            score *= (1.0 - over).clamp(0.25, 1.0);
        }
    }

    let mut transport_counts: HashMap<&str, usize> = HashMap::new();
    for obs in trusted_obs {
        *transport_counts
            .entry(obs.transport_hint.as_str())
            .or_default() += 1;
    }
    if let Some(max_same) = transport_counts.values().max().copied() {
        let same_frac = max_same as f32 / n as f32;
        if transports.len() > 1 && same_frac > config.max_same_transport_fraction {
            let over = same_frac - config.max_same_transport_fraction;
            score *= (1.0 - over).clamp(0.25, 1.0);
        }
    }

    if config.geohash_bucket_chars > 0 && geo_buckets.len() > 1 {
        let mut geo_counts: HashMap<&str, usize> = HashMap::new();
        for obs in trusted_obs {
            if let Some(gh) = obs.geohash_bucket.as_deref() {
                *geo_counts.entry(gh).or_default() += 1;
            }
        }
        if let Some(max_same) = geo_counts.values().max().copied() {
            let geo_n = geo_counts.values().sum::<usize>().max(1);
            let same_frac = max_same as f32 / geo_n as f32;
            if same_frac > config.max_same_geohash_fraction {
                let over = same_frac - config.max_same_geohash_fraction;
                score *= (1.0 - over).clamp(0.25, 1.0);
            }
        }
    }

    score.clamp(0.0, 1.0)
}

fn social_bucket_from_ms(first_encounter_ms: u64, config: &PeerDiversityConfig) -> u64 {
    if config.social_window_ms == 0 {
        first_encounter_ms
    } else {
        first_encounter_ms
            .checked_div(config.social_window_ms)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use libp2p::identity;
    use mycena_core::data::Envelope;

    fn sample_message(signed: bool, body: &str) -> DirectMessage {
        let kp = identity::Keypair::generate_ed25519();
        let author = kp.public().to_peer_id().to_string();
        let mut envelope = Envelope::new(author, Some("dest".into()), b"payload".to_vec());
        if signed {
            envelope.sign(&kp).unwrap();
        } else {
            envelope.signature = None;
        }
        DirectMessage {
            envelope,
            body: body.into(),
        }
    }

    fn established_trust(
        now_ms: u64,
        reliability: f32,
        relay_rate: f32,
        age_days: u64,
    ) -> PeerTrust {
        PeerTrust {
            first_seen_ms: now_ms.saturating_sub(age_days * 86_400_000),
            reliability,
            spam_score: 0.02,
            relay_success_rate: relay_rate,
            relay_success_count: 100,
            relay_failure_count: 5,
            mutual_contacts: 2,
            messages_accepted: 500,
            messages_rejected: 2,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: None,
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    fn sybil_gaming_trust(now_ms: u64, reliability: f32, idx: u64) -> PeerTrust {
        PeerTrust {
            first_seen_ms: now_ms.saturating_sub(30 * 86_400_000 + idx * 86_400_000),
            reliability,
            spam_score: 0.01,
            relay_success_rate: reliability,
            relay_success_count: 200,
            relay_failure_count: 2,
            mutual_contacts: 0,
            messages_accepted: 800,
            messages_rejected: 0,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: Some("attacker-relay".into()),
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    #[test]
    fn eclipse_swarm_blocked_without_diversity() {
        let now = 1_700_000_000_000u64;
        let mut trust = HashMap::new();
        let mut meta = HashMap::new();
        let mut peers = Vec::new();
        for i in 0..9 {
            let id = format!("sybil_{i}");
            peers.push(id.clone());
            trust.insert(id.clone(), sybil_gaming_trust(now, 0.96, i as u64));
            meta.insert(
                id,
                PeerRoutingMeta {
                    transport_hint: "attacker-relay".into(),
                    first_encounter_ms: now.saturating_sub(3 * 86_400_000),
                    geohash_bucket: None,
                },
            );
        }
        peers.push("legit".into());
        trust.insert("legit".into(), established_trust(now, 0.82, 0.80, 45));
        meta.insert(
            "legit".into(),
            PeerRoutingMeta {
                transport_hint: "mesh-lan".into(),
                first_encounter_ms: now.saturating_sub(45 * 86_400_000),
                geohash_bucket: None,
            },
        );

        let naive = select_relay_candidates_with_diversity(&RelaySelectionRequest {
            peers: &peers,
            to_peer: "dest",
            peer_trust: &trust,
            endorsement_boost: None,
            now_ms: now,
            max_fanout: 3,
            routing_meta: Some(&meta),
            diversity: Some(&PeerDiversityConfig::score_only()),
        });
        assert_eq!(naive.len(), 3);
        assert!(
            naive.iter().all(|p| p.starts_with("sybil_")),
            "score-only should eclipse victim: {naive:?}"
        );

        let diverse = select_relay_candidates_with_diversity(&RelaySelectionRequest {
            peers: &peers,
            to_peer: "dest",
            peer_trust: &trust,
            endorsement_boost: None,
            now_ms: now,
            max_fanout: 3,
            routing_meta: Some(&meta),
            diversity: None,
        });
        assert_eq!(diverse.len(), 3);
        assert!(
            diverse.contains(&"legit".to_string()),
            "diversity must retain established legit peer: {diverse:?}"
        );
    }

    #[test]
    fn unsigned_dropped_after_grace_period() {
        let msg = sample_message(false, "hello");
        let after_grace = UNSIGNED_GRACE_PERIOD_UNTIL_MS + 1;
        assert_eq!(
            validate_data_message_signature(&msg, after_grace),
            Err(Sd030DropReason::NoSignature)
        );
    }

    #[test]
    fn unsigned_rejected_immediately() {
        let msg = sample_message(false, "hello");
        assert_eq!(
            validate_data_message_signature(&msg, 0),
            Err(Sd030DropReason::NoSignature)
        );
    }

    #[test]
    fn gossip_relay_body_skips_sig_check() {
        let msg = sample_message(false, "[mycena:group]");
        let after_grace = UNSIGNED_GRACE_PERIOD_UNTIL_MS + 1;
        assert!(validate_data_message_signature(&msg, after_grace).is_ok());
    }

    #[test]
    fn invalid_sig_is_dropped() {
        let kp_a = identity::Keypair::generate_ed25519();
        let kp_b = identity::Keypair::generate_ed25519();
        let author_b = kp_b.public().to_peer_id().to_string();
        let mut envelope = Envelope::new(author_b, Some("dest".into()), b"payload".to_vec());
        envelope.sign(&kp_a).unwrap();
        let msg = DirectMessage {
            envelope,
            body: "hello".into(),
        };
        assert_eq!(
            validate_data_message_signature(&msg, 0),
            Err(Sd030DropReason::InvalidSignature)
        );
    }

    #[test]
    fn fanout_is_bounded() {
        let peers: Vec<String> = (0..10).map(|i| format!("peer{i}")).collect();
        let now = 1_700_000_000_000u64;
        let relay = select_relay_candidates(&peers, "unknown", &HashMap::new(), None, now, 3);
        assert_eq!(relay.len(), 3);
        assert!(!relay.iter().any(|p| p == "unknown"));
    }

    #[test]
    fn high_trust_peers_preferred_over_spammers() {
        let now = 1_700_000_000_000u64;
        let peers = vec![
            "good_a".into(),
            "good_b".into(),
            "bad_a".into(),
            "bad_b".into(),
        ];
        let mut trust = HashMap::new();
        trust.insert("good_a".into(), established_trust(now, 0.95, 0.92, 30));
        trust.insert("good_b".into(), established_trust(now, 0.90, 0.88, 14));
        let mut bad = established_trust(now, 0.2, 0.15, 30);
        bad.spam_score = 0.85;
        bad.strike_count = 6;
        trust.insert("bad_a".into(), bad.clone());
        trust.insert("bad_b".into(), bad);
        let selected = select_relay_candidates(&peers, "dest", &trust, None, now, 2);
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&"good_a".to_string()));
        assert!(selected.contains(&"good_b".to_string()));
    }

    /// Property-style: a Sybil swarm of fresh peers must not outrank established reliable peers.
    #[test]
    fn sybil_swarm_not_preferred_over_established() {
        let now = 1_700_000_000_000u64;
        let mut trust = HashMap::new();
        let mut peers = Vec::new();

        for i in 0..3 {
            let id = format!("12D3KooWEstablished{i:02}");
            peers.push(id.clone());
            trust.insert(id, established_trust(now, 0.93, 0.90, 21));
        }
        for i in 0..25 {
            let id = format!("12D3KooWSybil{i:02}");
            peers.push(id.clone());
            trust.insert(id, PeerTrust::cold_unknown(now));
        }

        for fanout in 1..=3 {
            let selected =
                select_relay_candidates(&peers, "12D3KooWDest", &trust, None, now, fanout);
            assert_eq!(selected.len(), fanout);
            assert!(
                selected
                    .iter()
                    .all(|p| p.starts_with("12D3KooWEstablished")),
                "fanout {fanout} picked sybil: {selected:?}"
            );
        }
    }

    /// Randomized property: adding more Sybil peers must not displace top established picks.
    #[test]
    fn sybil_property_more_sybls_still_lose() {
        let now = 1_700_000_000_000u64;
        let mut trust = HashMap::new();
        let mut peers = Vec::new();
        for i in 0..5 {
            let id = format!("est_{i}");
            peers.push(id.clone());
            trust.insert(id, established_trust(now, 0.91, 0.89, 10 + i as u64));
        }

        for sybil_count in [5, 15, 40, 100] {
            let mut trial_peers = peers.clone();
            let mut trial_trust = trust.clone();
            for i in 0..sybil_count {
                let id = format!("sybil_{sybil_count}_{i}");
                trial_peers.push(id.clone());
                trial_trust.insert(id, PeerTrust::cold_unknown(now));
            }
            let selected =
                select_relay_candidates(&trial_peers, "target", &trial_trust, None, now, 3);
            assert_eq!(selected.len(), 3);
            let established_in_top: usize =
                selected.iter().filter(|p| p.starts_with("est_")).count();
            assert_eq!(
                established_in_top, 3,
                "sybil_count={sybil_count} selected={selected:?}"
            );
        }
    }

    #[test]
    fn local_endorsement_boosts_relay_score() {
        let now = 1_700_000_000_000u64;
        let peers = vec!["endorsed".into(), "plain".into()];
        let mut trust = HashMap::new();
        trust.insert("endorsed".into(), PeerTrust::cold_unknown(now));
        trust.insert("plain".into(), PeerTrust::cold_unknown(now));
        let mut boosts = HashMap::new();
        boosts.insert("endorsed".to_string(), 1.0f32);
        let selected = select_relay_candidates(&peers, "dest", &trust, Some(&boosts), now, 1);
        assert_eq!(selected, vec!["endorsed".to_string()]);
    }

    #[test]
    fn geographic_diversity_defers_same_geohash_cluster() {
        let now = 1_700_000_000_000u64;
        let config = PeerDiversityConfig::default();
        let mut trust = HashMap::new();
        let mut meta = HashMap::new();
        let mut scored = Vec::new();

        for i in 0..4 {
            let id = format!("local_{i}");
            let mut t = established_trust(now, 0.95 - i as f32 * 0.01, 0.90, 30);
            t.last_geohash_bucket = Some("u4pr".into());
            trust.insert(id.clone(), t);
            meta.insert(
                id.clone(),
                PeerRoutingMeta {
                    transport_hint: "ble".into(),
                    first_encounter_ms: now.saturating_sub(30 * 86_400_000),
                    geohash_bucket: Some("u4pr".into()),
                },
            );
            scored.push((id, 1.0 - i as f64 * 0.01));
        }

        let mut distant = established_trust(now, 0.88, 0.85, 30);
        distant.last_geohash_bucket = Some("u33d".into());
        trust.insert("distant".into(), distant);
        meta.insert(
            "distant".into(),
            PeerRoutingMeta {
                transport_hint: "ble".into(),
                first_encounter_ms: now.saturating_sub(30 * 86_400_000),
                geohash_bucket: Some("u33d".into()),
            },
        );
        scored.push(("distant".into(), 0.80));

        let picked = apply_peer_diversity(&scored, &trust, &meta, now, 2, &config);
        assert_eq!(picked.len(), 2);
        assert!(
            picked.contains(&"distant".to_string()),
            "should include geographically distant peer: {picked:?}"
        );
        assert!(
            picked.iter().filter(|p| p.starts_with("local_")).count() <= 1,
            "should not fill fanout from one geohash cluster: {picked:?}"
        );
    }
}
