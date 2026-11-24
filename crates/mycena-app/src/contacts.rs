// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use mycena_l0_facade::crdt::{ContactDocument, ContactStatusWire, HlcTimestamp};
use mycena_l0_facade::data::now_ms;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContactStatus {
    Pending,
    Accepted,
    Blocked,
}

impl From<ContactStatusWire> for ContactStatus {
    fn from(w: ContactStatusWire) -> Self {
        match w {
            ContactStatusWire::Pending => Self::Pending,
            ContactStatusWire::Accepted => Self::Accepted,
            ContactStatusWire::Blocked => Self::Blocked,
        }
    }
}

impl From<ContactStatus> for ContactStatusWire {
    fn from(s: ContactStatus) -> Self {
        match s {
            ContactStatus::Pending => Self::Pending,
            ContactStatus::Accepted => Self::Accepted,
            ContactStatus::Blocked => Self::Blocked,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub peer_id: String,
    pub display_name: String,
    pub added_at_ms: u64,
    pub status: ContactStatus,
    /// Stable account id from signed invite / identity rotation (SD-190).
    #[serde(default)]
    pub account_id: Option<String>,
    /// Recent transport peer ids during rotation transition (SD-190 dial window).
    #[serde(default)]
    pub previous_peer_ids: Vec<String>,
    /// CRDT document (LWW per field + OR-Set for previous peer ids). Omitted on legacy rows until migrated.
    #[serde(default)]
    pub crdt: Option<ContactDocument>,
}

impl Contact {
    pub fn new(peer_id: String, display_name: String, status: ContactStatus) -> Self {
        Self {
            peer_id,
            display_name,
            added_at_ms: now_ms(),
            status,
            account_id: None,
            previous_peer_ids: Vec::new(),
            crdt: None,
        }
    }

    pub fn ensure_crdt(&mut self, actor_id: &str) {
        if self.crdt.is_some() {
            return;
        }
        self.crdt = Some(ContactDocument::bootstrap_legacy(
            self.peer_id.clone(),
            self.display_name.clone(),
            self.added_at_ms,
            ContactStatusWire::from(self.status),
            self.account_id.clone(),
            self.previous_peer_ids.clone(),
            actor_id,
        ));
    }

    pub fn sync_from_crdt(&mut self) {
        let Some(doc) = &self.crdt else {
            return;
        };
        self.display_name = doc.materialized_display_name().to_string();
        self.status = doc.materialized_status().into();
        self.added_at_ms = doc.materialized_added_at_ms();
        self.account_id = doc.materialized_account_id().map(str::to_string);
        self.previous_peer_ids = doc.materialized_previous_peers();
    }

    pub fn apply_crdt_update<F>(&mut self, actor_id: &str, ts: HlcTimestamp, update: F)
    where
        F: FnOnce(&mut ContactDocument),
    {
        self.ensure_crdt(actor_id);
        if let Some(doc) = self.crdt.as_mut() {
            update(doc);
        }
        let _ = ts;
        self.sync_from_crdt();
    }
}
