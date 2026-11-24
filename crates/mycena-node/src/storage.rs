// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use anyhow::Context;
use mycena_core::data::now_ms;
use mycena_core::transport::{DirectMessage, MessageStore, StoreStats};
use std::time::Duration;

const ORDER_TREE: &str = "message_order";
const TS_PREFIX_LEN: usize = 8;
const MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Local DTN store. sled 0.34 is unmaintained; a redb/sqlite migration is tracked
/// as storage-format work, not a silent swap (wire/on-disk compatibility).
pub struct SledMessageStore {
    db: sled::Db,
    order: sled::Tree,
}

/// Sortable sled key: big-endian timestamp (8 bytes) || message_id (UTF-8).
fn order_key(ts: u64, message_id: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(TS_PREFIX_LEN + message_id.len());
    key.extend_from_slice(&ts.to_be_bytes());
    key.extend_from_slice(message_id.as_bytes());
    key
}

fn timestamp_from_order_key(key: &[u8]) -> u64 {
    if key.len() < TS_PREFIX_LEN {
        return 0;
    }
    let mut buf = [0u8; TS_PREFIX_LEN];
    buf.copy_from_slice(&key[..TS_PREFIX_LEN]);
    u64::from_be_bytes(buf)
}

fn message_id_from_order_key(key: &[u8]) -> Option<&str> {
    if key.len() <= TS_PREFIX_LEN {
        return None;
    }
    std::str::from_utf8(&key[TS_PREFIX_LEN..]).ok()
}

impl SledMessageStore {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(Some(5000))
            .open()
            .with_context(|| format!("opening sled db at {path}"))?;
        let order = db
            .open_tree(ORDER_TREE)
            .with_context(|| format!("opening sled order tree at {path}"))?;
        let store = Self { db, order };
        store.migrate_legacy_messages()?;
        Ok(store)
    }

    /// One-time rebuild when messages exist but the order tree is empty (pre-fix stores).
    fn migrate_legacy_messages(&self) -> anyhow::Result<()> {
        if !self.order.is_empty() {
            return Ok(());
        }
        let mut batch = sled::Batch::default();
        let mut entries = Vec::new();
        for item in self.db.iter() {
            let (_key, value) = item?;
            let msg: DirectMessage = bincode::deserialize(&value)?;
            entries.push((msg.envelope.created_at_ms, msg.envelope.id.0.clone()));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut order_ts = 0u64;
        for (created_at, id) in &entries {
            order_ts = (*created_at).max(order_ts.saturating_add(1));
            batch.insert(order_key(order_ts, id), b"");
        }
        if !entries.is_empty() {
            self.order.apply_batch(batch)?;
        }
        Ok(())
    }

    pub async fn gc_expired(&self) -> anyhow::Result<usize> {
        let db = self.db.clone();
        let order = self.order.clone();
        tokio::task::spawn_blocking(move || {
            let now = now_ms();
            let cutoff = now.saturating_sub(MAX_AGE_MS);
            let mut deleted = 0usize;
            let mut order_batch = sled::Batch::default();
            let mut db_batch = sled::Batch::default();
            for item in order.range(..cutoff.to_be_bytes()) {
                let (key, _) = item?;
                let id = message_id_from_order_key(&key)
                    .with_context(|| format!("invalid order key: {key:?}"))?;
                order_batch.remove(&key);
                db_batch.remove(id.as_bytes());
                deleted += 1;
            }
            order.apply_batch(order_batch)?;
            db.apply_batch(db_batch)?;
            Ok(deleted)
        })
        .await?
    }

    pub async fn stats(&self) -> StoreStats {
        let oldest_ms = match self.order.first() {
            Ok(Some((k, _))) => timestamp_from_order_key(&k),
            _ => 0,
        };
        StoreStats {
            count: self.order.len(),
            oldest_ms,
        }
    }
}

#[async_trait::async_trait]
impl MessageStore for SledMessageStore {
    async fn persist(&self, message: &DirectMessage) -> anyhow::Result<()> {
        let key = message.envelope.id.0.clone();
        let encoded = bincode::serialize(message)?;
        let db = self.db.clone();
        let order = self.order.clone();
        tokio::task::spawn_blocking(move || {
            let ts = {
                let last_ts = match order.last() {
                    Ok(Some((k, _))) => timestamp_from_order_key(&k),
                    Ok(None) => 0,
                    Err(e) => return Err(e.into()),
                };
                now_ms().max(last_ts.saturating_add(1))
            };
            db.insert(key.as_bytes(), encoded)?;
            order.insert(order_key(ts, &key), b"")?;
            Ok(())
        })
        .await?
    }

    async fn recent(&self, limit: usize) -> anyhow::Result<Vec<DirectMessage>> {
        let mut out = Vec::new();
        for item in self.order.iter().rev().take(limit) {
            let (key, _) = item?;
            let Some(id) = message_id_from_order_key(&key) else {
                continue;
            };
            if let Some(value) = self.db.get(id.as_bytes())? {
                let msg: DirectMessage = bincode::deserialize(&value)?;
                out.push(msg);
            }
        }
        Ok(out)
    }

    async fn contains(&self, message_id: &str) -> anyhow::Result<bool> {
        Ok(self.db.contains_key(message_id.as_bytes())?)
    }

    async fn load_by_id(&self, message_id: &str) -> anyhow::Result<Option<DirectMessage>> {
        let value = self.db.get(message_id.as_bytes())?;
        if let Some(bytes) = value {
            let msg: DirectMessage = bincode::deserialize(&bytes)?;
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }

    async fn list_ids_window(&self, window: Duration) -> anyhow::Result<Vec<String>> {
        let cutoff = now_ms().saturating_sub(window.as_millis() as u64);
        let start = cutoff.to_be_bytes();
        let mut out = Vec::new();
        for item in self.order.range(start..) {
            let (key, _) = item?;
            let Some(id) = message_id_from_order_key(&key) else {
                continue;
            };
            out.push(id.to_string());
        }
        Ok(out)
    }

    async fn gc_expired(&self) -> anyhow::Result<usize> {
        SledMessageStore::gc_expired(self).await
    }

    async fn stats(&self) -> anyhow::Result<StoreStats> {
        Ok(SledMessageStore::stats(self).await)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::data::Envelope;

    fn mk_msg(body: &str) -> DirectMessage {
        DirectMessage {
            envelope: Envelope::new(
                "a".to_string(),
                Some("b".to_string()),
                body.as_bytes().to_vec(),
            ),
            body: body.to_string(),
        }
    }

    fn set_order_timestamp(store: &SledMessageStore, message_id: &str, ts: u64) {
        let mut to_remove = Vec::new();
        for item in store.order.iter() {
            let (key, _) = item.expect("order iter");
            if message_id_from_order_key(&key) == Some(message_id) {
                to_remove.push(key.to_vec());
            }
        }
        for key in to_remove {
            store.order.remove(key).expect("remove old order key");
        }
        store
            .order
            .insert(order_key(ts, message_id), b"")
            .expect("insert order key");
    }

    #[tokio::test]
    async fn store_persist_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SledMessageStore::open(dir.path().to_str().expect("path")).expect("open store");
        let msg = mk_msg("roundtrip");
        let id = msg.envelope.id.0.clone();
        store.persist(&msg).await.expect("persist");
        assert!(store.contains(&id).await.expect("contains"));
        let loaded = store.load_by_id(&id).await.expect("load").expect("some");
        assert_eq!(loaded.body, "roundtrip");
    }

    #[tokio::test]
    async fn store_recent_returns_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SledMessageStore::open(dir.path().to_str().expect("path")).expect("open store");
        store.persist(&mk_msg("one")).await.expect("persist1");
        store.persist(&mk_msg("two")).await.expect("persist2");
        let recent = store.recent(2).await.expect("recent");
        assert_eq!(recent.first().expect("first").body, "two");
    }

    #[tokio::test]
    async fn list_ids_window_filters_old_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SledMessageStore::open(dir.path().to_str().expect("path")).expect("open store");
        let old = mk_msg("old");
        let old_id = old.envelope.id.0.clone();
        store.persist(&old).await.expect("persist old");
        set_order_timestamp(&store, &old_id, now_ms().saturating_sub(10_000));
        let new = mk_msg("new");
        let new_id = new.envelope.id.0.clone();
        store.persist(&new).await.expect("persist new");
        let ids = store
            .list_ids_window(Duration::from_secs(1))
            .await
            .expect("window");
        assert!(ids.contains(&new_id));
        assert!(!ids.contains(&old_id));
    }

    #[tokio::test]
    async fn store_order_survives_close_and_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("path");
        let (old_id, new_id);
        {
            let store = SledMessageStore::open(path).expect("open store");
            let old = mk_msg("before_restart");
            old_id = old.envelope.id.0.clone();
            store.persist(&old).await.expect("persist old");
            let new = mk_msg("also_before_restart");
            new_id = new.envelope.id.0.clone();
            store.persist(&new).await.expect("persist new");
            store.db.flush().expect("flush before close");
        }
        {
            let store = SledMessageStore::open(path).expect("reopen store");
            let recent = store.recent(10).await.expect("recent after reopen");
            assert_eq!(recent.len(), 2, "recent must see persisted messages");
            assert_eq!(recent.first().expect("newest").body, "also_before_restart");
            let window = store
                .list_ids_window(Duration::from_secs(3600))
                .await
                .expect("window after reopen");
            assert!(window.contains(&old_id), "old id in window");
            assert!(window.contains(&new_id), "new id in window");
            let stats = store.stats().await;
            assert_eq!(stats.count, 2);
            assert!(stats.oldest_ms > 0);
        }
    }

    #[test]
    fn order_key_sorts_chronologically_in_sled_order() {
        let k1 = order_key(100, "aaa");
        let k2 = order_key(200, "aaa");
        let k3 = order_key(200, "bbb");
        assert!(k1 < k2);
        assert!(k2 < k3);
    }
}
