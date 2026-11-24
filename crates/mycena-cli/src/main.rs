// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
#![deny(clippy::unwrap_used, clippy::expect_used)]
mod node_boot;
mod release;

use anyhow::Context;
use clap::{Parser, Subcommand};
use mycena_app::api::start_api_server;
use mycena_app::miniapp::store::{AppSource, AppStoreListing};
use mycena_app::miniapp::MiniAppBundle;
use mycena_app::node::AppNode;
use mycena_app::notify::NoopNotifier;
use mycena_app::storage::AppStorage;
use mycena_l1_facade::{
    load_or_create_keypair, ConnectivityMonitor, HostNodeOptions, NodeCommand, NodeConfig,
    NodeRunner,
};
use node_boot::{boot_node, panic_wipe_db, RunArgs};
use release::ReleaseCommands;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "mycena")]
#[command(about = "Delay-tolerant local-first mesh node")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[command(flatten)]
    run: RunArgsCli,
}

#[derive(Parser, Debug, Clone)]
struct RunArgsCli {
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
    listen: String,
    #[arg(long, default_value = ".mycena-node")]
    db: String,
    #[arg(long, default_value = "active")]
    energy_state: String,
    #[arg(long, default_value = "anon")]
    name: String,
    #[arg(long, default_value_t = 7760)]
    api_port: u16,
    #[arg(long, help = "Run HTTP API only; no interactive stdin loop")]
    headless: bool,
    #[arg(long = "bootstrap", value_name = "MULTIADDR")]
    bootstrap: Vec<String>,
}

impl From<RunArgsCli> for RunArgs {
    fn from(v: RunArgsCli) -> Self {
        Self {
            listen: v.listen,
            db: v.db,
            energy_state: v.energy_state,
            name: v.name,
            api_port: v.api_port,
            bootstrap: v.bootstrap,
            headless: v.headless,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Mini-app store utilities
    App {
        #[command(subcommand)]
        cmd: AppCommands,
    },
    /// Reproducible core release integrity (hash, sign, verify)
    Release {
        #[command(subcommand)]
        cmd: ReleaseCommands,
    },
    /// Encrypted group chat
    Group {
        #[command(subcommand)]
        cmd: GroupCommands,
    },
    /// MeshCoin wallet (local ledger)
    Wallet {
        #[command(subcommand)]
        cmd: WalletCommands,
    },
    /// Secure wipe local node data (panic)
    Panic {
        #[arg(long, default_value = ".mycena-node")]
        db: String,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AppCommands {
    Publish {
        bundle: PathBuf,
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
    PublishLive {
        bundle: PathBuf,
        #[arg(long, default_value = ".mycena-node")]
        db: String,
        #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
        listen: String,
        #[arg(long = "bootstrap", value_name = "MULTIADDR")]
        bootstrap: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum GroupCommands {
    Create {
        name: String,
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
    List {
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
    Join {
        invite: PathBuf,
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
    Send {
        group_id: String,
        message: String,
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
}

#[derive(Subcommand, Debug)]
enum WalletCommands {
    Address {
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
    Balance {
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
    Send {
        to: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value = ".mycena-node")]
        db: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::App { cmd } => match cmd {
                AppCommands::Publish { bundle, db } => return cmd_app_publish(db, bundle).await,
                AppCommands::PublishLive {
                    bundle,
                    db,
                    listen,
                    bootstrap,
                } => return cmd_app_publish_live(db, bundle, listen, bootstrap).await,
            },
            Commands::Release { cmd } => return release::run(cmd.clone()),
            Commands::Group { cmd } => return cmd_group(cmd).await,
            Commands::Wallet { cmd } => return cmd_wallet(cmd).await,
            Commands::Panic { db, yes } => return cmd_panic(db, *yes).await,
        }
    }

    run_interactive_server(cli.run.into()).await
}

async fn cmd_group(cmd: &GroupCommands) -> anyhow::Result<()> {
    match cmd {
        GroupCommands::Create { name, db } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            let group = boot.app_node.create_group(name.clone()).await?;
            println!("Created group {} ({})", group.name, group.id);
            println!("Invite JSON:\n{}", group.export_invite());
        }
        GroupCommands::List { db } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            for g in boot.app_node.list_groups()? {
                println!("{}  {}  members={}", g.id, g.name, g.members.len());
            }
        }
        GroupCommands::Join { invite, db } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            let json = std::fs::read_to_string(invite)?;
            let group = boot.app_node.join_group_from_invite(&json).await?;
            println!("Joined {} ({})", group.name, group.id);
        }
        GroupCommands::Send {
            group_id,
            message,
            db,
        } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            boot.app_node
                .send_group_message_by_id(group_id, message.clone())
                .await?;
            println!("Sent group message to {group_id}");
        }
    }
    Ok(())
}

async fn cmd_wallet(cmd: &WalletCommands) -> anyhow::Result<()> {
    match cmd {
        WalletCommands::Address { db } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            let coin = boot
                .coin_node
                .as_ref()
                .context("coin node not initialized")?;
            println!("{}", coin.local_address());
        }
        WalletCommands::Balance { db } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            let coin = boot
                .coin_node
                .as_ref()
                .context("coin node not initialized")?;
            let (confirmed, pending) = coin.balance()?;
            println!("confirmed: {confirmed} μ");
            println!("pending:   {pending} μ");
        }
        WalletCommands::Send { to, amount, db } => {
            let boot = boot_node(&offline_run_args(db)).await?;
            let coin = boot
                .coin_node
                .as_ref()
                .context("coin node not initialized")?;
            let identity = format!("{db}/identity");
            coin.submit_transfer_from_identity_path(&identity, to.clone(), *amount, 0, None)
                .await?;
            println!("Sent {amount} μ to {to}");
        }
    }
    Ok(())
}

async fn cmd_panic(db: &str, yes: bool) -> anyhow::Result<()> {
    if !yes {
        eprintln!("This permanently deletes all data in {db}. Re-run with --yes to confirm.");
        return Ok(());
    }
    panic_wipe_db(db)?;
    println!("Wiped local data at {db}");
    Ok(())
}

fn offline_run_args(db: &str) -> RunArgs {
    RunArgs {
        listen: "/ip4/127.0.0.1/tcp/0".into(),
        db: db.to_string(),
        energy_state: "active".into(),
        name: "cli".into(),
        api_port: 0,
        bootstrap: Vec::new(),
        headless: true,
    }
}

fn build_signed_listing(db: &str, bundle_path: &PathBuf) -> anyhow::Result<AppStoreListing> {
    let identity_path = format!("{db}/identity");
    let keypair = load_or_create_keypair(&identity_path)?;
    let peer_id = keypair.public().to_peer_id().to_string();
    let bundle_bytes = std::fs::read(bundle_path)?;
    let bundle = MiniAppBundle::load_from_bytes(&bundle_bytes)?;
    let mut manifest = bundle.manifest.clone();
    if manifest.developer_peer_id.is_none() {
        manifest.developer_peer_id = Some(peer_id.clone());
    }
    AppStoreListing::new_signed(
        manifest,
        &bundle_bytes,
        vec![AppSource::Peer(peer_id)],
        &keypair,
    )
}

async fn cmd_app_publish(db: &str, bundle: &PathBuf) -> anyhow::Result<()> {
    let listing = build_signed_listing(db, bundle)?;
    println!(
        "✓ Signed: {} v{}",
        listing.manifest.id, listing.manifest.version
    );
    println!("  Bundle hash: {}", listing.bundle_hash);
    Ok(())
}

async fn cmd_app_publish_live(
    db: &str,
    bundle: &PathBuf,
    listen: &str,
    bootstrap: &[String],
) -> anyhow::Result<()> {
    let listing = build_signed_listing(db, bundle)?;
    let listen_addr = listen.parse().context("invalid --listen multiaddr")?;
    let (connectivity, internet_enabled) = ConnectivityMonitor::new(true);
    ConnectivityMonitor::spawn_monitor(connectivity.mode_tx.clone(), internet_enabled.clone());
    let config = NodeConfig::for_host(HostNodeOptions {
        listen_addr,
        db_path: db.to_string(),
        keypair_path: None,
        bootstrap_peers: bootstrap.to_vec(),
        connectivity_rx: Some(connectivity.mode_rx),
        connectivity_mode_tx: Some(connectivity.mode_tx),
        display_name: Some("publisher".into()),
        storage_key: None,
        rendezvous_enabled: true,
        internet_enabled,
        bulletin_subscriptions: Vec::new(),
        crowd_mode: false,
    });
    let (runner, handle) = NodeRunner::new(config)?;
    let publisher_peer = runner.local_peer_id().to_string();
    tokio::spawn(async move {
        if let Err(err) = runner.run().await {
            warn!("node loop terminated: {err:?}");
        }
    });
    let app_storage = Arc::new(AppStorage::open(&format!("{db}/app"))?);
    let app_store = Arc::new(mycena_app::miniapp::AppStore::open(&format!(
        "{db}/miniapp"
    ))?);
    let enc_keypair = mycena_l0_facade::secrets::load_or_create_enc_keypair(db, None)?;
    let (app_node, _inbox) = AppNode::new(
        handle.clone(),
        publisher_peer,
        "publisher".into(),
        app_storage,
        Arc::new(NoopNotifier),
        None,
        Some(app_store),
        enc_keypair,
    );
    let app_node = Arc::new(app_node);
    app_node.clone().start_incoming_task();
    app_node.publish_app_listing(&listing).await?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    println!("Published listing to mesh.");
    Ok(())
}

async fn run_interactive_server(args: RunArgs) -> anyhow::Result<()> {
    let boot = boot_node(&args).await?;
    info!("local peer id: {}", boot.local_peer_id);

    if args.api_port > 0 {
        let token = start_api_server(
            boot.app_node.clone(),
            boot.app_storage.clone(),
            args.api_port,
            PathBuf::from(&args.db),
        )
        .await?;
        info!(
            "api server listening on 127.0.0.1:{} (Authorization: Bearer <token>)",
            args.api_port
        );
        eprintln!("Mycena API token: {token}");
    }

    if args.headless {
        info!("headless mode — waiting indefinitely (send SIGTERM to exit)");
        std::future::pending::<()>().await;
        return Ok(());
    }

    info!("commands: /peers | /chat <peer_id> <msg> | /broadcast <scope> <text> | /group:list | /group:send <id> <msg> | /wallet:balance | /panic");
    let stdin = io::BufReader::new(io::stdin());
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/peers" {
            boot.handle.send(NodeCommand::ListPeers).await?;
            continue;
        }
        if trimmed == "/panic" {
            let db = args.db.clone();
            boot.runner_task.abort();
            panic_wipe_db(&db)?;
            println!("Wiped {db}");
            return Ok(());
        }
        if trimmed == "/wallet:balance" {
            if let Some(coin) = boot.coin_node.as_ref() {
                let (c, p) = coin.balance()?;
                println!("confirmed={c} pending={p} μ");
            }
            continue;
        }
        if trimmed == "/group:list" {
            for g in boot.app_node.list_groups()? {
                println!("{} {}", g.id, g.name);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/group:send ") {
            let mut parts = rest.splitn(2, ' ');
            let gid = parts.next().unwrap_or_default();
            let body = parts.next().unwrap_or_default();
            boot.app_node
                .send_group_message_by_id(gid, body.to_string())
                .await?;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/chat ") {
            let mut parts = rest.splitn(2, ' ');
            let peer = parts.next().unwrap_or_default().to_string();
            let body = parts.next().unwrap_or_default().to_string();
            if peer.is_empty() || body.is_empty() {
                warn!("usage: /chat <peer_id> <text>");
                continue;
            }
            boot.app_node.send_chat(Some(peer), body).await?;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/broadcast ") {
            let mut parts = rest.splitn(2, ' ');
            let scope = parts.next().unwrap_or_default().to_string();
            let body = parts.next().unwrap_or_default().to_string();
            if scope.is_empty() || body.is_empty() {
                warn!("usage: /broadcast <scope> <text>");
                continue;
            }
            boot.app_node.broadcast_chat(scope, body).await?;
            continue;
        }
        warn!("unknown command");
    }
    Ok(())
}
