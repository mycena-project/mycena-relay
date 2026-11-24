// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Federated relay pool selection and failover.

use mycena_core::bootstrap::relay_circuit_multiaddr_for_db;
use mycena_core::relay_pool::RelayPool;
use mycena_core::relay_registry::RelayRegistryManifest;

#[test]
fn federated_circuit_multiaddr_uses_pool_not_hardcoded_peer() {
    let remote = "12D3KooWRemotePeerIdForTestOnly012345678901234";
    let addr = relay_circuit_multiaddr_for_db("", remote, None).expect("circuit");
    assert!(addr.contains("/p2p-circuit/p2p/"));
    assert!(addr.ends_with(remote));
}

#[test]
fn relay_pool_failover_skips_failed_relay() {
    let manifest = RelayRegistryManifest::default_embedded();
    let mut pool = RelayPool::from_registry_manifest(&manifest, false);
    assert!(!pool.is_empty());
    let start = pool.select_index_for("client-seed");
    pool.mark_failure(start);
    pool.mark_failure(start);
    pool.mark_failure(start);
    let after = pool.select_index_for_with_skip("client-seed", &[]);
    if pool.len() > 1 {
        assert_ne!(start, after);
    }
}
