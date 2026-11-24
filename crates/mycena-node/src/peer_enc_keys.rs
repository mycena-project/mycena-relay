// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Persist peer X25519 public keys so E2E chat survives app restarts (Phase B2).

use anyhow::Context;
use std::collections::HashMap;
use std::path::Path;

pub(crate) struct PeerEncKeyStore {
    _db: sled::Db,
    tree: sled::Tree,
}

impl PeerEncKeyStore {
    pub(crate) fn open(db_path: &str) -> anyhow::Result<Self> {
        let path = Path::new(db_path).join("peer_enc_keys");
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()
            .context("opening peer_enc_keys db")?;
        let tree = db.open_tree("keys")?;
        Ok(Self { _db: db, tree })
    }

    pub(crate) fn load_all(&self) -> anyhow::Result<HashMap<String, [u8; 32]>> {
        let mut out = HashMap::new();
        for item in self.tree.iter() {
            let (k, v) = item?;
            let peer_id = String::from_utf8(k.to_vec()).context("peer id utf8")?;
            let bytes: [u8; 32] = v
                .as_ref()
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid enc key length for {peer_id}"))?;
            out.insert(peer_id, bytes);
        }
        Ok(out)
    }

    pub(crate) fn save(&self, peer_id: &str, key: &[u8; 32]) -> anyhow::Result<()> {
        self.tree.insert(peer_id.as_bytes(), key.as_slice())?;
        Ok(())
    }

    pub(crate) fn remove(&self, peer_id: &str) -> anyhow::Result<()> {
        self.tree.remove(peer_id.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn peer_enc_keys_persist_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_str().expect("path");
        {
            let store = PeerEncKeyStore::open(base).expect("open");
            let key = [7u8; 32];
            store
                .save("12D3KooWTestPeerIdOnlyForEncKeyStore1", &key)
                .expect("save");
            let loaded = store.load_all().expect("load");
            assert_eq!(
                loaded.get("12D3KooWTestPeerIdOnlyForEncKeyStore1"),
                Some(&key)
            );
        }
        let store2 = PeerEncKeyStore::open(base).expect("reopen");
        let again = store2.load_all().expect("reload");
        let key = [7u8; 32];
        assert_eq!(
            again.get("12D3KooWTestPeerIdOnlyForEncKeyStore1"),
            Some(&key)
        );
    }
}
