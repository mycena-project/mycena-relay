// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Shared node + app bootstrap for interactive and headless CLI runs.

use anyhow::Context;
use async_trait::async_trait;
use mycena_app::envelope::AppMessage;
use mycena_app::node::AppNode;
use mycena_app::notify::NoopNotifier;
use mycena_app::storage::AppStorage;
use mycena_coin::{address_from_keypair, CoinNode, CoinTransport, LocalLedger};
use mycena_l0_facade::consistency::OP_GOSSIP_BULLETIN_PUBLISH;
use mycena_l0_facade::energy::NodeState;
use mycena_l1_facade::{
    load_or_create_keypair, ConnectivityMonitor, HostNodeOptions, NodeCommand, NodeConfig,
    NodeHandle, NodeRunner,
};
use std::path::PathBuf;
use std::sync::Arc;

pub struct RunArgs {
    pub listen: String,
    pub db: String,
    pub energy_state: String,
    pub name: String,
    pub api_port: u16,
    pub bootstrap: Vec<String>,
    pub headless: bool,
}

pub struct BootedNode {
    pub handle: NodeHandle,
    pub app_node: Arc<AppNode>,
    pub app_storage: Arc<AppStorage>,
    pub coin_node: Option<Arc<CoinNode>>,
    pub local_peer_id: String,
    pub runner_task: tokio::task::JoinHandle<()>,
}

struct CliCoinTransport {
    handle: NodeHandle,
}

#[async_trait]
impl CoinTransport for CliCoinTransport {
    async fn broadcast_coin_inner(&self, coin_inner: Vec<u8>) -> anyhow::Result<()> {
        let payload = AppMessage::encode_coin_payload(&coin_inner)?;
        self.handle
            .mesh_broadcast(
                "mycena/coin".to_string(),
                "coin:broadcast".to_string(),
                payload,
                OP_GOSSIP_BULLETIN_PUBLISH,
            )
            .await
    }

    async fn send_direct_coin_inner(
        &self,
        to_peer: String,
        coin_inner: Vec<u8>,
    ) -> anyhow::Result<()> {
        let payload = AppMessage::encode_coin_payload(&coin_inner)?;
        self.handle
            .mesh_broadcast(
                "mycena/coin".to_string(),
                format!("coin:direct:{to_peer}"),
                payload,
                OP_GOSSIP_BULLETIN_PUBLISH,
            )
            .await
    }
}

pub fn parse_energy_state(input: &str) -> anyhow::Result<NodeState> {
    match input.to_ascii_lowercase().as_str() {
        "active" => Ok(NodeState::Active),
        "intermittent" => Ok(NodeState::Intermittent),
        "passive" => Ok(NodeState::Passive),
        _ => Err(anyhow::anyhow!(
            "invalid --energy-state, expected active|intermittent|passive"
        )),
    }
}

pub async fn boot_node(args: &RunArgs) -> anyhow::Result<BootedNode> {
    let listen_addr = args.listen.parse().context("invalid --listen multiaddr")?;
    let (connectivity, internet_enabled) = ConnectivityMonitor::new(true);
    ConnectivityMonitor::spawn_monitor(connectivity.mode_tx.clone(), internet_enabled.clone());
    let config = NodeConfig::for_host(HostNodeOptions {
        listen_addr,
        db_path: args.db.clone(),
        keypair_path: None,
        bootstrap_peers: args.bootstrap.clone(),
        connectivity_rx: Some(connectivity.mode_rx),
        connectivity_mode_tx: Some(connectivity.mode_tx),
        display_name: Some(args.name.clone()),
        storage_key: None,
        rendezvous_enabled: true,
        internet_enabled,
        bulletin_subscriptions: Vec::new(),
        crowd_mode: false,
    });
    let (runner, handle) = NodeRunner::new(config)?;
    let local_peer_id = runner.local_peer_id().to_string();
    let runner_task = tokio::spawn(async move {
        if let Err(err) = runner.run().await {
            tracing::warn!("node loop terminated: {err:?}");
        }
    });

    handle
        .send(NodeCommand::SetEnergyState(parse_energy_state(
            &args.energy_state,
        )?))
        .await?;

    let app_storage = Arc::new(AppStorage::open(&format!("{}/app", args.db))?);
    let app_store = Arc::new(mycena_app::miniapp::AppStore::open(&format!(
        "{}/miniapp",
        args.db
    ))?);
    let identity_path = format!("{}/identity", args.db);
    let coin_ledger = Arc::new(LocalLedger::open(&format!("{}/coin", args.db))?);
    let coin_keypair = load_or_create_keypair(&identity_path)?;
    let coin_addr = address_from_keypair(&coin_keypair);
    let coin_transport = Arc::new(CliCoinTransport {
        handle: handle.clone(),
    });
    let coin_node = Arc::new(CoinNode::new(
        coin_ledger,
        coin_transport,
        coin_addr,
        local_peer_id.clone(),
        identity_path,
    ));
    let enc_keypair = mycena_l0_facade::secrets::load_or_create_enc_keypair(&args.db, None)?;
    let (app_node, _inbox) = AppNode::new(
        handle.clone(),
        local_peer_id.clone(),
        args.name.clone(),
        app_storage.clone(),
        Arc::new(NoopNotifier),
        Some(coin_node.clone()),
        Some(app_store),
        enc_keypair,
    );
    let app_node = Arc::new(app_node);
    app_node.clone().start_incoming_task();

    Ok(BootedNode {
        handle,
        app_node,
        app_storage,
        coin_node: Some(coin_node),
        local_peer_id,
        runner_task,
    })
}

pub fn panic_wipe_db(db: &str) -> anyhow::Result<()> {
    mycena_l0_facade::wipe_node_directory(PathBuf::from(db).as_path())
}
