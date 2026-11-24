// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! L0 facade — stable boundary over [`mycena_core`] for app-layer crates.
//!
//! This crate *is* the app-layer surface: it re-exports the core modules hosts
//! need. Prefer `mycena-l0-facade` / `mycena-l1-facade` in FFI and UI crates
//! when the type already exists there. Do not strip these re-exports without a
//! coordinated consumer migration.
//!
//! App and mini-app crates should depend on this crate instead of `mycena-core`
//! directly. Node and transport internals may still use `mycena-core` for ratchet
//! and mesh protocol details.

pub mod crypto;
pub mod secrets;

pub use mycena_core::account_identity;
pub use mycena_core::at_rest::{
    clear_keyring_master, parse_storage_key_hex, validate_wipe_target, wipe_node_directory,
};
pub use mycena_core::bearer_transport;
pub use mycena_core::ble_mesh;
pub use mycena_core::bootstrap;
pub use mycena_core::consistency;
pub use mycena_core::contact_hello;
pub use mycena_core::crdt;
pub use mycena_core::crisis_mode;
pub use mycena_core::data;
pub use mycena_core::e2e_direct_wrap;
pub use mycena_core::energy;
pub use mycena_core::identity_rotation;
pub use mycena_core::information_trust;
pub use mycena_core::invite;
pub use mycena_core::lock_or_recover;
pub use mycena_core::lock_recover;
pub use mycena_core::privacy_shaping;
pub use mycena_core::protocol;
pub use mycena_core::relay_pool;
pub use mycena_core::relay_registry;
pub use mycena_core::relay_status;
pub use mycena_core::release_integrity;
pub use mycena_core::rendezvous;
pub use mycena_core::resource_map;
pub use mycena_core::sos;
pub use mycena_core::volunteer;
pub use mycena_core::webrtc_signaling;

pub use crypto::{
    decrypt_group, encrypt_for, encrypt_group, parse_x25519_public_hex, public_key_from_bytes,
    EncKey,
};
