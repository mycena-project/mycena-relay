// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Mesh consistency model — tags what each state-changing operation may assume.
//!
//! Mycena is partition-tolerant and offline-first. **Strong** (global,
//! linearizable) consistency is not available on the mesh; the types here make
//! that explicit so callers cannot accidentally treat a local sled view as
//! authoritative for the whole network.

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

/// Consistency guarantee for a read or write on mesh-visible state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consistency {
    /// Single device only; may never leave this node (or leaves under a strict propagation cap).
    Local,
    /// CRDT / mergeable; peers converge asynchronously; stale reads are normal.
    EventuallyConsistent,
    /// Global linearizable view — **not provided** by the mesh (documented forbidden assumption).
    Strong,
    /// Funds or settlement-sensitive writes; spend only after local witness quorum / policy.
    SettlementRequired,
}

mod sealed {
    pub trait Sealed {}
}

/// Marker for compile-time consistency tagging on views and operations.
pub trait ConsistencyModel: sealed::Sealed {
    const LEVEL: Consistency;
}

/// Local-only state (UI prefs, SOS queue before send, install preview).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct LocalModel;

/// Mergeable mesh state (messages, contacts, bloom sync, invites).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct EventuallyConsistentModel;

/// Witness-quorum settled ledger reads used to authorize spends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SettlementRequiredModel;

impl sealed::Sealed for LocalModel {}
impl sealed::Sealed for EventuallyConsistentModel {}
impl sealed::Sealed for SettlementRequiredModel {}

impl ConsistencyModel for LocalModel {
    const LEVEL: Consistency = Consistency::Local;
}

impl ConsistencyModel for EventuallyConsistentModel {
    const LEVEL: Consistency = Consistency::EventuallyConsistent;
}

impl ConsistencyModel for SettlementRequiredModel {
    const LEVEL: Consistency = Consistency::SettlementRequired;
}

/// Phantom wrapper so APIs can require a specific consistency level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateView<C: ConsistencyModel> {
    _marker: PhantomData<C>,
}

impl<C: ConsistencyModel> Default for StateView<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: ConsistencyModel> StateView<C> {
    pub const fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    pub const fn level() -> Consistency {
        C::LEVEL
    }
}

/// Compile-time metadata for a state-changing operation (see ADR 006).
#[derive(Debug, Clone, Copy)]
pub struct StateOp<C: ConsistencyModel> {
    pub name: &'static str,
    pub description: &'static str,
    _marker: PhantomData<C>,
}

impl<C: ConsistencyModel> StateOp<C> {
    pub const fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            _marker: PhantomData,
        }
    }

    pub const fn consistency(&self) -> Consistency {
        C::LEVEL
    }
}

// --- Operation catalogue (ADR 006) ---

pub const OP_DIRECT_MESSAGE_SEND: StateOp<EventuallyConsistentModel> = StateOp::new(
    "direct_message.send",
    "Envelope relay; delivery best-effort, no global ordering",
);

pub const OP_GOSSIP_BULLETIN_PUBLISH: StateOp<EventuallyConsistentModel> = StateOp::new(
    "bulletin.publish",
    "Scoped gossip; peers merge by timestamp / seen cache",
);

pub const OP_CONTACT_UPSERT: StateOp<EventuallyConsistentModel> = StateOp::new(
    "contact.upsert",
    "Contact book merge: LWW-Register per field (HLC timestamps); OR-Set for previous peer ids",
);

pub const OP_GROUP_MEMBERSHIP: StateOp<EventuallyConsistentModel> = StateOp::new(
    "group.membership",
    "Group membership OR-Set (add/remove with tag compaction); group name LWW via HLC",
);

pub const OP_MARKETPLACE_STOCK: StateOp<EventuallyConsistentModel> = StateOp::new(
    "marketplace.stock",
    "Listing stock PN-Counter — conflict surfaced on oversell, no silent LWW merge",
);

pub const OP_INVITE_PARSE: StateOp<EventuallyConsistentModel> = StateOp::new(
    "invite.parse",
    "QR / deep-link TOFU; local trust seed until PeerInfo confirms",
);

pub const OP_BLOOM_SYNC: StateOp<EventuallyConsistentModel> = StateOp::new(
    "sync.bloom",
    "Bloom filter exchange; anti-entropy, not authoritative inventory",
);

pub const OP_RESOURCE_MAP_BROADCAST: StateOp<LocalModel> = StateOp::new(
    "resource_map.broadcast",
    "Local compose + MaximumPropagation; ephemeral geohash resource pins",
);

pub const OP_VOLUNTEER_TASK_BROADCAST: StateOp<LocalModel> = StateOp::new(
    "volunteer.task_broadcast",
    "Local compose + MaximumPropagation; volunteer slot assignment",
);

pub const OP_SOS_BROADCAST: StateOp<LocalModel> = StateOp::new(
    "sos.broadcast",
    "Local compose + MaximumPropagation policy; no settlement",
);

pub const OP_MINIAPP_INSTALL: StateOp<LocalModel> = StateOp::new(
    "miniapp.install",
    "Local bundle verify + store write; listing gossip is separate",
);

pub const OP_COIN_TRANSFER: StateOp<SettlementRequiredModel> = StateOp::new(
    "coin.transfer",
    "Signed TX; spend only settled local balance, propagate for witness quorum",
);

pub const OP_COIN_REFILL: StateOp<SettlementRequiredModel> = StateOp::new(
    "coin.refill",
    "Cold→hot signed refill; cold wallet checks settled balance before sign",
);

pub const OP_COIN_BALANCE_QUERY: StateOp<EventuallyConsistentModel> = StateOp::new(
    "coin.balance_query",
    "Peer hint only; must not authorize spends",
);

/// Strong global views are intentionally uninhabited in production mesh code.
#[derive(Debug, Clone, Copy)]
pub struct StrongConsistencyForbidden {
    _private: (),
}

/// Documented escape hatch for ADR/tests — **no mesh API returns this**.
pub const fn strong_consistency_not_on_mesh() -> StrongConsistencyForbidden {
    StrongConsistencyForbidden { _private: () }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn operation_levels_match_adr() {
        assert_eq!(
            OP_DIRECT_MESSAGE_SEND.consistency(),
            Consistency::EventuallyConsistent
        );
        assert_eq!(OP_SOS_BROADCAST.consistency(), Consistency::Local);
        assert_eq!(
            OP_COIN_TRANSFER.consistency(),
            Consistency::SettlementRequired
        );
        assert_eq!(
            OP_CONTACT_UPSERT.consistency(),
            Consistency::EventuallyConsistent
        );
    }

    #[test]
    fn strong_is_not_a_mesh_model() {
        // Strong exists in the enum for documentation only; no ConsistencyModel impl.
        assert_eq!(Consistency::Strong as u8, 2);
        let _ = strong_consistency_not_on_mesh();
    }
}
