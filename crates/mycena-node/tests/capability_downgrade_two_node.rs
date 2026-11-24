// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Two-node capability negotiation: legacy peer forces ratchet + custody downgrade.

use async_trait::async_trait;
use libp2p::identity;
use mycena_core::crypto::ratchet::{self, E2E_VERSION_LEGACY, E2E_VERSION_RATCHET};
use mycena_core::e2e_direct_wrap;
use mycena_core::protocol_capabilities::{custody, ratchet as cap_ratchet, ProtocolCapabilities};
use mycena_core::transport::{MeshTransport, TransportEvent, WireMessage};
use mycena_node::{IdentityBootstrap, NodeCommand, NodeConfig, NodeRunner};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::{mpsc, Mutex};

struct LinkedTransport {
    local_id: String,
    keypair: identity::Keypair,
    connected: HashSet<String>,
    inbox: mpsc::UnboundedReceiver<TransportEvent>,
    inbox_tx: mpsc::UnboundedSender<TransportEvent>,
    routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<TransportEvent>>>>,
}

impl LinkedTransport {
    fn new(routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<TransportEvent>>>>) -> Self {
        let keypair = identity::Keypair::generate_ed25519();
        let local_id = keypair.public().to_peer_id().to_string();
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        Self {
            local_id,
            keypair,
            connected: HashSet::new(),
            inbox,
            inbox_tx,
            routes,
        }
    }
}

fn peer_id_from_multiaddr(addr: &str) -> Option<String> {
    let id = addr.rsplit("/p2p/").next()?.trim();
    if id.starts_with("12D3Koo") && !id.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

async fn link_transports(a: &LinkedTransport, b: &LinkedTransport) {
    let mut routes = a.routes.lock().await;
    routes.insert(a.local_id.clone(), a.inbox_tx.clone());
    routes.insert(b.local_id.clone(), b.inbox_tx.clone());
}

#[async_trait]
impl MeshTransport for LinkedTransport {
    fn local_peer_id(&self) -> String {
        self.local_id.clone()
    }

    fn known_peers(&self) -> Vec<String> {
        self.connected.iter().cloned().collect()
    }

    fn local_keypair(&self) -> Option<identity::Keypair> {
        Some(self.keypair.clone())
    }

    async fn dial_peer(&mut self, multiaddr: String) -> anyhow::Result<()> {
        self.remember_and_dial(multiaddr).await
    }

    async fn remember_and_dial(&mut self, multiaddr: String) -> anyhow::Result<()> {
        if let Some(peer) = peer_id_from_multiaddr(&multiaddr) {
            self.connected.insert(peer.clone());
            let _ = self.inbox_tx.send(TransportEvent::PeerUp {
                peer_id: peer.clone(),
            });
            let routes = self.routes.lock().await;
            if let Some(remote_tx) = routes.get(&peer) {
                let _ = remote_tx.send(TransportEvent::PeerUp {
                    peer_id: self.local_id.clone(),
                });
            }
        }
        Ok(())
    }

    fn redial_stored_targets(&mut self) {}

    async fn send_direct(&mut self, to_peer: String, message: WireMessage) -> anyhow::Result<()> {
        let routes = self.routes.lock().await;
        if let Some(tx) = routes.get(&to_peer) {
            tx.send(TransportEvent::DirectReceived {
                from_peer: self.local_id.clone(),
                message,
            })?;
        }
        Ok(())
    }

    async fn publish_scoped(&mut self, _scope: String, _payload: Vec<u8>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn subscribe_scope(&mut self, _scope: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn unsubscribe_scope(&mut self, _scope: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn next_event(&mut self) -> anyhow::Result<TransportEvent> {
        self.inbox
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("linked transport inbox closed"))
    }
}

async fn spawn_linked_pair(
    alice_caps: Option<ProtocolCapabilities>,
    bob_caps: Option<ProtocolCapabilities>,
) -> (
    mycena_node::NodeHandle,
    mycena_node::NodeHandle,
    String,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let routes = Arc::new(Mutex::new(HashMap::new()));

    let bob_dir = tempdir().expect("bob tempdir");
    let mut bob_config = NodeConfig::with_defaults(bob_dir.path().to_str().expect("path"));
    bob_config.protocol_capabilities_override = bob_caps;
    let bob_transport = LinkedTransport::new(routes.clone());
    let bob_id = bob_transport.local_id.clone();

    let alice_dir = tempdir().expect("alice tempdir");
    let mut alice_config = NodeConfig::with_defaults(alice_dir.path().to_str().expect("path"));
    alice_config.protocol_capabilities_override = alice_caps;
    let alice_transport = LinkedTransport::new(routes.clone());
    let alice_id = alice_transport.local_id.clone();

    link_transports(&bob_transport, &alice_transport).await;

    let (bob_runner, bob_handle) = NodeRunner::new_with_transport(
        bob_config,
        Box::new(bob_transport),
        IdentityBootstrap::ephemeral(),
    )
    .expect("bob runner");
    let bob_task = tokio::spawn(async move { bob_runner.run().await });

    let (alice_runner, alice_handle) = NodeRunner::new_with_transport(
        alice_config,
        Box::new(alice_transport),
        IdentityBootstrap::ephemeral(),
    )
    .expect("alice runner");
    let alice_task = tokio::spawn(async move { alice_runner.run().await });

    let bob_addr = format!("/p2p/{bob_id}");
    alice_handle
        .send(NodeCommand::AddBootstrapPeer {
            multiaddr: bob_addr,
        })
        .await
        .expect("dial bob");

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    (
        alice_handle,
        bob_handle,
        alice_id,
        bob_id,
        alice_task,
        bob_task,
    )
}

#[tokio::test]
async fn legacy_bob_downgrades_ratchet_and_custody_on_alice() {
    let current = ProtocolCapabilities::current(true, true);
    let legacy = ProtocolCapabilities::legacy_client();

    let (alice, _bob, _alice_id, bob_id, alice_task, bob_task) =
        spawn_linked_pair(Some(current), Some(legacy)).await;

    let negotiated = alice
        .query_negotiated_protocol(&bob_id)
        .await
        .expect("query")
        .expect("caps stored after PeerInfo");

    assert_eq!(negotiated.ratchet, cap_ratchet::LEGACY_X25519);
    assert_eq!(negotiated.custody, custody::NONE);
    assert!(!negotiated.ratchet_v1_allowed());
    assert!(!negotiated.custody_v1_allowed());
    assert!(!negotiated.payment_voucher_v1_allowed());
    assert!(!negotiated.relay_diversity_allowed());

    let wrapped = e2e_direct_wrap::wrap_inner("alice", b"legacy-path-check");
    let (_payload, e2e_version) = alice
        .encrypt_e2e_payload(&bob_id, wrapped)
        .await
        .expect("encrypt");
    assert_eq!(e2e_version, E2E_VERSION_LEGACY);

    alice_task.abort();
    bob_task.abort();
}

#[tokio::test]
async fn both_current_enables_ratchet_v1() {
    let current = ProtocolCapabilities::current(true, true);

    let (alice, _bob, _alice_id, bob_id, alice_task, bob_task) =
        spawn_linked_pair(Some(current), Some(current)).await;

    let negotiated = alice
        .query_negotiated_protocol(&bob_id)
        .await
        .expect("query")
        .expect("caps stored");

    assert!(negotiated.ratchet_v1_allowed());
    assert!(negotiated.custody_v1_allowed());
    assert!(negotiated.payment_voucher_v1_allowed());
    assert!(negotiated.relay_diversity_allowed());

    let wrapped = e2e_direct_wrap::wrap_inner("alice", b"ratchet-path");
    let (encrypted_payload, e2e_version) = alice
        .encrypt_e2e_payload(&bob_id, wrapped)
        .await
        .expect("encrypt");
    assert_eq!(e2e_version, E2E_VERSION_RATCHET);
    assert_eq!(
        ratchet::wire_e2e_version(&encrypted_payload, e2e_version),
        1
    );

    alice_task.abort();
    bob_task.abort();
}

#[tokio::test]
async fn legacy_alice_downgrades_when_talking_to_current_bob() {
    let current = ProtocolCapabilities::current(true, true);
    let legacy = ProtocolCapabilities::legacy_client();

    let (alice, _bob, _alice_id, bob_id, alice_task, bob_task) =
        spawn_linked_pair(Some(legacy), Some(current)).await;

    let negotiated = alice
        .query_negotiated_protocol(&bob_id)
        .await
        .expect("query")
        .expect("caps stored");

    assert!(!negotiated.ratchet_v1_allowed());
    assert!(!negotiated.custody_v1_allowed());

    let wrapped = e2e_direct_wrap::wrap_inner("alice", b"downgrade-from-legacy-local");
    let (_payload, e2e_version) = alice
        .encrypt_e2e_payload(&bob_id, wrapped)
        .await
        .expect("encrypt");
    assert_eq!(e2e_version, E2E_VERSION_LEGACY);

    alice_task.abort();
    bob_task.abort();
}
