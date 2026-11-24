// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Persisted fact-claim observations for corroboration scoring across restarts.

use mycena_core::information_trust::{FactClaimKey, SourceObservation};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedFactRow {
    key: FactClaimKey,
    observation: SourceObservation,
}

pub struct InformationTrustStore {
    _db: sled::Db,
    tree: sled::Tree,
}

impl InformationTrustStore {
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let path = Path::new(db_path).join("information_trust");
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()
            .map_err(|e| anyhow::anyhow!("opening information_trust db: {e}"))?;
        let tree = db.open_tree("observations")?;
        Ok(Self { _db: db, tree })
    }

    pub fn append(
        &self,
        key: &FactClaimKey,
        observation: &SourceObservation,
    ) -> anyhow::Result<()> {
        let row = PersistedFactRow {
            key: key.clone(),
            observation: observation.clone(),
        };
        let sled_key = format!(
            "{:020}:{}:{:016x}",
            observation.observed_at_ms, observation.relay_peer, key.content_hash
        );
        self.tree
            .insert(sled_key.as_bytes(), bincode::serialize(&row)?)?;
        Ok(())
    }

    pub fn load_all(&self) -> anyhow::Result<Vec<(FactClaimKey, SourceObservation)>> {
        let mut out = Vec::new();
        for item in self.tree.iter() {
            let (_k, v) = item?;
            let row: PersistedFactRow = bincode::deserialize(&v)?;
            out.push((row.key, row.observation));
        }
        Ok(out)
    }

    pub fn prune_before(&self, cutoff_ms: u64) -> anyhow::Result<usize> {
        let keys: Vec<Vec<u8>> = self
            .tree
            .iter()
            .filter_map(|item| {
                let (k, v) = item.ok()?;
                let row: PersistedFactRow = bincode::deserialize(&v).ok()?;
                if row.observation.observed_at_ms < cutoff_ms {
                    Some(k.to_vec())
                } else {
                    None
                }
            })
            .collect();
        let removed = keys.len();
        for k in keys {
            self.tree.remove(k)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::information_trust::FactKind;
    use mycena_core::resource_map::ResourceCategory;
    use tempfile::tempdir;

    #[test]
    fn observations_persist_roundtrip() {
        let dir = tempdir().expect("tempdir");
        let store = InformationTrustStore::open(dir.path().to_str().expect("path")).expect("open");
        let key = FactClaimKey::resource("u4pru", ResourceCategory::Water, "fountain");
        let obs = SourceObservation {
            relay_peer: "L1".into(),
            observed_at_ms: 1_000,
            trusted_independent: true,
            transport_hint: "wifi".into(),
            first_encounter_ms: 500,
            geohash_bucket: Some("u4pr".into()),
        };
        store.append(&key, &obs).expect("append");
        drop(store);
        let store2 =
            InformationTrustStore::open(dir.path().to_str().expect("path")).expect("reopen");
        let rows = store2.load_all().expect("load");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.kind, FactKind::Resource);
    }
}
