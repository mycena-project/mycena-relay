// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Node continues after mutex poisoning on local trust state.

use async_trait::async_trait;
use libp2p::identity;
use mycena_core::transport::{MeshTransport, TransportEvent};
use mycena_node::{IdentityBootstrap, NodeConfig, NodeRunner};
use std::future;
use tempfile::tempdir;

struct StubTransport {
    local_id: String,
    keypair: identity::Keypair,
}

impl StubTransport {
    fn new() -> Self {
        let keypair = identity::Keypair::generate_ed25519();
        let local_id = keypair.public().to_peer_id().to_string();
        Self { local_id, keypair }
    }
}

#[async_trait]
impl MeshTransport for StubTransport {
    fn local_peer_id(&self) -> String {
        self.local_id.clone()
    }

    fn known_peers(&self) -> Vec<String> {
        Vec::new()
    }

    fn local_keypair(&self) -> Option<identity::Keypair> {
        Some(self.keypair.clone())
    }

    async fn dial_peer(&mut self, _multiaddr: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remember_and_dial(&mut self, _multiaddr: String) -> anyhow::Result<()> {
        Ok(())
    }

    fn redial_stored_targets(&mut self) {}

    async fn send_direct(
        &mut self,
        _to_peer: String,
        _message: mycena_core::transport::WireMessage,
    ) -> anyhow::Result<()> {
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
        future::pending().await
    }
}

#[tokio::test]
async fn node_metrics_after_peer_trust_cache_poison() {
    let dir = tempdir().expect("tempdir");
    let config = NodeConfig::with_defaults(dir.path().to_str().expect("path"));
    let (runner, handle) = NodeRunner::new_with_transport(
        config,
        Box::new(StubTransport::new()),
        IdentityBootstrap::ephemeral(),
    )
    .expect("node runner");

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runner
            .peer_trust_for_test()
            .with_peer("__poison__", 0, |_| {
                panic!("deliberate poison during trust update")
            })
            .ok();
    }));

    let metrics = handle.metrics().await;
    assert_eq!(metrics.connected_peers, 0);

    runner
        .peer_trust_for_test()
        .with_peer("12D3KooWNodeAfterPoisonTest0001", 3000, |t| {
            t.relay_success_rate = 0.75;
        })
        .expect("trust update after poison");

    let again = handle.metrics().await;
    assert_eq!(again.connected_peers, 0);
}
