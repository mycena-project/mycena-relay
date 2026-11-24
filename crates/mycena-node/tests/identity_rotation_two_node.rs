// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! SD-190: transport remap and unified bearer drain integration.

use async_trait::async_trait;
use libp2p::identity;
use mycena_core::crypto::EncryptionKeypair;
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
    #[allow(dead_code)]
    inbox_tx: mpsc::UnboundedSender<TransportEvent>,
    #[allow(dead_code)]
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

    async fn remember_and_dial(&mut self, _multiaddr: String) -> anyhow::Result<()> {
        Ok(())
    }

    fn redial_stored_targets(&mut self) {}

    async fn send_direct(&mut self, _to_peer: String, _message: WireMessage) -> anyhow::Result<()> {
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

    fn is_peer_connected(&self, _peer_id: &str) -> bool {
        false
    }

    async fn next_event(&mut self) -> anyhow::Result<TransportEvent> {
        self.inbox
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("linked transport inbox closed"))
    }
}

#[tokio::test]
async fn remap_transport_peer_moves_enc_key() {
    let routes = Arc::new(Mutex::new(HashMap::new()));
    let dir = tempdir().expect("tempdir");
    let transport = LinkedTransport::new(routes);

    let (runner, handle) = NodeRunner::new_with_transport(
        NodeConfig::with_defaults(dir.path().to_str().unwrap()),
        Box::new(transport),
        IdentityBootstrap::ephemeral(),
    )
    .expect("runner");
    let task = tokio::spawn(async move { runner.run().await });

    let old_peer = "12D3KooWOldPeerIdForRemapTest".to_string();
    let new_peer = "12D3KooWNewPeerIdForRemapTest".to_string();
    let enc_hex = EncryptionKeypair::generate().public_hex();

    handle
        .seed_peer_enc_key(&old_peer, &enc_hex)
        .await
        .expect("seed");

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(handle.has_enc_key_for(&old_peer).await);
    assert!(!handle.has_enc_key_for(&new_peer).await);

    handle
        .send(NodeCommand::RemapTransportPeer {
            old_peer_id: old_peer.clone(),
            new_peer_id: new_peer.clone(),
        })
        .await
        .expect("remap");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(!handle.has_enc_key_for(&old_peer).await);
    assert!(handle.has_enc_key_for(&new_peer).await);

    task.abort();
}
