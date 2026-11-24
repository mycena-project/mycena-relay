// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! CRDT primitives for offline-first mesh state (contacts, groups, marketplace stock).

mod contact;
mod group;
mod hlc;
mod lww;
mod or_set;
mod pn_counter;

pub use contact::{ContactDocument, ContactStatusWire};
pub use group::GroupDocument;
pub use hlc::{stable_actor_hash, HlcClock, HlcTimestamp};
pub use lww::LwwRegister;
pub use or_set::{OrSet, OrTag};
pub use pn_counter::{MarketplaceStock, PnCounter, StockConflict, StockQuantity, StockResolution};
