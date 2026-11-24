// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Persist Double-Ratchet sessions and local prekeys (offline-first).

use anyhow::Context;
use mycena_core::crypto::ratchet::{LocalPrekeyStore, PrekeyBundle, RatchetSession};
use std::collections::HashMap;
use std::path::Path;

const SESSIONS_TREE: &str = "ratchet_sessions";
const BUNDLES_TREE: &str = "ratchet_bundles";
const PREKEYS_KEY: &[u8] = b"local_prekeys";

pub struct RatchetSessionStore {
    _db: sled::Db,
    sessions: sled::Tree,
    bundles: sled::Tree,
    meta: sled::Tree,
}

impl RatchetSessionStore {
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let path = Path::new(db_path).join("ratchet");
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()
            .context("opening ratchet session db")?;
        Ok(Self {
            sessions: db.open_tree(SESSIONS_TREE)?,
            bundles: db.open_tree(BUNDLES_TREE)?,
            meta: db.open_tree("meta")?,
            _db: db,
        })
    }

    pub fn load_local_prekeys(&self) -> anyhow::Result<Option<LocalPrekeyStore>> {
        let Some(bytes) = self.meta.get(PREKEYS_KEY)? else {
            return Ok(None);
        };
        Ok(Some(bincode::deserialize(&bytes)?))
    }

    pub fn save_local_prekeys(&self, prekeys: &LocalPrekeyStore) -> anyhow::Result<()> {
        self.meta
            .insert(PREKEYS_KEY, bincode::serialize(prekeys)?)?;
        Ok(())
    }

    pub fn load_session(&self, peer_id: &str) -> anyhow::Result<Option<RatchetSession>> {
        let Some(bytes) = self.sessions.get(peer_id.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(bincode::deserialize(&bytes)?))
    }

    pub fn save_session(&self, session: &RatchetSession) -> anyhow::Result<()> {
        self.sessions
            .insert(session.peer_id().as_bytes(), bincode::serialize(session)?)?;
        Ok(())
    }

    pub fn remove_session(&self, peer_id: &str) -> anyhow::Result<()> {
        self.sessions.remove(peer_id.as_bytes())?;
        Ok(())
    }

    pub fn save_peer_bundle(&self, peer_id: &str, bundle: &PrekeyBundle) -> anyhow::Result<()> {
        self.bundles
            .insert(peer_id.as_bytes(), bincode::serialize(bundle)?)?;
        Ok(())
    }

    pub fn load_peer_bundle(&self, peer_id: &str) -> anyhow::Result<Option<PrekeyBundle>> {
        let Some(bytes) = self.bundles.get(peer_id.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(bincode::deserialize(&bytes)?))
    }

    pub fn load_all_bundles(&self) -> anyhow::Result<HashMap<String, PrekeyBundle>> {
        let mut out = HashMap::new();
        for item in self.bundles.iter() {
            let (k, v) = item?;
            let peer_id = String::from_utf8(k.to_vec()).context("bundle peer id utf8")?;
            out.insert(peer_id, bincode::deserialize(&v)?);
        }
        Ok(out)
    }

    pub fn session_peer_ids(&self) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        for item in self.sessions.iter() {
            let (k, _) = item?;
            out.push(String::from_utf8(k.to_vec()).context("session peer id utf8")?);
        }
        Ok(out)
    }

    /// Move ratchet state when a contact rotates transport PeerId (SD-190).
    pub fn remap_peer_id(&self, old_peer_id: &str, new_peer_id: &str) -> anyhow::Result<()> {
        if old_peer_id == new_peer_id {
            return Ok(());
        }
        if let Some(bytes) = self.sessions.remove(old_peer_id.as_bytes())? {
            self.sessions.insert(new_peer_id.as_bytes(), bytes)?;
        }
        if let Some(bytes) = self.bundles.remove(old_peer_id.as_bytes())? {
            self.bundles.insert(new_peer_id.as_bytes(), bytes)?;
        }
        Ok(())
    }
}
