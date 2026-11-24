// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::scheduler::LinkProfile;
use async_trait::async_trait;
use mycena_core::lock_or_recover;
use mycena_core::transport::{MeshTransport, ScopeId, TransportEvent, WireMessage};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum SimAction {
    SendDirect {
        from_peer: String,
        to_peer: String,
        message: WireMessage,
    },
    PublishScoped {
        from_peer: String,
        scope: ScopeId,
        payload: Vec<u8>,
    },
}

pub struct SimTransport {
    local_peer: String,
    keypair: Option<libp2p::identity::Keypair>,
    links: Arc<Mutex<HashMap<(String, String), LinkProfile>>>,
    event_rx: mpsc::UnboundedReceiver<TransportEvent>,
    action_tx: mpsc::UnboundedSender<SimAction>,
    subscribed_scopes: HashSet<String>,
}

impl SimTransport {
    pub fn new(
        local_peer: String,
        keypair: Option<libp2p::identity::Keypair>,
        links: Arc<Mutex<HashMap<(String, String), LinkProfile>>>,
        event_rx: mpsc::UnboundedReceiver<TransportEvent>,
        action_tx: mpsc::UnboundedSender<SimAction>,
    ) -> Self {
        Self {
            local_peer,
            keypair,
            links,
            event_rx,
            action_tx,
            subscribed_scopes: HashSet::new(),
        }
    }

    pub fn subscribed_scopes(&self) -> &HashSet<String> {
        &self.subscribed_scopes
    }
}

#[async_trait]
impl MeshTransport for SimTransport {
    fn local_peer_id(&self) -> String {
        self.local_peer.clone()
    }

    fn known_peers(&self) -> Vec<String> {
        // POISON-RECOVER: read-only link lookup for simulated send routing.
        let links = lock_or_recover!(self.links, "sim links");
        links
            .keys()
            .filter_map(|(a, b)| {
                if a == &self.local_peer {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn local_keypair(&self) -> Option<libp2p::identity::Keypair> {
        self.keypair.clone()
    }

    async fn dial_peer(&mut self, _multiaddr: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remember_and_dial(&mut self, _multiaddr: String) -> anyhow::Result<()> {
        Ok(())
    }

    fn redial_stored_targets(&mut self) {}

    async fn send_direct(&mut self, to_peer: String, message: WireMessage) -> anyhow::Result<()> {
        self.action_tx.send(SimAction::SendDirect {
            from_peer: self.local_peer.clone(),
            to_peer,
            message,
        })?;
        Ok(())
    }

    async fn publish_scoped(&mut self, scope: ScopeId, payload: Vec<u8>) -> anyhow::Result<()> {
        self.action_tx.send(SimAction::PublishScoped {
            from_peer: self.local_peer.clone(),
            scope,
            payload,
        })?;
        Ok(())
    }

    async fn subscribe_scope(&mut self, scope: ScopeId) -> anyhow::Result<()> {
        self.subscribed_scopes.insert(scope);
        Ok(())
    }

    async fn unsubscribe_scope(&mut self, scope: ScopeId) -> anyhow::Result<()> {
        self.subscribed_scopes.remove(&scope);
        Ok(())
    }

    async fn next_event(&mut self) -> anyhow::Result<TransportEvent> {
        self.event_rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("sim transport event channel closed"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::scheduler::LinkProfile;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    #[test]
    fn sim_transport_links_survive_mutex_poison() {
        let links = Arc::new(Mutex::new(HashMap::new()));
        let links2 = Arc::clone(&links);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock_or_recover!(links2, "sim links")
                .insert(("local".into(), "remote".into()), LinkProfile::default());
            panic!("deliberate sim transport links poison");
        }));
        let (action_tx, _action_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        let transport = SimTransport::new(
            "local".into(),
            None,
            Arc::clone(&links),
            event_rx,
            action_tx,
        );
        lock_or_recover!(links, "sim links")
            .insert(("local".into(), "remote".into()), LinkProfile::default());
        assert_eq!(transport.known_peers(), vec!["remote".to_string()]);
    }
}
