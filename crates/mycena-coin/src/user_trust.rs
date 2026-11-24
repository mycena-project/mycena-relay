// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Local **UserTrust** — network contribution reputation, separate from MXC ledger balance.
//!
//! ## PeerTrust vs UserTrust (intentionally separate)
//!
//! | | [`PeerTrust`](../../mycena-node/src/peer_trust.rs) | `UserTrust` |
//! |---|---|---|
//! | Layer | `mycena-node` | `mycena-coin` (non-financial tree) |
//! | Subject | Remote peers | Self + local endorsements of others |
//! | Attack surface | Sybil relay / spam | Social gaming of endorsements |
//! | Transferable | No | **No** — not on ledger, not tradable |
//! | Used for | Ingest strikes, relay *peer* scoring | UI contribution, endorsement boost, spam caution |
//!
//! Relay routing combines both: `PeerTrust::relay_selection_score` + local endorsement boost.

use crate::error::{CoinError, CoinResult};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// Non-transferable local network citizenship metrics (not MXC balance).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct UserTrust {
    /// Messages forwarded for others (relay hops initiated locally).
    pub relay_contribution: u32,
    /// Cumulative mesh service uptime (seconds).
    pub uptime_secs: u64,
    /// Direct messages successfully delivered to this device.
    pub successful_deliveries: u32,
    /// Count of distinct peers locally endorsed (social trust).
    pub local_endorsements: u32,
    pub updated_at_ms: u64,
}

impl UserTrust {
    pub fn contribution_score(&self) -> f64 {
        let relay = (self.relay_contribution as f64).ln_1p() * 8.0;
        let uptime = (self.uptime_secs as f64 / 3600.0).min(168.0) * 0.5;
        let deliveries = (self.successful_deliveries as f64).ln_1p() * 5.0;
        let endorse = (self.local_endorsements as f64).ln_1p() * 3.0;
        relay + uptime + deliveries + endorse
    }

    pub fn contribution_tier(&self) -> &'static str {
        match self.contribution_score() as u32 {
            0..=15 => "Newcomer",
            16..=40 => "Helper",
            41..=80 => "Reliable",
            _ => "Anchor",
        }
    }
}

/// Read-only snapshot for UI / FFI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserTrustSummary {
    pub relay_contribution: u32,
    pub uptime_secs: u64,
    pub successful_deliveries: u32,
    pub local_endorsements: u32,
    pub contribution_score: f64,
    pub contribution_tier: String,
}

impl From<&UserTrust> for UserTrustSummary {
    fn from(t: &UserTrust) -> Self {
        Self {
            relay_contribution: t.relay_contribution,
            uptime_secs: t.uptime_secs,
            successful_deliveries: t.successful_deliveries,
            local_endorsements: t.local_endorsements,
            contribution_score: t.contribution_score(),
            contribution_tier: t.contribution_tier().to_string(),
        }
    }
}

pub struct UserTrustStore {
    _db: sled::Db,
    self_tree: sled::Tree,
    endorse_tree: sled::Tree,
    cache: Mutex<UserTrust>,
    endorse_cache: Mutex<HashMap<String, u8>>,
}

const SELF_KEY: &[u8] = b"self";

fn lock_user_trust<'a>(m: &'a Mutex<UserTrust>) -> MutexGuard<'a, UserTrust> {
    // POISON-RECOVER: in-memory metrics; sled persist runs after guard drop with full snapshot.
    mycena_l0_facade::lock_recover::lock_or_recover(m, "user trust cache")
}

fn lock_endorse<'a>(m: &'a Mutex<HashMap<String, u8>>) -> MutexGuard<'a, HashMap<String, u8>> {
    // POISON-RECOVER: endorsement map; worst case stale boost until next write.
    mycena_l0_facade::lock_recover::lock_or_recover(m, "endorse cache")
}

impl UserTrustStore {
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let path = Path::new(db_path).join("user_trust");
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()
            .context("opening user_trust db")?;
        let self_tree = db.open_tree("self_metrics")?;
        let endorse_tree = db.open_tree("local_endorsements")?;
        let cache = self_tree
            .get(SELF_KEY)?
            .map(|v| bincode::deserialize::<UserTrust>(&v))
            .transpose()
            .context("decode user trust")?
            .unwrap_or_default();
        let mut endorse_cache = HashMap::new();
        for item in endorse_tree.iter() {
            let (k, v) = item.context("endorse iter")?;
            let peer = String::from_utf8(k.to_vec()).context("endorse key utf8")?;
            let strength = v.first().copied().unwrap_or(1);
            endorse_cache.insert(peer, strength);
        }
        Ok(Self {
            _db: db,
            self_tree,
            endorse_tree,
            cache: Mutex::new(cache),
            endorse_cache: Mutex::new(endorse_cache),
        })
    }

    pub fn snapshot(&self) -> UserTrustSummary {
        UserTrustSummary::from(&*lock_user_trust(&self.cache))
    }

    pub fn with_self<F>(&self, now_ms: u64, f: F) -> CoinResult<()>
    where
        F: FnOnce(&mut UserTrust),
    {
        let mut guard = lock_user_trust(&self.cache);
        f(&mut guard);
        guard.updated_at_ms = now_ms;
        self.self_tree
            .insert(SELF_KEY, bincode::serialize(&*guard)?)
            .map_err(CoinError::from)?;
        Ok(())
    }

    pub fn record_relay_forward(&self, now_ms: u64, count: u32) -> CoinResult<()> {
        self.with_self(now_ms, |t| {
            t.relay_contribution = t.relay_contribution.saturating_add(count);
        })
    }

    pub fn record_delivery(&self, now_ms: u64) -> CoinResult<()> {
        self.with_self(now_ms, |t| {
            t.successful_deliveries = t.successful_deliveries.saturating_add(1);
        })
    }

    pub fn tick_uptime(&self, now_ms: u64, delta_secs: u64) -> CoinResult<()> {
        if delta_secs == 0 {
            return Ok(());
        }
        self.with_self(now_ms, |t| {
            t.uptime_secs = t.uptime_secs.saturating_add(delta_secs);
        })
    }

    /// Endorse a peer locally (1–5). Not gossiped; boosts relay preference only.
    pub fn endorse_peer(&self, peer_id: &str, strength: u8, now_ms: u64) -> CoinResult<()> {
        let strength = strength.clamp(1, 5);
        let mut endorse = lock_endorse(&self.endorse_cache);
        let is_new = !endorse.contains_key(peer_id);
        endorse.insert(peer_id.to_string(), strength);
        self.endorse_tree
            .insert(peer_id.as_bytes(), &[strength])
            .map_err(CoinError::from)?;
        if is_new {
            self.with_self(now_ms, |t| {
                t.local_endorsements = t.local_endorsements.saturating_add(1);
            })?;
        }
        Ok(())
    }

    pub fn revoke_endorsement(&self, peer_id: &str, now_ms: u64) -> CoinResult<()> {
        let mut endorse = lock_endorse(&self.endorse_cache);
        if endorse.remove(peer_id).is_some() {
            self.endorse_tree
                .remove(peer_id.as_bytes())
                .map_err(CoinError::from)?;
            self.with_self(now_ms, |t| {
                t.local_endorsements = t.local_endorsements.saturating_sub(1);
            })?;
        }
        Ok(())
    }

    /// Endorsement boost [0, 1] for relay scoring (`select_relay_candidates`).
    pub fn endorsement_boost_map(&self) -> HashMap<String, f32> {
        lock_endorse(&self.endorse_cache)
            .iter()
            .map(|(peer, strength)| (peer.clone(), f32::from(*strength) / 5.0))
            .collect()
    }

    pub fn endorsement_boost_for(&self, peer_id: &str) -> f32 {
        lock_endorse(&self.endorse_cache)
            .get(peer_id)
            .map(|s| f32::from(*s) / 5.0)
            .unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn user_trust_not_in_ledger() {
        let dir = tempdir().unwrap();
        let store = UserTrustStore::open(dir.path().to_str().unwrap()).unwrap();
        store.record_relay_forward(1000, 3).unwrap();
        store.record_delivery(1001).unwrap();
        store.endorse_peer("peerA", 4, 1002).unwrap();
        let s = store.snapshot();
        assert_eq!(s.relay_contribution, 3);
        assert_eq!(s.successful_deliveries, 1);
        assert_eq!(s.local_endorsements, 1);
        assert!(s.contribution_score > 0.0);
    }

    #[test]
    fn user_trust_survives_cache_poison() {
        let dir = tempdir().unwrap();
        let store = UserTrustStore::open(dir.path().to_str().unwrap()).unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store
                .with_self(0, |_| panic!("deliberate user trust poison"))
                .ok();
        }));
        store.record_relay_forward(1000, 2).unwrap();
        assert_eq!(store.snapshot().relay_contribution, 2);
    }
}
