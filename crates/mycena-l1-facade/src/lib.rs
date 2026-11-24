// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! L1 facade — stable boundary over [`mycena_node`] for app-layer crates.
//!
//! App crates should depend on this crate instead of `mycena-node` directly.
//! Transport internals (`storage`, `ratchet_store`, mesh behaviour) remain private
//! inside `mycena-node`.

pub use mycena_l0_facade::consistency;
pub use mycena_node::connectivity::{
    kad_action_for_mode, ConnectivityMonitor, KadConnectivityAction,
};
pub use mycena_node::secrets::{
    self, bootstrap_identities, encode_signed_invite_v3, load_or_create_enc_keypair,
    IdentityBootstrap, StorageKey,
};
pub use mycena_node::ConnectivityMode;
pub use mycena_node::{
    load_or_create_keypair, load_or_create_keypair_from_env, DirectMessage, HostNodeOptions,
    NodeCommand, NodeConfig, NodeHandle, NodeMetrics, NodeRunner, SendPathError, WireMessage,
};
