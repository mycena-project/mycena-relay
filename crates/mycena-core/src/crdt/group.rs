// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Group CRDT — OR-Set membership, LWW group name.

use super::hlc::{HlcClock, HlcTimestamp};
use super::lww::LwwRegister;
use super::or_set::OrSet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDocument {
    pub id: String,
    pub name: LwwRegister<String>,
    pub key: [u8; 32],
    pub members: OrSet<String>,
    pub created_at_ms: LwwRegister<u64>,
}

impl GroupDocument {
    pub fn new(
        id: String,
        name: String,
        key: [u8; 32],
        created_at_ms: u64,
        ts: HlcTimestamp,
    ) -> Self {
        Self {
            id: id.clone(),
            name: LwwRegister::new(name, ts),
            key,
            members: OrSet::new(&id),
            created_at_ms: LwwRegister::new(created_at_ms, ts),
        }
    }

    pub fn bootstrap_legacy(
        id: String,
        name: String,
        key: [u8; 32],
        members: Vec<String>,
        created_at_ms: u64,
        actor_id: &str,
    ) -> Self {
        let ts = HlcTimestamp {
            wall_ms: created_at_ms,
            logical: 0,
            actor: super::hlc::stable_actor_hash(actor_id),
        };
        let mut doc = Self::new(id.clone(), name, key, created_at_ms, ts);
        doc.members = OrSet::new(actor_id);
        for m in members {
            doc.members.add(m);
        }
        doc
    }

    pub fn merge(&mut self, other: &Self, clock: &mut HlcClock) {
        if other.id != self.id || other.key != self.key {
            return;
        }
        clock.observe(other.name.ts);
        self.name.merge(&other.name);
        clock.observe(other.created_at_ms.ts);
        self.created_at_ms.merge(&other.created_at_ms);
        self.members.merge(&other.members);
    }

    pub fn add_member(&mut self, peer_id: String) {
        if peer_id.is_empty() || self.members.contains(&peer_id) {
            return;
        }
        self.members.add(peer_id);
    }

    pub fn remove_member(&mut self, peer_id: &str) {
        self.members.remove(&peer_id.to_string());
    }

    pub fn materialized_name(&self) -> &str {
        self.name.read()
    }

    pub fn materialized_members(&self) -> Vec<String> {
        self.members.elements()
    }

    pub fn materialized_created_at_ms(&self) -> u64 {
        *self.created_at_ms.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_membership_or_set_survives_concurrent_adds() {
        let key = [7u8; 32];
        let ts = HlcTimestamp {
            wall_ms: 1,
            logical: 0,
            actor: 1,
        };
        let mut ga = GroupDocument::new("g1".into(), "Team".into(), key, 1, ts);
        ga.add_member("peer-a".into());
        let mut gb = GroupDocument::new("g1".into(), "Team".into(), key, 1, ts);
        gb.add_member("peer-b".into());
        let mut clock = HlcClock::new("x");
        ga.merge(&gb, &mut clock);
        assert_eq!(ga.materialized_members().len(), 2);
    }

    #[test]
    fn group_remove_is_observed() {
        let key = [1u8; 32];
        let ts = HlcTimestamp {
            wall_ms: 1,
            logical: 0,
            actor: 1,
        };
        let mut g = GroupDocument::new("g".into(), "G".into(), key, 1, ts);
        g.add_member("p1".into());
        g.remove_member("p1");
        assert!(g.materialized_members().is_empty());
    }
}
