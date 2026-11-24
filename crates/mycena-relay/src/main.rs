// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
mod status_http;

use clap::Parser;
use libp2p::kad::store::MemoryStore;
use libp2p::{
    futures::StreamExt,
    identify, kad, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, SwarmBuilder,
};
use mycena_l0_facade::lock_recover::write_rwlock_or_recover;
use mycena_l0_facade::protocol::IDENTIFY_PROTOCOL;
use mycena_l1_facade::load_or_create_keypair_from_env;
use status_http::{status_router, StatusState};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::info;

#[derive(Parser)]
#[command(name = "mycena-relay")]
struct Args {
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/4001")]
    listen: String,
    #[arg(long, default_value = "/ip4/0.0.0.0/udp/4001/quic-v1")]
    listen_quic: String,
    #[arg(long)]
    keypair_path: Option<String>,
    #[arg(long, default_value_t = 8080)]
    status_port: u16,
    /// Bind address for the HTTP status port (health is public; /status needs a token).
    #[arg(long, default_value = "0.0.0.0")]
    status_bind: String,
    /// Bearer token for GET /status. Env: MYCENA_RELAY_STATUS_TOKEN.
    #[arg(long)]
    status_token: Option<String>,
}

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kad: kad::Behaviour<MemoryStore>,
}

fn atomic_saturating_sub(a: &AtomicU64) {
    // fetch_update is deprecated on newer toolchains and try_update is still
    // unstable on the Rust 1.88 image used to build the relay.
    #[allow(deprecated)]
    let _ = a.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_sub(1))
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("mycena_relay=info".parse()?)
                .add_directive("libp2p_relay=info".parse()?),
        )
        .init();

    let args = Args::parse();
    let keypair_path = args
        .keypair_path
        .unwrap_or_else(|| ".mycena-relay/identity".to_string());
    let key = load_or_create_keypair_from_env(&keypair_path)?;
    let local_peer_id = key.public().to_peer_id();

    info!("Relay Peer ID: {local_peer_id}");
    info!("Bootstrap multiaddr: /dns4/mycena-relay.fly.dev/tcp/4001/p2p/{local_peer_id}");

    let connections = Arc::new(AtomicU64::new(0));
    let reservations = Arc::new(AtomicU64::new(0));

    let connected_peers = Arc::new(RwLock::new(HashSet::new()));
    let rendezvous_opt_in = Arc::new(RwLock::new(HashSet::new()));
    let status_token = args
        .status_token
        .or_else(|| std::env::var("MYCENA_RELAY_STATUS_TOKEN").ok())
        .filter(|t| t.len() >= 16);
    if status_token.is_none() {
        tracing::warn!(
            "MYCENA_RELAY_STATUS_TOKEN unset — GET /status is disabled; /health stays public"
        );
    }
    let state = StatusState {
        peer_id: local_peer_id.to_string(),
        connections: connections.clone(),
        reservations: reservations.clone(),
        connected_peers: connected_peers.clone(),
        rendezvous_opt_in: rendezvous_opt_in.clone(),
        started: Instant::now(),
        version: env!("CARGO_PKG_VERSION"),
        status_token,
    };
    let app = status_router(state);
    let status_port = args.status_port;
    let status_bind = args.status_bind.clone();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(format!("{status_bind}:{status_port}"))
            .await
            .expect("bind status port");
        info!("Status server on http://{status_bind}:{status_port} (/health public, /status token-gated)");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("status server exited: {e}");
        }
    });

    let mut swarm = SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            let identify = identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.into(),
                key.public(),
            ));
            Ok(RelayBehaviour {
                relay: relay::Behaviour::new(
                    key.public().to_peer_id(),
                    relay::Config {
                        max_reservations: 1024,
                        max_reservations_per_peer: 4,
                        reservation_duration: Duration::from_secs(3600),
                        reservation_rate_limiters: vec![],
                        max_circuits: 1024,
                        max_circuits_per_peer: 16,
                        max_circuit_duration: Duration::from_secs(300),
                        max_circuit_bytes: 10 * 1024 * 1024,
                        circuit_src_rate_limiters: vec![],
                    },
                ),
                identify,
                ping: ping::Behaviour::new(ping::Config::new()),
                kad: {
                    let local_peer_id = key.public().to_peer_id();
                    let mut config = kad::Config::new(libp2p::StreamProtocol::new(
                        mycena_l0_facade::protocol::KAD_PROTOCOL,
                    ));
                    config.set_query_timeout(Duration::from_secs(60));
                    let mut behaviour = kad::Behaviour::with_config(
                        local_peer_id,
                        MemoryStore::new(local_peer_id),
                        config,
                    );
                    behaviour.set_mode(Some(kad::Mode::Server));
                    behaviour
                },
            })
        })?
        .build();

    swarm.listen_on(args.listen.parse::<Multiaddr>()?)?;
    swarm.listen_on(args.listen_quic.parse::<Multiaddr>()?)?;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {address}");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                connections.fetch_add(1, Ordering::Relaxed);
                write_rwlock_or_recover(&connected_peers, "relay connected_peers")
                    .insert(peer_id.to_string());
                info!("+ {peer_id} ({})", connections.load(Ordering::Relaxed));
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                atomic_saturating_sub(&connections);
                write_rwlock_or_recover(&connected_peers, "relay connected_peers")
                    .remove(&peer_id.to_string());
                info!("- {peer_id} ({})", connections.load(Ordering::Relaxed));
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(ev)) => match ev {
                relay::Event::ReservationReqAccepted {
                    src_peer_id,
                    renewed,
                    ..
                } if !renewed => {
                    reservations.fetch_add(1, Ordering::Relaxed);
                    info!("Reservation accepted: {src_peer_id}");
                }
                relay::Event::ReservationTimedOut { src_peer_id } => {
                    atomic_saturating_sub(&reservations);
                    info!("Reservation timed out: {src_peer_id}");
                }
                relay::Event::CircuitClosed {
                    src_peer_id,
                    dst_peer_id,
                    ..
                } => {
                    info!("Circuit closed: {src_peer_id} → {dst_peer_id}");
                }
                _ => {}
            },
            SwarmEvent::Behaviour(RelayBehaviourEvent::Kad(kad::Event::RoutingUpdated {
                peer,
                ..
            })) => {
                info!("kad routing table: {peer}");
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Kad(_)) => {}
            SwarmEvent::Behaviour(_) => {}
            _ => {}
        }
    }
}
