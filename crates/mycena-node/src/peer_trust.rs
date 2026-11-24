// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Persistent peer trust for SD-031 relay selection and Sybil resistance.

use anyhow::Context;
use mycena_core::lock_or_recover;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const RELIABILITY_ALPHA: f32 = 0.15;
const SPAM_BETA: f32 = 0.20;
const RELAY_EMA_ALPHA: f32 = 0.12;

/// Trust signals for a mesh peer (persisted across restarts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerTrust {
    /// When this peer was first observed (epoch ms).
    pub first_seen_ms: u64,
    /// EMA of successful vs. failed message handling [0, 1].
    pub reliability: f32,
    /// EMA of invalid / duplicate / rate-limited ingest events [0, 1].
    pub spam_score: f32,
    /// EMA of relay delivery ack success [0, 1].
    pub relay_success_rate: f32,
    pub relay_success_count: u32,
    pub relay_failure_count: u32,
    /// Confirmed relationship depth (1 = we stored their enc key / contact hello).
    pub mutual_contacts: u16,
    pub messages_accepted: u32,
    pub messages_rejected: u32,
    pub strike_count: u8,
    pub last_strike_ms: u64,
    pub throttled_until_ms: u64,
    pub last_updated_ms: u64,
    /// Last observed bearer / relay bucket for eclipse-resistant routing (e.g. `ble`, `tcp`, `relay`).
    #[serde(default)]
    pub transport_hint: Option<String>,
    /// Coarse geohash bucket (prefix) last associated with this peer — SOS/resource gossip or co-presence.
    #[serde(default)]
    pub last_geohash_bucket: Option<String>,
    #[serde(default)]
    pub last_geohash_ms: u64,
}

impl Default for PeerTrust {
    fn default() -> Self {
        Self::new_peer(0)
    }
}

impl PeerTrust {
    pub fn new_peer(now_ms: u64) -> Self {
        Self {
            first_seen_ms: now_ms,
            reliability: 0.5,
            spam_score: 0.0,
            relay_success_rate: 0.5,
            relay_success_count: 0,
            relay_failure_count: 0,
            mutual_contacts: 0,
            messages_accepted: 0,
            messages_rejected: 0,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: None,
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    /// Pessimistic prior for unseen peers at relay selection time (Sybil-resistant).
    pub fn cold_unknown(now_ms: u64) -> Self {
        Self {
            first_seen_ms: now_ms,
            reliability: 0.25,
            spam_score: 0.0,
            relay_success_rate: 0.25,
            relay_success_count: 0,
            relay_failure_count: 0,
            mutual_contacts: 0,
            messages_accepted: 0,
            messages_rejected: 0,
            strike_count: 0,
            last_strike_ms: 0,
            throttled_until_ms: 0,
            last_updated_ms: now_ms,
            transport_hint: None,
            last_geohash_bucket: None,
            last_geohash_ms: 0,
        }
    }

    pub fn note_geohash_bucket(&mut self, bucket: String, now_ms: u64) {
        if bucket.is_empty() {
            return;
        }
        self.last_geohash_bucket = Some(bucket);
        self.last_geohash_ms = now_ms;
    }

    pub fn note_transport_hint(&mut self, hint: &str) {
        let trimmed = hint.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.transport_hint.as_deref() != Some(trimmed) {
            self.transport_hint = Some(trimmed.to_string());
        }
    }

    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.first_seen_ms)
    }

    fn ema(prev: f32, sample: f32, alpha: f32) -> f32 {
        prev + alpha * (sample - prev)
    }

    pub fn touch(&mut self, now_ms: u64) {
        if self.first_seen_ms == 0 {
            self.first_seen_ms = now_ms;
        }
        self.last_updated_ms = now_ms;
    }

    pub fn record_message_accepted(&mut self, now_ms: u64) {
        self.touch(now_ms);
        self.messages_accepted = self.messages_accepted.saturating_add(1);
        self.reliability = Self::ema(self.reliability, 1.0, RELIABILITY_ALPHA);
        if self.strike_count > 0 {
            self.strike_count = self.strike_count.saturating_sub(1);
        }
    }

    pub fn record_message_rejected(&mut self, now_ms: u64) {
        self.touch(now_ms);
        self.messages_rejected = self.messages_rejected.saturating_add(1);
        self.reliability = Self::ema(self.reliability, 0.0, RELIABILITY_ALPHA);
        self.spam_score = Self::ema(self.spam_score, 1.0, SPAM_BETA);
        self.register_strike(now_ms);
    }

    pub fn register_strike(&mut self, now_ms: u64) {
        self.touch(now_ms);
        if now_ms.saturating_sub(self.last_strike_ms) > 60_000 {
            self.strike_count = 0;
        }
        self.strike_count = self.strike_count.saturating_add(1);
        self.last_strike_ms = now_ms;
        if self.strike_count >= 3 {
            self.throttled_until_ms = now_ms + 5 * 60 * 1000;
        }
    }

    pub fn decay_strikes_if_idle(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_strike_ms) > 10 * 60 * 1000 {
            self.strike_count = 0;
        }
    }

    pub fn is_throttled(&mut self, now_ms: u64) -> bool {
        self.decay_strikes_if_idle(now_ms);
        now_ms < self.throttled_until_ms
    }

    pub fn record_relay_outcome(&mut self, now_ms: u64, success: bool) {
        self.touch(now_ms);
        if success {
            self.relay_success_count = self.relay_success_count.saturating_add(1);
            self.relay_success_rate = Self::ema(self.relay_success_rate, 1.0, RELAY_EMA_ALPHA);
        } else {
            self.relay_failure_count = self.relay_failure_count.saturating_add(1);
            self.relay_success_rate = Self::ema(self.relay_success_rate, 0.0, RELAY_EMA_ALPHA);
        }
    }

    /// Local-only signal: we persisted this peer's enc key (confirmed contact).
    pub fn note_confirmed_contact(&mut self, now_ms: u64) {
        self.touch(now_ms);
        if self.mutual_contacts == 0 {
            self.mutual_contacts = 1;
        }
    }

    /// Weighted score for SD-031 relay selection (higher = preferred).
    pub fn relay_selection_score(&self, now_ms: u64) -> f64 {
        let age_ms = self.age_ms(now_ms);
        let age_factor = (age_ms as f64 / (7.0 * 86_400_000.0)).min(1.0);

        let mut score = 0.0;
        score += 0.35 * f64::from(self.reliability);
        score += 0.30 * f64::from(self.relay_success_rate);
        score += 0.15 * age_factor;
        score += 0.10 * (f64::from(self.mutual_contacts.min(5)) / 5.0);
        score -= 0.25 * f64::from(self.spam_score);
        score -= 0.20 * f64::from(self.strike_count) / 10.0;

        if age_ms < 3_600_000 {
            let youth = 1.0 - age_ms as f64 / 3_600_000.0;
            score -= 0.20 * youth;
        }
        score
    }
}

/// Snapshot for UI / FFI (legacy strike fields preserved).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PeerReputationSnapshot {
    pub strikes: u8,
    pub last_strike_ms: u64,
    pub throttled_until_ms: u64,
    pub reliability: f32,
    pub spam_score: f32,
    pub relay_success_rate: f32,
    pub age_ms: u64,
    pub mutual_contacts: u16,
}

impl PeerReputationSnapshot {
    pub fn from_trust(trust: &PeerTrust, now_ms: u64) -> Self {
        Self {
            strikes: trust.strike_count,
            last_strike_ms: trust.last_strike_ms,
            throttled_until_ms: trust.throttled_until_ms,
            reliability: trust.reliability,
            spam_score: trust.spam_score,
            relay_success_rate: trust.relay_success_rate,
            age_ms: trust.age_ms(now_ms),
            mutual_contacts: trust.mutual_contacts,
        }
    }
}

pub struct PeerTrustStore {
    _db: sled::Db,
    tree: sled::Tree,
    cache: Mutex<HashMap<String, PeerTrust>>,
}

impl PeerTrustStore {
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let path = Path::new(db_path).join("peer_trust");
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()
            .context("opening peer_trust db")?;
        let tree = db.open_tree("trust")?;
        let mut cache = HashMap::new();
        for item in tree.iter() {
            let (k, v) = item?;
            let peer_id = String::from_utf8(k.to_vec()).context("peer id utf8")?;
            let trust: PeerTrust = bincode::deserialize(&v).context("decode peer trust")?;
            cache.insert(peer_id, trust);
        }
        Ok(Self {
            _db: db,
            tree,
            cache: Mutex::new(cache),
        })
    }

    pub fn load_all(&self) -> HashMap<String, PeerTrust> {
        // POISON-RECOVER: in-memory cache clone; sled is source of truth on reopen.
        lock_or_recover!(self.cache, "peer trust cache").clone()
    }

    pub fn get(&self, peer_id: &str, now_ms: u64) -> PeerTrust {
        lock_or_recover!(self.cache, "peer trust cache")
            .get(peer_id)
            .cloned()
            .unwrap_or_else(|| PeerTrust::cold_unknown(now_ms))
    }

    fn persist(&self, peer_id: &str, trust: &PeerTrust) -> anyhow::Result<()> {
        let bytes = bincode::serialize(trust)?;
        self.tree.insert(peer_id.as_bytes(), bytes)?;
        Ok(())
    }

    pub fn with_peer<F>(&self, peer_id: &str, now_ms: u64, f: F) -> anyhow::Result<()>
    where
        F: FnOnce(&mut PeerTrust),
    {
        // POISON-RECOVER: cache update then persist; panic mid-callback only skips one sled write.
        let mut cache = lock_or_recover!(self.cache, "peer trust cache");
        let entry = cache
            .entry(peer_id.to_string())
            .or_insert_with(|| PeerTrust::new_peer(now_ms));
        entry.touch(now_ms);
        f(entry);
        let trust = entry.clone();
        drop(cache);
        self.persist(peer_id, &trust)
    }

    pub(crate) fn get_cached(&self, peer_id: &str) -> Option<PeerTrust> {
        // POISON-RECOVER: read-only cache lookup; no partial mutation on panic.
        lock_or_recover!(self.cache, "peer trust cache")
            .get(peer_id)
            .cloned()
    }

    pub(crate) fn is_throttled(&self, peer_id: &str, now_ms: u64) -> bool {
        // POISON-RECOVER: throttle check is derived; stale read only skips one decision.
        let mut cache = lock_or_recover!(self.cache, "peer trust cache");
        if let Some(trust) = cache.get_mut(peer_id) {
            return trust.is_throttled(now_ms);
        }
        false
    }

    /// Record a peer's geohash from SOS / resource-map gossip (strong location signal).
    pub fn note_peer_geohash(&self, peer_id: &str, geohash: &str, now_ms: u64) {
        let Some(bucket) = mycena_core::sos::geohash_diversity_bucket(
            geohash,
            mycena_core::sos::GEOHASH_DIVERSITY_PREFIX_LEN,
        ) else {
            return;
        };
        let _ = self.with_peer(peer_id, now_ms, |t| {
            t.note_geohash_bucket(bucket, now_ms);
        });
    }

    /// Weak hint: peer seen locally (BLE/Wi‑Fi) while we are at `local_geohash`.
    pub fn note_peer_co_presence_geohash(&self, peer_id: &str, local_geohash: &str, now_ms: u64) {
        const STALE_MS: u64 = 24 * 3_600_000;
        let Some(bucket) = mycena_core::sos::geohash_diversity_bucket(
            local_geohash,
            mycena_core::sos::GEOHASH_DIVERSITY_PREFIX_LEN,
        ) else {
            return;
        };
        let _ = self.with_peer(peer_id, now_ms, |t| {
            if t.last_geohash_bucket.is_none()
                || now_ms.saturating_sub(t.last_geohash_ms) > STALE_MS
            {
                t.note_geohash_bucket(bucket, now_ms);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn trust_persists_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_str().expect("path");
        {
            let store = PeerTrustStore::open(base).expect("open");
            store
                .with_peer("12D3KooWTrustPeerIdForPersistTest01", 1000, |t| {
                    t.reliability = 0.88;
                    t.relay_success_rate = 0.91;
                    t.first_seen_ms = 500;
                })
                .expect("save");
        }
        let store2 = PeerTrustStore::open(base).expect("reopen");
        let t = store2
            .get_cached("12D3KooWTrustPeerIdForPersistTest01")
            .expect("loaded");
        assert!((t.reliability - 0.88).abs() < f32::EPSILON);
        assert!((t.relay_success_rate - 0.91).abs() < f32::EPSILON);
    }

    #[test]
    fn geohash_bucket_persists_and_co_presence_respects_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_str().expect("path");
        let store = PeerTrustStore::open(base).expect("open");
        store.note_peer_geohash("peer_a", "u4pruydq", 1_000);
        let t = store.get_cached("peer_a").expect("loaded");
        assert_eq!(t.last_geohash_bucket.as_deref(), Some("u4pr"));
        store.note_peer_co_presence_geohash("peer_b", "u33dxyz", 2_000);
        assert_eq!(
            store
                .get_cached("peer_b")
                .expect("b")
                .last_geohash_bucket
                .as_deref(),
            Some("u33d")
        );
        store.note_peer_geohash("peer_b", "u4prfresh", 3_000);
        assert_eq!(
            store
                .get_cached("peer_b")
                .expect("b2")
                .last_geohash_bucket
                .as_deref(),
            Some("u4pr")
        );
    }

    #[test]
    fn peer_trust_survives_cache_poison() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_str().expect("path");
        let store = PeerTrustStore::open(base).expect("open");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store
                .with_peer("__poison__", 0, |_| panic!("deliberate peer trust poison"))
                .ok();
        }));
        store
            .with_peer("12D3KooWRecoveredPeerIdAfterPoison01", 2000, |t| {
                t.relay_success_rate = 0.5;
            })
            .expect("recover after poison");
        let t = store
            .get_cached("12D3KooWRecoveredPeerIdAfterPoison01")
            .expect("loaded after poison");
        assert!((t.relay_success_rate - 0.5).abs() < f32::EPSILON);
    }
}
