// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Contact book CRDT — LWW register per field (HLC timestamps).

use super::hlc::{HlcClock, HlcTimestamp};
use super::lww::LwwRegister;
use super::or_set::OrSet;
use serde::{Deserialize, Serialize};

/// Wire / storage status for contact book (matches app `ContactStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum ContactStatusWire {
    #[default]
    Pending = 0,
    Accepted = 1,
    Blocked = 2,
}

impl ContactStatusWire {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Pending),
            1 => Some(Self::Accepted),
            2 => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// Mergeable contact record keyed by current transport `peer_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactDocument {
    pub peer_id: String,
    pub display_name: LwwRegister<String>,
    pub status: LwwRegister<ContactStatusWire>,
    pub added_at_ms: LwwRegister<u64>,
    pub account_id: LwwRegister<Option<String>>,
    pub previous_peer_ids: OrSet<String>,
}

impl ContactDocument {
    pub fn new(
        peer_id: String,
        display_name: String,
        status: ContactStatusWire,
        ts: HlcTimestamp,
    ) -> Self {
        Self {
            peer_id: peer_id.clone(),
            display_name: LwwRegister::new(display_name, ts),
            status: LwwRegister::new(status, ts),
            added_at_ms: LwwRegister::new(ts.wall_ms, ts),
            account_id: LwwRegister::new(None, ts),
            previous_peer_ids: OrSet::new(&peer_id),
        }
    }

    pub fn bootstrap_legacy(
        peer_id: String,
        display_name: String,
        added_at_ms: u64,
        status: ContactStatusWire,
        account_id: Option<String>,
        previous_peer_ids: Vec<String>,
        actor_id: &str,
    ) -> Self {
        let ts = HlcTimestamp {
            wall_ms: added_at_ms,
            logical: 0,
            actor: super::hlc::stable_actor_hash(actor_id),
        };
        let mut doc = Self::new(peer_id.clone(), display_name, status, ts);
        if let Some(aid) = account_id {
            doc.account_id = LwwRegister::new(Some(aid), ts);
        }
        doc.previous_peer_ids = OrSet::new(actor_id);
        for prev in previous_peer_ids {
            doc.previous_peer_ids.add(prev);
        }
        doc
    }

    pub fn merge(&mut self, other: &Self, clock: &mut HlcClock) {
        if other.peer_id != self.peer_id {
            return;
        }
        clock.observe(other.display_name.ts);
        self.display_name.merge(&other.display_name);
        clock.observe(other.status.ts);
        self.status.merge(&other.status);
        clock.observe(other.added_at_ms.ts);
        self.added_at_ms.merge(&other.added_at_ms);
        clock.observe(other.account_id.ts);
        self.account_id.merge(&other.account_id);
        self.previous_peer_ids.merge(&other.previous_peer_ids);
    }

    pub fn set_display_name(&mut self, name: String, ts: HlcTimestamp) {
        self.display_name.assign(name, ts);
    }

    pub fn set_status(&mut self, status: ContactStatusWire, ts: HlcTimestamp) {
        self.status.assign(status, ts);
    }

    pub fn set_account_id(&mut self, account_id: Option<String>, ts: HlcTimestamp) {
        self.account_id.assign(account_id, ts);
    }

    pub fn note_previous_peer(&mut self, peer: String) {
        if peer.is_empty() || peer == self.peer_id {
            return;
        }
        if !self.previous_peer_ids.contains(&peer) {
            self.previous_peer_ids.add(peer);
        }
    }

    pub fn materialized_display_name(&self) -> &str {
        self.display_name.read()
    }

    pub fn materialized_status(&self) -> ContactStatusWire {
        *self.status.read()
    }

    pub fn materialized_added_at_ms(&self) -> u64 {
        *self.added_at_ms.read()
    }

    pub fn materialized_account_id(&self) -> Option<&str> {
        self.account_id.read().as_deref()
    }

    pub fn materialized_previous_peers(&self) -> Vec<String> {
        self.previous_peer_ids.elements()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_lww_field_merge_uses_hlc_not_wall_only() {
        let mut clock_a = HlcClock::new("device-a");
        let ts_a = clock_a.tick(1000);
        let mut doc_a = ContactDocument::new(
            "peer-1".into(),
            "Alice".into(),
            ContactStatusWire::Accepted,
            ts_a,
        );
        let ts_b = HlcTimestamp {
            wall_ms: 1000,
            logical: 2,
            actor: super::super::hlc::stable_actor_hash("device-b"),
        };
        let doc_b = ContactDocument::new(
            "peer-1".into(),
            "Bob".into(),
            ContactStatusWire::Pending,
            ts_b,
        );
        doc_a.merge(&doc_b, &mut clock_a);
        assert_eq!(doc_a.materialized_display_name(), "Bob");
    }
}
