// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Two NodeRunner instances: invite v3 ratchet tail → v1 first E2E message.

use async_trait::async_trait;
use libp2p::identity;
use mycena_core::crypto::ratchet::{self, E2E_VERSION_RATCHET};
use mycena_core::data::Priority;
use mycena_core::e2e_direct_wrap;
use mycena_core::invite::{parse_invite, InviteTarget};
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

#[tokio::test]
async fn invite_ratchet_bundle_enables_v1_first_e2e_between_two_nodes() {
    let routes = Arc::new(Mutex::new(HashMap::new()));

    let bob_dir = tempdir().expect("bob tempdir");
    let bob_base = bob_dir.path().to_str().expect("path");
    let bob_transport = LinkedTransport::new(routes.clone());
    let bob_id = bob_transport.local_id.clone();

    let alice_dir = tempdir().expect("alice tempdir");
    let alice_base = alice_dir.path().to_str().expect("path");
    let alice_transport = LinkedTransport::new(routes.clone());
    let alice_id = alice_transport.local_id.clone();

    link_transports(&bob_transport, &alice_transport).await;

    let bob_config = NodeConfig::with_defaults(bob_base);
    let (bob_runner, bob_handle) = NodeRunner::new_with_transport(
        bob_config,
        Box::new(bob_transport),
        IdentityBootstrap::ephemeral(),
    )
    .expect("bob runner");
    let bob_task = tokio::spawn(async move { bob_runner.run().await });

    let alice_config = NodeConfig::with_defaults(alice_base);
    let (alice_runner, alice_handle) = NodeRunner::new_with_transport(
        alice_config,
        Box::new(alice_transport),
        IdentityBootstrap::ephemeral(),
    )
    .expect("alice runner");
    let alice_task = tokio::spawn(async move { alice_runner.run().await });

    let invite = bob_handle
        .encode_signed_invite_v3(&bob_id, None)
        .await
        .expect("bob invite");
    let info = match parse_invite(&invite).expect("parse invite") {
        InviteTarget::Peer(info) => info,
        _ => panic!("expected peer invite"),
    };
    assert!(
        info.ratchet_bundle.is_some(),
        "running node invite must include ratchet prekeys"
    );

    if let Some(enc) = &info.enc_pubkey_hex {
        alice_handle
            .seed_peer_enc_key(&bob_id, enc)
            .await
            .expect("seed enc");
    }
    if let Some(bundle) = &info.ratchet_bundle {
        alice_handle
            .seed_peer_ratchet_bundle(&bob_id, bundle.clone())
            .await
            .expect("seed bundle");
    }

    for addr in info.dial_multiaddrs() {
        alice_handle
            .send(NodeCommand::AddBootstrapPeer { multiaddr: addr })
            .await
            .expect("dial bob");
    }

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let app_payload = b"ratchet-first-message".to_vec();
    let wrapped = e2e_direct_wrap::wrap_inner(&alice_id, &app_payload);
    let (encrypted_payload, e2e_version) = alice_handle
        .encrypt_e2e_payload(&bob_id, wrapped)
        .await
        .expect("encrypt");
    assert_eq!(e2e_version, E2E_VERSION_RATCHET);
    assert_eq!(
        ratchet::wire_e2e_version(&encrypted_payload, e2e_version),
        1
    );

    let wire = WireMessage::EncryptedDirect {
        to_peer: bob_id.clone(),
        sender_enc_pubkey: alice_handle.local_enc_pubkey_hex(),
        encrypted_payload,
        mesh_signature: None,
        hop_count: 0,
        max_hops: 8,
        e2e_version,
        priority: Priority::Normal,
    };

    alice_handle
        .send(NodeCommand::SendWire {
            to_peer: bob_id.clone(),
            message: wire,
        })
        .await
        .expect("send wire");

    let mut bob_in = bob_handle.subscribe_incoming();
    let dm = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(dm) = bob_in.recv().await {
                if dm.envelope.to_peer.as_deref() == Some(bob_id.as_str()) {
                    return dm;
                }
            }
        }
    })
    .await
    .expect("timeout waiting for bob delivery");

    assert_eq!(dm.envelope.e2e_version, 1);
    assert_eq!(dm.envelope.from_peer, alice_id);
    assert_eq!(dm.envelope.payload, app_payload);

    bob_task.abort();
    alice_task.abort();
}
