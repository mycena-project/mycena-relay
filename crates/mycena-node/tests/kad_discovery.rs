// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Kademlia DHT integration tests.

use libp2p::identity;
use libp2p::relay::client;
use mycena_core::bootstrap::{
    peer_id_from_multiaddr, standard_bootstrap_multiaddrs, RELAY_PEER_ID,
};
use mycena_core::mesh_profile::MeshProfile;
use mycena_core::transport::ConnectivityMode;
use mycena_node::behaviour::MeshBehaviour;
use mycena_node::connectivity::{kad_action_for_mode, KadConnectivityAction};

#[test]
fn peer_id_from_multiaddr_parses_bootstrap() {
    for addr in standard_bootstrap_multiaddrs() {
        if !addr.contains("/tcp/") {
            continue;
        }
        let peer_id = peer_id_from_multiaddr(&addr).expect("bootstrap addr must contain /p2p/");
        assert!(
            peer_id.starts_with("12D3KooW"),
            "valid libp2p peer id: {peer_id}"
        );
    }
    assert!(
        standard_bootstrap_multiaddrs()
            .iter()
            .any(|a| a.contains(RELAY_PEER_ID)),
        "fly relay must remain in standard bootstrap"
    );
}

#[tokio::test]
async fn mesh_behaviour_includes_kad() {
    let key = identity::Keypair::generate_ed25519();
    let (_relay_transport, relay_behaviour) = client::new(key.public().to_peer_id());
    let _behaviour = MeshBehaviour::new(&key, relay_behaviour, MeshProfile::Default);
}

#[test]
fn kad_paused_in_mesh_only_mode() {
    assert_eq!(
        kad_action_for_mode(ConnectivityMode::MeshOnly),
        KadConnectivityAction::Pause
    );
}

#[test]
fn kad_active_on_internet_mode() {
    assert_eq!(
        kad_action_for_mode(ConnectivityMode::Internet),
        KadConnectivityAction::Activate
    );
}

#[test]
fn kad_bootstrap_addrs_present() {
    assert!(
        standard_bootstrap_multiaddrs().len() >= 6,
        "multi-relay bootstrap required for kad"
    );
}
