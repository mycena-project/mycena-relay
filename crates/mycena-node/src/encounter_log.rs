// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Local encounter history for DTN custody scheduling (privacy: device-local, TTL-pruned).

use mycena_core::data::now_ms;
use mycena_core::energy::MovementIndicator;
use mycena_core::lock_or_recover;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Default retention for encounter records (7 days).
pub const ENCOUNTER_LOG_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Max co-encounter start timestamps kept for regularity scoring.
const MAX_CO_START_SAMPLES: usize = 24;

/// One hour — window for movement heuristics from peer churn.
const MOVEMENT_WINDOW_MS: u64 = 60 * 60_000;

/// Short encounter duration threshold (5 minutes).
const SHORT_ENCOUNTER_MS: u64 = 5 * 60_000;

/// Stable peer relationship duration (10 minutes cumulative).
const STABLE_ENCOUNTER_MS: u64 = 10 * 60_000;

/// One day in milliseconds (regularity heuristic).
const ONE_DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CustodyCandidateScoring {
    /// Rank carriers by delivery probability toward the destination plus local recency.
    #[default]
    DeliveryProbabilityAndRecency,
    /// Legacy behaviour: rank purely by when the carrier was last seen locally.
    RecencyOnly,
}

#[derive(Debug, Clone)]
pub struct CustodyCandidateQuery<'a> {
    pub connected: &'a [String],
    pub exclude: &'a str,
    pub tried: &'a [String],
    pub limit: usize,
    pub now: u64,
    pub dest_peer: Option<&'a str>,
    pub scoring: CustodyCandidateScoring,
}

impl<'a> CustodyCandidateQuery<'a> {
    pub fn recency_only(
        connected: &'a [String],
        exclude: &'a str,
        tried: &'a [String],
        limit: usize,
        now: u64,
    ) -> Self {
        Self {
            connected,
            exclude,
            tried,
            limit,
            now,
            dest_peer: None,
            scoring: CustodyCandidateScoring::RecencyOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncounterRecord {
    pub peer_id: String,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Cumulative connection time across encounters.
    pub total_duration_ms: u64,
    pub encounter_count: u32,
    /// Start of the current open encounter, if connected now.
    #[serde(default)]
    pub open_since_ms: Option<u64>,
}

/// Observed simultaneous visibility of two peers from this device (co-presence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoEncounterRecord {
    pub peer_a: String,
    pub peer_b: String,
    pub first_co_seen_ms: u64,
    pub last_co_seen_ms: u64,
    pub co_encounter_count: u32,
    pub total_co_duration_ms: u64,
    #[serde(default)]
    pub open_since_ms: Option<u64>,
    /// Start times of recent co-encounters (for regularity / schedule detection).
    #[serde(default)]
    pub recent_starts_ms: Vec<u64>,
}

/// Estimated likelihood that `carrier_peer` will meet `dest_peer` soon.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryProbabilityEstimate {
    pub carrier_peer: String,
    pub dest_peer: String,
    pub co_encounter_count: u32,
    pub avg_co_duration_ms: u64,
    pub frequency_score: f64,
    pub duration_score: f64,
    pub regularity_score: f64,
    pub recency_score: f64,
    pub combined: f64,
}

/// How an expected delivery window was derived (for UI / diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectedDeliveryBasis {
    /// Co-encounter regularity + [`DeliveryProbabilityEstimate`].
    DeliveryProbability,
    /// Local encounter frequency only (no co-encounter history toward destination).
    EncounterFrequency,
    /// No viable connected carriers; wide fallback window.
    NoCarriersNearby,
}

/// Estimated time until `carrier_peer` meets `dest_peer` again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedEncounterWindow {
    pub eta_min_ms: u64,
    pub eta_max_ms: u64,
    pub confidence: f64,
    pub basis: ExpectedDeliveryBasis,
}

/// Minimum plausible ETA shown in UI (1 minute).
pub const MIN_EXPECTED_ETA_MS: u64 = 60_000;
/// Default fallback when no carrier data exists (30 minutes).
pub const DEFAULT_EXPECTED_ETA_MIN_MS: u64 = 30 * 60_000;
/// Default fallback upper bound (24 hours).
pub const DEFAULT_EXPECTED_ETA_MAX_MS: u64 = 24 * 60 * 60_000;

#[derive(Debug)]
pub struct EncounterLogStore {
    db: sled::Db,
    open: Mutex<HashMap<String, u64>>,
    open_co: Mutex<HashMap<String, u64>>,
}

impl EncounterLogStore {
    pub fn open(base_path: &str) -> anyhow::Result<Self> {
        Self::open_with_flush(base_path, Some(5000))
    }

    /// Open without background flush (deterministic single-threaded use, e.g. sim).
    pub fn open_sync(base_path: &str) -> anyhow::Result<Self> {
        Self::open_with_flush(base_path, None)
    }

    fn open_with_flush(base_path: &str, flush_every_ms: Option<u64>) -> anyhow::Result<Self> {
        let path = Path::new(base_path).join("encounters");
        std::fs::create_dir_all(&path)?;
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(flush_every_ms)
            .open()?;
        Ok(Self {
            db,
            open: Mutex::new(HashMap::new()),
            open_co: Mutex::new(HashMap::new()),
        })
    }

    pub fn begin_encounter(&self, peer_id: &str, at_ms: u64) -> anyhow::Result<()> {
        // POISON-RECOVER: open encounter map; sled persist follows in save().
        let mut open = lock_or_recover!(self.open, "encounter open map");
        open.insert(peer_id.to_string(), at_ms);
        let mut rec = self.load_or_new(peer_id, at_ms)?;
        rec.last_seen_ms = at_ms;
        rec.open_since_ms = Some(at_ms);
        rec.encounter_count = rec.encounter_count.saturating_add(1);
        self.save(&rec)
    }

    pub fn end_encounter(&self, peer_id: &str, at_ms: u64) -> anyhow::Result<()> {
        let started = {
            // POISON-RECOVER: remove open marker; duration math uses returned start time only.
            let mut open = lock_or_recover!(self.open, "encounter open map");
            open.remove(peer_id)
        };
        let mut rec = self.load_or_new(peer_id, at_ms)?;
        rec.last_seen_ms = at_ms;
        rec.open_since_ms = None;
        if let Some(s) = started {
            rec.total_duration_ms = rec
                .total_duration_ms
                .saturating_add(at_ms.saturating_sub(s));
        }
        self.save(&rec)
    }

    pub fn touch_encounter(&self, peer_id: &str, at_ms: u64) -> anyhow::Result<()> {
        let mut rec = self.load_or_new(peer_id, at_ms)?;
        rec.last_seen_ms = at_ms;
        self.save(&rec)
    }

    /// Record that two peers were visible to this device at the same time.
    pub fn begin_co_encounter(&self, peer_a: &str, peer_b: &str, at_ms: u64) -> anyhow::Result<()> {
        if peer_a == peer_b {
            return Ok(());
        }
        let key = pair_key(peer_a, peer_b);
        let mut open_co = lock_or_recover!(self.open_co, "co-encounter open map");
        if open_co.contains_key(&key) {
            drop(open_co);
            return self.touch_co_encounter(peer_a, peer_b, at_ms);
        }
        open_co.insert(key, at_ms);
        drop(open_co);

        let mut rec = self.load_or_new_co(peer_a, peer_b, at_ms)?;
        rec.last_co_seen_ms = at_ms;
        rec.open_since_ms = Some(at_ms);
        rec.co_encounter_count = rec.co_encounter_count.saturating_add(1);
        rec.recent_starts_ms.push(at_ms);
        if rec.recent_starts_ms.len() > MAX_CO_START_SAMPLES {
            let drain = rec.recent_starts_ms.len() - MAX_CO_START_SAMPLES;
            rec.recent_starts_ms.drain(0..drain);
        }
        self.save_co(&rec)
    }

    pub fn end_co_encounter(&self, peer_a: &str, peer_b: &str, at_ms: u64) -> anyhow::Result<()> {
        if peer_a == peer_b {
            return Ok(());
        }
        let key = pair_key(peer_a, peer_b);
        let started = {
            let mut open_co = lock_or_recover!(self.open_co, "co-encounter open map");
            open_co.remove(&key)
        };
        let mut rec = self.load_or_new_co(peer_a, peer_b, at_ms)?;
        rec.last_co_seen_ms = at_ms;
        rec.open_since_ms = None;
        if let Some(s) = started {
            rec.total_co_duration_ms = rec
                .total_co_duration_ms
                .saturating_add(at_ms.saturating_sub(s));
        }
        self.save_co(&rec)
    }

    pub fn touch_co_encounter(&self, peer_a: &str, peer_b: &str, at_ms: u64) -> anyhow::Result<()> {
        if peer_a == peer_b {
            return Ok(());
        }
        let mut rec = self.load_or_new_co(peer_a, peer_b, at_ms)?;
        rec.last_co_seen_ms = at_ms;
        self.save_co(&rec)
    }

    /// Update co-encounters for every pair among simultaneously connected peers.
    pub fn observe_co_presence_among(
        &self,
        connected: &[String],
        at_ms: u64,
    ) -> anyhow::Result<()> {
        for i in 0..connected.len() {
            for j in (i + 1)..connected.len() {
                self.begin_co_encounter(&connected[i], &connected[j], at_ms)?;
            }
        }
        Ok(())
    }

    pub fn record(&self, peer_id: &str) -> anyhow::Result<Option<EncounterRecord>> {
        let tree = self.tree()?;
        let Some(bytes) = tree.get(peer_id.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(bincode::deserialize(&bytes)?))
    }

    pub fn co_encounter_record(
        &self,
        peer_a: &str,
        peer_b: &str,
    ) -> anyhow::Result<Option<CoEncounterRecord>> {
        let tree = self.co_tree()?;
        let key = pair_key(peer_a, peer_b);
        let Some(bytes) = tree.get(key.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(bincode::deserialize(&bytes)?))
    }

    pub fn delivery_probability(
        &self,
        carrier_peer: &str,
        dest_peer: &str,
        now: u64,
    ) -> anyhow::Result<DeliveryProbabilityEstimate> {
        Ok(self.delivery_probability_from_record(
            carrier_peer,
            dest_peer,
            self.co_encounter_record(carrier_peer, dest_peer)?,
            now,
        ))
    }

    pub fn delivery_probability_score(
        &self,
        carrier_peer: &str,
        dest_peer: &str,
        now: u64,
    ) -> anyhow::Result<f64> {
        Ok(self
            .delivery_probability(carrier_peer, dest_peer, now)?
            .combined)
    }

    /// Estimate when `carrier_peer` is likely to co-meet `dest_peer` again.
    pub fn estimate_encounter_window(
        &self,
        carrier_peer: &str,
        dest_peer: &str,
        now: u64,
    ) -> anyhow::Result<ExpectedEncounterWindow> {
        let prob = self.delivery_probability(carrier_peer, dest_peer, now)?;
        if let Some(co) = self.co_encounter_record(carrier_peer, dest_peer)? {
            if co.co_encounter_count >= 2 {
                if let Some(mean_interval) = mean_co_encounter_interval(&co.recent_starts_ms) {
                    let elapsed = now.saturating_sub(co.last_co_seen_ms) as f64;
                    let until_next = (mean_interval as f64 - elapsed).max(0.0);
                    let eta_center = until_next.max(MIN_EXPECTED_ETA_MS as f64);
                    let spread = (mean_interval as f64 * 0.5).max(MIN_EXPECTED_ETA_MS as f64);
                    let eta_min = (eta_center - spread).max(MIN_EXPECTED_ETA_MS as f64) as u64;
                    let eta_max = ((eta_center + spread) as u64).min(ENCOUNTER_LOG_TTL_MS);
                    return Ok(ExpectedEncounterWindow {
                        eta_min_ms: eta_min,
                        eta_max_ms: eta_max.max(eta_min),
                        confidence: prob.combined.clamp(0.15, 1.0),
                        basis: ExpectedDeliveryBasis::DeliveryProbability,
                    });
                }
            }
        }

        if let Some(rec) = self.record(carrier_peer)? {
            let span = now.saturating_sub(rec.first_seen_ms).max(1);
            let count = u64::from(rec.encounter_count.max(1));
            let mean_interval = span / count;
            let elapsed = now.saturating_sub(rec.last_seen_ms);
            let until = mean_interval.saturating_sub(elapsed);
            let eta_min = until.clamp(MIN_EXPECTED_ETA_MS, ENCOUNTER_LOG_TTL_MS);
            let eta_max = until
                .saturating_mul(3)
                .max(until.saturating_add(MIN_EXPECTED_ETA_MS))
                .min(ENCOUNTER_LOG_TTL_MS);
            return Ok(ExpectedEncounterWindow {
                eta_min_ms: eta_min,
                eta_max_ms: eta_max.max(eta_min),
                confidence: 0.25,
                basis: ExpectedDeliveryBasis::EncounterFrequency,
            });
        }

        Ok(ExpectedEncounterWindow {
            eta_min_ms: DEFAULT_EXPECTED_ETA_MIN_MS,
            eta_max_ms: DEFAULT_EXPECTED_ETA_MAX_MS,
            confidence: 0.1,
            basis: ExpectedDeliveryBasis::NoCarriersNearby,
        })
    }

    fn delivery_probability_from_record(
        &self,
        carrier_peer: &str,
        dest_peer: &str,
        rec: Option<CoEncounterRecord>,
        now: u64,
    ) -> DeliveryProbabilityEstimate {
        let Some(rec) = rec else {
            return DeliveryProbabilityEstimate {
                carrier_peer: carrier_peer.to_string(),
                dest_peer: dest_peer.to_string(),
                co_encounter_count: 0,
                avg_co_duration_ms: 0,
                frequency_score: 0.0,
                duration_score: 0.0,
                regularity_score: 0.0,
                recency_score: 0.0,
                combined: 0.0,
            };
        };

        let count = rec.co_encounter_count.max(1);
        let avg_duration = rec.total_co_duration_ms / u64::from(count);
        let frequency_score =
            (1.0 - (-f64::from(rec.co_encounter_count) / 5.0).exp()).clamp(0.0, 1.0);
        let duration_score = (avg_duration as f64 / 3_600_000.0).clamp(0.0, 1.0);
        let regularity_score = regularity_score(&rec.recent_starts_ms);
        let age = now.saturating_sub(rec.last_co_seen_ms);
        let recency_score = (1.0 - age as f64 / ENCOUNTER_LOG_TTL_MS as f64).clamp(0.0, 1.0);
        let combined = (0.35 * regularity_score
            + 0.25 * frequency_score
            + 0.15 * duration_score
            + 0.25 * recency_score)
            .clamp(0.0, 1.0);

        DeliveryProbabilityEstimate {
            carrier_peer: carrier_peer.to_string(),
            dest_peer: dest_peer.to_string(),
            co_encounter_count: rec.co_encounter_count,
            avg_co_duration_ms: avg_duration,
            frequency_score,
            duration_score,
            regularity_score,
            recency_score,
            combined,
        }
    }

    pub fn recent_peers(
        &self,
        limit: usize,
        since_ms: u64,
    ) -> anyhow::Result<Vec<EncounterRecord>> {
        let mut out = Vec::new();
        for item in self.tree()?.iter() {
            let (_, v) = item?;
            let rec: EncounterRecord = bincode::deserialize(&v)?;
            if rec.last_seen_ms >= since_ms {
                out.push(rec);
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.last_seen_ms));
        out.truncate(limit);
        Ok(out)
    }

    pub fn custody_candidates(
        &self,
        connected: &[String],
        exclude: &str,
        tried: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        self.custody_candidates_at(&CustodyCandidateQuery::recency_only(
            connected,
            exclude,
            tried,
            limit,
            now_ms(),
        ))
    }

    /// Peers to try for custody spray — ranked by delivery probability and/or recency.
    pub fn custody_candidates_at(
        &self,
        query: &CustodyCandidateQuery<'_>,
    ) -> anyhow::Result<Vec<String>> {
        let CustodyCandidateQuery {
            connected,
            exclude,
            tried,
            limit,
            now,
            dest_peer,
            scoring,
        } = query;
        let since = now.saturating_sub(ENCOUNTER_LOG_TTL_MS);
        let tried_set: std::collections::HashSet<_> = tried.iter().cloned().collect();
        let mut pool: Vec<String> = connected
            .iter()
            .filter(|p| **p != *exclude && !tried_set.contains(*p))
            .cloned()
            .collect();

        if pool.is_empty() {
            return Ok(Vec::new());
        }

        if *scoring == CustodyCandidateScoring::RecencyOnly || dest_peer.is_none() {
            return self.rank_by_recency(&pool, since, *limit);
        }

        let Some(dest) = dest_peer else {
            return self.rank_by_recency(&pool, since, *limit);
        };
        let mut scored: Vec<(String, f64)> = pool
            .iter()
            .map(|peer| {
                let local_recency = self
                    .record(peer)
                    .ok()
                    .flatten()
                    .map(|rec| {
                        let age = now.saturating_sub(rec.last_seen_ms);
                        (1.0 - age as f64 / ENCOUNTER_LOG_TTL_MS as f64).clamp(0.0, 1.0)
                    })
                    .unwrap_or(0.0);
                let delivery = self
                    .delivery_probability_from_record(
                        peer,
                        dest,
                        self.co_encounter_record(peer, dest).ok().flatten(),
                        *now,
                    )
                    .combined;
                let score = 0.55 * delivery + 0.45 * local_recency;
                (peer.clone(), score)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        pool = scored.into_iter().map(|(p, _)| p).collect();
        pool.truncate(*limit);
        Ok(pool)
    }

    fn rank_by_recency(
        &self,
        pool: &[String],
        since: u64,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let recent = self.recent_peers(pool.len().max(limit).saturating_mul(2), since)?;
        let connected_set: std::collections::HashSet<_> = pool.iter().cloned().collect();
        let mut out = Vec::new();
        for rec in recent {
            if connected_set.contains(&rec.peer_id) {
                out.push(rec.peer_id);
                if out.len() >= limit {
                    break;
                }
            }
        }
        if out.len() < limit {
            for p in pool {
                if out.contains(p) {
                    continue;
                }
                out.push(p.clone());
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub fn prune_expired(&self, now: u64) -> anyhow::Result<usize> {
        let cutoff = now.saturating_sub(ENCOUNTER_LOG_TTL_MS);
        let mut removed = 0usize;
        for item in self.tree()?.iter() {
            let (k, v) = item?;
            let rec: EncounterRecord = bincode::deserialize(&v)?;
            if rec.last_seen_ms < cutoff {
                self.tree()?.remove(k)?;
                removed += 1;
            }
        }
        for item in self.co_tree()?.iter() {
            let (k, v) = item?;
            let rec: CoEncounterRecord = bincode::deserialize(&v)?;
            if rec.last_co_seen_ms < cutoff {
                self.co_tree()?.remove(k)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn load_or_new(&self, peer_id: &str, at_ms: u64) -> anyhow::Result<EncounterRecord> {
        Ok(match self.record(peer_id)? {
            Some(r) => r,
            None => EncounterRecord {
                peer_id: peer_id.to_string(),
                first_seen_ms: at_ms,
                last_seen_ms: at_ms,
                total_duration_ms: 0,
                encounter_count: 0,
                open_since_ms: None,
            },
        })
    }

    fn load_or_new_co(
        &self,
        peer_a: &str,
        peer_b: &str,
        at_ms: u64,
    ) -> anyhow::Result<CoEncounterRecord> {
        let (a, b) = ordered_pair(peer_a, peer_b);
        Ok(match self.co_encounter_record(peer_a, peer_b)? {
            Some(r) => r,
            None => CoEncounterRecord {
                peer_a: a,
                peer_b: b,
                first_co_seen_ms: at_ms,
                last_co_seen_ms: at_ms,
                co_encounter_count: 0,
                total_co_duration_ms: 0,
                open_since_ms: None,
                recent_starts_ms: Vec::new(),
            },
        })
    }

    fn save(&self, rec: &EncounterRecord) -> anyhow::Result<()> {
        self.tree()?
            .insert(rec.peer_id.as_bytes(), bincode::serialize(rec)?)?;
        Ok(())
    }

    fn save_co(&self, rec: &CoEncounterRecord) -> anyhow::Result<()> {
        let key = pair_key(&rec.peer_a, &rec.peer_b);
        self.co_tree()?
            .insert(key.as_bytes(), bincode::serialize(rec)?)?;
        Ok(())
    }

    fn tree(&self) -> anyhow::Result<sled::Tree> {
        Ok(self.db.open_tree("encounters")?)
    }

    fn co_tree(&self) -> anyhow::Result<sled::Tree> {
        Ok(self.db.open_tree("co_encounters")?)
    }

    /// Infer whether this device is likely moving from local encounter churn (no extra sensors).
    pub fn estimate_movement_indicator(&self, now: u64) -> anyhow::Result<MovementIndicator> {
        let since = now.saturating_sub(MOVEMENT_WINDOW_MS);
        let recent = self.recent_peers(64, since)?;
        if recent.is_empty() {
            return Ok(MovementIndicator::Unknown);
        }
        let unique = recent.len();
        let short_lived = recent
            .iter()
            .filter(|r| r.encounter_count >= 2 && r.total_duration_ms < SHORT_ENCOUNTER_MS)
            .count();
        let avg_duration = recent
            .iter()
            .map(|r| r.total_duration_ms)
            .sum::<u64>()
            .checked_div(unique as u64)
            .unwrap_or(0);

        if unique >= 4 || short_lived >= 3 {
            return Ok(MovementIndicator::InMotion);
        }
        if unique <= 2 && avg_duration >= STABLE_ENCOUNTER_MS {
            return Ok(MovementIndicator::Stationary);
        }
        Ok(MovementIndicator::Unknown)
    }
}

fn ordered_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

fn pair_key(a: &str, b: &str) -> String {
    let (x, y) = ordered_pair(a, b);
    format!("{x}|{y}")
}

fn mean_co_encounter_interval(starts: &[u64]) -> Option<u64> {
    if starts.len() < 2 {
        return None;
    }
    let intervals: Vec<u64> = starts
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .filter(|i| *i > 0)
        .collect();
    if intervals.is_empty() {
        return None;
    }
    Some(intervals.iter().sum::<u64>() / intervals.len() as u64)
}

fn regularity_score(starts: &[u64]) -> f64 {
    if starts.len() < 2 {
        return 0.0;
    }
    let mut intervals: Vec<u64> = starts
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    intervals.retain(|i| *i > 0);
    if intervals.is_empty() {
        return 0.0;
    }
    let mean = intervals.iter().map(|i| *i as f64).sum::<f64>() / intervals.len() as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let variance = intervals
        .iter()
        .map(|i| {
            let d = *i as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / intervals.len() as f64;
    let cv = variance.sqrt() / mean;
    let interval_regularity = (1.0 - cv.min(1.0)).clamp(0.0, 1.0);

    let mut hour_counts = [0u16; 24];
    for t in starts {
        let hour = ((t / 3_600_000) % 24) as usize;
        hour_counts[hour] = hour_counts[hour].saturating_add(1);
    }
    let max_hour_frac = hour_counts.iter().copied().max().unwrap_or(0) as f64 / starts.len() as f64;

    let daily_bonus = if mean >= ONE_DAY_MS as f64 * 0.75 && mean <= ONE_DAY_MS as f64 * 1.25 {
        0.15
    } else {
        0.0
    };

    (0.55 * interval_regularity + 0.30 * max_hour_frac + daily_bonus).clamp(0.0, 1.0)
}

#[cfg(test)]
impl EncounterLogStore {
    pub fn test_poison_open_map(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut open = lock_or_recover!(self.open, "encounter open map");
            open.insert("__poison__".into(), 0);
            panic!("deliberate encounter open map poison");
        }));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn encounter_duration_accumulates() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        log.begin_encounter("peer-a", 1000).unwrap();
        log.end_encounter("peer-a", 5000).unwrap();
        let rec = log.record("peer-a").unwrap().unwrap();
        assert_eq!(rec.total_duration_ms, 4000);
        assert_eq!(rec.encounter_count, 1);
        assert!(rec.open_since_ms.is_none());
    }

    #[test]
    fn custody_candidates_prefers_recent_connected() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let base = now_ms();
        log.begin_encounter("b", base + 100).unwrap();
        log.end_encounter("b", base + 200).unwrap();
        log.begin_encounter("c", base + 300).unwrap();
        log.end_encounter("c", base + 400).unwrap();
        let cands = log
            .custody_candidates(&["b".into(), "c".into()], "self", &[], 1)
            .unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0], "c");
    }

    #[test]
    fn co_encounter_builds_delivery_probability() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let day = ONE_DAY_MS;
        let hub = 8 * 3_600_000;
        for d in 0..5 {
            let start = hub + d * day;
            log.begin_co_encounter("carrier", "dest", start).unwrap();
            log.end_co_encounter("carrier", "dest", start + 30 * 60_000)
                .unwrap();
        }
        let now = hub + 5 * day + 60_000;
        let est = log.delivery_probability("carrier", "dest", now).unwrap();
        assert!(est.co_encounter_count >= 5);
        assert!(
            est.regularity_score > 0.5,
            "regularity={}",
            est.regularity_score
        );
        assert!(est.combined > 0.4, "combined={}", est.combined);
    }

    #[test]
    fn custody_candidates_prefers_regular_carrier_over_recent_wanderer() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let day = ONE_DAY_MS;
        let base = 1_700_000_000_000u64;
        for d in 0..4 {
            let start = base + d * day + 8 * 3_600_000;
            log.begin_co_encounter("commuter", "receiver", start)
                .unwrap();
            log.end_co_encounter("commuter", "receiver", start + 20 * 60_000)
                .unwrap();
        }
        log.begin_encounter("commuter", base + 4 * day + 7 * 3_600_000)
            .unwrap();
        log.end_encounter("commuter", base + 4 * day + 7 * 3_600_000 + 60_000)
            .unwrap();
        log.begin_encounter("wanderer", base + 4 * day + 7 * 3_600_000 + 120_000)
            .unwrap();
        log.end_encounter("wanderer", base + 4 * day + 7 * 3_600_000 + 180_000)
            .unwrap();

        let now = base + 4 * day + 7 * 3_600_000 + 200_000;
        let recency = log
            .custody_candidates_at(&CustodyCandidateQuery {
                connected: &["commuter".into(), "wanderer".into()],
                exclude: "sender",
                tried: &[],
                limit: 1,
                now,
                dest_peer: Some("receiver"),
                scoring: CustodyCandidateScoring::RecencyOnly,
            })
            .unwrap();
        assert_eq!(recency[0], "wanderer");

        let smart = log
            .custody_candidates_at(&CustodyCandidateQuery {
                connected: &["commuter".into(), "wanderer".into()],
                exclude: "sender",
                tried: &[],
                limit: 1,
                now,
                dest_peer: Some("receiver"),
                scoring: CustodyCandidateScoring::DeliveryProbabilityAndRecency,
            })
            .unwrap();
        assert_eq!(smart[0], "commuter");
    }

    #[test]
    fn estimate_encounter_window_from_regular_co_encounters() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let day = ONE_DAY_MS;
        let hub = 8 * 3_600_000;
        for d in 0..5 {
            let start = hub + d * day;
            log.begin_co_encounter("carrier", "dest", start).unwrap();
            log.end_co_encounter("carrier", "dest", start + 20 * 60_000)
                .unwrap();
        }
        let now = hub + 5 * day + 30 * 60_000;
        let win = log
            .estimate_encounter_window("carrier", "dest", now)
            .unwrap();
        assert_eq!(win.basis, ExpectedDeliveryBasis::DeliveryProbability);
        assert!(win.eta_min_ms >= MIN_EXPECTED_ETA_MS);
        assert!(win.eta_max_ms >= win.eta_min_ms);
        assert!(win.confidence > 0.2);
    }

    #[test]
    fn estimate_movement_from_peer_churn() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let base = 1_700_000_000_000u64;
        for i in 0..5 {
            let peer = format!("passer_{i}");
            let start = base + i as u64 * 120_000;
            log.begin_encounter(&peer, start).unwrap();
            log.end_encounter(&peer, start + 60_000).unwrap();
        }
        let now = base + 600_000;
        assert_eq!(
            log.estimate_movement_indicator(now).unwrap(),
            MovementIndicator::InMotion
        );
    }

    #[test]
    fn estimate_stationary_from_stable_peers() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let base = 1_700_000_000_000u64;
        log.begin_encounter("home_ap", base).unwrap();
        log.end_encounter("home_ap", base + 30 * 60_000).unwrap();
        log.begin_encounter("neighbor", base + 60_000).unwrap();
        log.end_encounter("neighbor", base + 20 * 60_000).unwrap();
        let now = base + 45 * 60_000;
        assert_eq!(
            log.estimate_movement_indicator(now).unwrap(),
            MovementIndicator::Stationary
        );
    }

    #[test]
    fn encounter_open_map_survives_poison() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        log.test_poison_open_map();
        log.begin_encounter("peer-after-poison", 1000).unwrap();
        log.end_encounter("peer-after-poison", 2000).unwrap();
        let rec = log.record("peer-after-poison").unwrap().expect("record");
        assert_eq!(rec.encounter_count, 1);
    }
}
