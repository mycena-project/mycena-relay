// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Tracks rotatable transport identity epoch (SD-190 spike).

use mycena_core::data::now_ms;
use serde::{Deserialize, Serialize};

const META_KEY: &[u8] = b"transport_meta";

/// Default: rotate transport PeerId every 24 hours (override with `MYCENA_TRANSPORT_ROTATION_SECS`).
pub fn rotation_interval_ms() -> u64 {
    std::env::var("MYCENA_TRANSPORT_ROTATION_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(86_400)
        .saturating_mul(1000)
}

/// When true (default), rotate transport PeerId on every process start after the first boot.
pub fn rotate_on_cold_start() -> bool {
    !matches!(
        std::env::var("MYCENA_TRANSPORT_ROTATE_ON_START").as_deref(),
        Ok("0") | Ok("false") | Ok("FALSE")
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TransportMeta {
    pub epoch: u64,
    pub rotated_at_ms: u64,
    #[serde(default)]
    pub previous_peer_ids: Vec<String>,
}

pub struct TransportMetaStore {
    tree: sled::Tree,
}

impl TransportMetaStore {
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let db = sled::open(format!("{db_path}/transport_meta"))?;
        let tree = db.open_tree("meta")?;
        Ok(Self { tree })
    }

    pub fn load(&self) -> anyhow::Result<TransportMeta> {
        match self.tree.get(META_KEY)? {
            Some(bytes) => Ok(bincode::deserialize(&bytes)?),
            None => Ok(TransportMeta::default()),
        }
    }

    pub fn save(&self, meta: &TransportMeta) -> anyhow::Result<()> {
        self.tree.insert(META_KEY, bincode::serialize(meta)?)?;
        Ok(())
    }

    /// Returns true when transport keys should be regenerated at node bootstrap.
    pub fn should_rotate(&self, meta: &TransportMeta) -> bool {
        if meta.rotated_at_ms == 0 {
            return false;
        }
        if rotate_on_cold_start() {
            return true;
        }
        now_ms().saturating_sub(meta.rotated_at_ms) >= rotation_interval_ms()
    }

    pub fn mark_initial(&self, meta: &mut TransportMeta) -> anyhow::Result<()> {
        if meta.rotated_at_ms == 0 {
            meta.rotated_at_ms = now_ms();
            meta.epoch = meta.epoch.max(1);
            self.save(meta)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn transport_meta_persist() {
        let dir = tempfile::tempdir().unwrap();
        let store = TransportMetaStore::open(dir.path().to_str().unwrap()).unwrap();
        let mut meta = TransportMeta {
            epoch: 3,
            rotated_at_ms: 1_000,
            previous_peer_ids: vec!["old".into()],
        };
        store.save(&meta).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, meta);
        meta.epoch = 4;
        store.save(&meta).unwrap();
        assert_eq!(store.load().unwrap().epoch, 4);
    }

    #[test]
    fn cold_start_rotation_env() {
        let dir = tempfile::tempdir().unwrap();
        let store = TransportMetaStore::open(dir.path().to_str().unwrap()).unwrap();
        let meta = TransportMeta {
            epoch: 1,
            rotated_at_ms: now_ms(),
            previous_peer_ids: vec![],
        };
        store.save(&meta).unwrap();
        std::env::remove_var("MYCENA_TRANSPORT_ROTATE_ON_START");
        assert!(rotate_on_cold_start());
        assert!(store.should_rotate(&meta));
        std::env::set_var("MYCENA_TRANSPORT_ROTATE_ON_START", "0");
        assert!(!rotate_on_cold_start());
        assert!(!store.should_rotate(&meta));
        std::env::remove_var("MYCENA_TRANSPORT_ROTATE_ON_START");
    }
}
