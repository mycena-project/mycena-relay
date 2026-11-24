// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use mycena_l0_facade::crdt::GroupDocument;
use mycena_l0_facade::data::now_ms;
use rand::RngCore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub key: [u8; 32],
    pub members: Vec<String>,
    pub created_at_ms: u64,
    #[serde(default)]
    pub crdt: Option<GroupDocument>,
}

impl Group {
    pub fn new(name: String) -> Self {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let id = hex::encode(blake3::hash(&key).as_bytes());
        Self {
            id,
            name,
            key,
            members: Vec::new(),
            created_at_ms: now_ms(),
            crdt: None,
        }
    }

    pub fn export_invite(&self) -> String {
        serde_json::json!({
            "type": "mycena-group-invite",
            "version": 1,
            "id": self.id,
            "name": self.name,
            "key": hex::encode(self.key),
        })
        .to_string()
    }

    pub fn from_invite(json: &str) -> anyhow::Result<Self> {
        let v: serde_json::Value = serde_json::from_str(json)?;
        let key_hex = v["key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing key"))?;
        let key_bytes = hex::decode(key_hex)?;
        let key: [u8; 32] = key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid key length"))?;
        Ok(Self {
            id: v["id"].as_str().unwrap_or("").to_string(),
            name: v["name"].as_str().unwrap_or("Group").to_string(),
            key,
            members: Vec::new(),
            created_at_ms: now_ms(),
            crdt: None,
        })
    }

    pub fn ensure_crdt(&mut self, actor_id: &str) {
        if self.crdt.is_some() {
            return;
        }
        self.crdt = Some(GroupDocument::bootstrap_legacy(
            self.id.clone(),
            self.name.clone(),
            self.key,
            self.members.clone(),
            self.created_at_ms,
            actor_id,
        ));
    }

    pub fn sync_from_crdt(&mut self) {
        let Some(doc) = &self.crdt else {
            return;
        };
        self.name = doc.materialized_name().to_string();
        self.members = doc.materialized_members();
        self.created_at_ms = doc.materialized_created_at_ms();
    }

    pub fn record_peer_seen(&mut self, peer_id: String) {
        if peer_id.is_empty() {
            return;
        }
        if !self.members.iter().any(|p| p == &peer_id) {
            self.members.push(peer_id);
        }
    }

    pub fn record_peer_seen_crdt(&mut self, peer_id: String, actor_id: &str) {
        self.ensure_crdt(actor_id);
        if let Some(doc) = self.crdt.as_mut() {
            doc.add_member(peer_id);
        }
        self.sync_from_crdt();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn group_invite_roundtrip() {
        let group = Group::new("Test Group".into());
        let invite = group.export_invite();
        let restored = Group::from_invite(&invite).expect("parse");
        assert_eq!(restored.key, group.key);
        assert_eq!(restored.name, "Test Group");
    }

    #[test]
    fn group_record_peer_seen_dedupes() {
        let mut g = Group::new("G".into());
        g.record_peer_seen("peer-a".into());
        g.record_peer_seen("peer-b".into());
        g.record_peer_seen("peer-a".into());
        assert_eq!(g.members, vec!["peer-a".to_string(), "peer-b".to_string()]);
    }

    #[test]
    fn group_or_set_membership_via_crdt() {
        let mut g = Group::new("G".into());
        g.record_peer_seen_crdt("peer-a".into(), "local");
        g.record_peer_seen_crdt("peer-b".into(), "local");
        g.record_peer_seen_crdt("peer-a".into(), "local");
        assert_eq!(g.members.len(), 2);
    }
}
