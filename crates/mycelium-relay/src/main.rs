// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycelium Project
use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use libp2p::kad::store::{MemoryStore, MemoryStoreConfig};
use libp2p::{
    futures::StreamExt,
    identify, kad, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use mycelium_core::bootstrap::IDENTIFY_PROTOCOL;
use mycelium_core::data::now_ms;
use mycelium_core::identity::load_or_create_keypair_from_env;
use mycelium_core::rendezvous::{verify_registration, RendezvousAuthBody};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const MAX_RENDEZVOUS_OPT_IN: usize = 1024;
const HTTP_MUTATE_LIMIT: u32 = 30;
const HTTP_MUTATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "mycelium-relay")]
struct Args {
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/4001")]
    listen: String,
    #[arg(long, default_value = "/ip4/0.0.0.0/udp/4001/quic-v1")]
    listen_quic: String,
    #[arg(long)]
    keypair_path: Option<String>,
    #[arg(long, default_value_t = 8080)]
    status_port: u16,
}

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kad: kad::Behaviour<MemoryStore>,
}

#[derive(Clone)]
struct StatusState {
    peer_id: String,
    connections: Arc<AtomicU64>,
    reservations: Arc<AtomicU64>,
    connected_peers: Arc<RwLock<HashMap<PeerId, u32>>>,
    reserved_peers: Arc<RwLock<std::collections::HashSet<PeerId>>>,
    rendezvous_opt_in: Arc<RwLock<std::collections::HashSet<PeerId>>>,
    http_hits: Arc<RwLock<HashMap<IpAddr, (u32, Instant)>>>,
    started: Instant,
    version: &'static str,
}

fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

fn allow_http_mutate(s: &StatusState, ip: IpAddr) -> bool {
    let mut hits = write_lock(&s.http_hits);
    let now = Instant::now();
    hits.retain(|_, (_, t)| now.duration_since(*t) < HTTP_MUTATE_WINDOW);
    let entry = hits.entry(ip).or_insert((0, now));
    if now.duration_since(entry.1) >= HTTP_MUTATE_WINDOW {
        *entry = (0, now);
    }
    if entry.0 >= HTTP_MUTATE_LIMIT {
        return false;
    }
    entry.0 += 1;
    true
}

fn reachable_peer_strings(s: &StatusState) -> Vec<String> {
    let reserved = read_lock(&s.reserved_peers);
    let opt_in = read_lock(&s.rendezvous_opt_in);
    reserved
        .iter()
        .filter(|p| opt_in.contains(*p))
        .map(ToString::to_string)
        .collect()
}

async fn rendezvous(State(s): State<StatusState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "peers": reachable_peer_strings(&s) }))
}

async fn rendezvous_register(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(s): State<StatusState>,
    Json(body): Json<RendezvousAuthBody>,
) -> StatusCode {
    if !allow_http_mutate(&s, addr.ip()) {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    let Ok(peer_id) = verify_registration(&body, true, now_ms()) else {
        return StatusCode::UNAUTHORIZED;
    };
    if !read_lock(&s.reserved_peers).contains(&peer_id) {
        return StatusCode::FORBIDDEN;
    }
    let mut opt_in = write_lock(&s.rendezvous_opt_in);
    if opt_in.len() >= MAX_RENDEZVOUS_OPT_IN && !opt_in.contains(&peer_id) {
        return StatusCode::INSUFFICIENT_STORAGE;
    }
    opt_in.insert(peer_id);
    StatusCode::NO_CONTENT
}

async fn rendezvous_unregister(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(s): State<StatusState>,
    Json(body): Json<RendezvousAuthBody>,
) -> StatusCode {
    if !allow_http_mutate(&s, addr.ip()) {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    let Ok(peer_id) = verify_registration(&body, false, now_ms()) else {
        return StatusCode::UNAUTHORIZED;
    };
    write_lock(&s.rendezvous_opt_in).remove(&peer_id);
    StatusCode::NO_CONTENT
}

async fn status(State(s): State<StatusState>) -> Json<serde_json::Value> {
    let unique_peers = read_lock(&s.connected_peers).len() as u64;
    let reachable_peers = reachable_peer_strings(&s).len() as u64;
    Json(serde_json::json!({
        "status": "ok",
        "peer_id": s.peer_id,
        "connections": s.connections.load(Ordering::Relaxed),
        "unique_peers": unique_peers,
        "reachable_peers": reachable_peers,
        "reservations": s.reservations.load(Ordering::Relaxed),
        "uptime_secs": s.started.elapsed().as_secs(),
        "version": s.version,
    }))
}

async fn health() -> &'static str {
    "ok"
}

fn atomic_saturating_sub(a: &AtomicU64) {
    let _ = a.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_sub(1))
    });
}

fn relay_config() -> relay::Config {
    relay::Config {
        max_reservations: 256,
        max_circuits: 32,
        max_circuit_bytes: 256 * 1024,
        max_circuit_duration: Duration::from_secs(180),
        ..relay::Config::default()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("mycelium_relay=info".parse()?)
                .add_directive("libp2p_relay=info".parse()?),
        )
        .init();

    let args = Args::parse();
    let keypair_path = args
        .keypair_path
        .unwrap_or_else(|| ".mycelium-relay/identity".to_string());
    let key = load_or_create_keypair_from_env(&keypair_path)?;
    let local_peer_id = key.public().to_peer_id();

    info!("Relay Peer ID: {local_peer_id}");
    if local_peer_id.to_string() != mycelium_core::bootstrap::RELAY_PEER_ID {
        warn!(
            "local peer id {local_peer_id} != compiled RELAY_PEER_ID {}; clients using bootstrap.rs will not dial this process",
            mycelium_core::bootstrap::RELAY_PEER_ID
        );
    }

    let connections = Arc::new(AtomicU64::new(0));
    let reservations = Arc::new(AtomicU64::new(0));
    let connected_peers = Arc::new(RwLock::new(HashMap::new()));
    let reserved_peers = Arc::new(RwLock::new(std::collections::HashSet::new()));
    let rendezvous_opt_in = Arc::new(RwLock::new(std::collections::HashSet::new()));
    let http_hits = Arc::new(RwLock::new(HashMap::new()));
    let state = StatusState {
        peer_id: local_peer_id.to_string(),
        connections: connections.clone(),
        reservations: reservations.clone(),
        connected_peers: connected_peers.clone(),
        reserved_peers: reserved_peers.clone(),
        rendezvous_opt_in: rendezvous_opt_in.clone(),
        http_hits,
        started: Instant::now(),
        version: env!("CARGO_PKG_VERSION"),
    };
    let app = Router::new()
        .route("/", get(status))
        .route("/health", get(health))
        .route("/rendezvous", get(rendezvous))
        .route("/rendezvous/register", post(rendezvous_register))
        .route("/rendezvous/unregister", post(rendezvous_unregister))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", args.status_port)).await?;
    info!("Status server on http://0.0.0.0:{}", args.status_port);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            error!("status server exited: {e}");
            std::process::exit(1);
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
                relay: relay::Behaviour::new(key.public().to_peer_id(), relay_config()),
                identify,
                ping: ping::Behaviour::new(ping::Config::new()),
                kad: {
                    let local_peer_id = key.public().to_peer_id();
                    let mut config =
                        kad::Config::new(libp2p::StreamProtocol::new("/mycelium/kad/1.0.0"));
                    config.set_query_timeout(Duration::from_secs(60));
                    let store = MemoryStore::with_config(
                        local_peer_id,
                        MemoryStoreConfig {
                            max_records: 256,
                            max_value_bytes: 4096,
                            max_provided_keys: 256,
                            max_providers_per_key: 20,
                        },
                    );
                    let mut behaviour = kad::Behaviour::with_config(local_peer_id, store, config);
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
            SwarmEvent::ConnectionEstablished {
                peer_id,
                num_established,
                ..
            } => {
                connections.fetch_add(1, Ordering::Relaxed);
                write_lock(&connected_peers).insert(peer_id, num_established.get());
                info!("+ {peer_id} ({})", connections.load(Ordering::Relaxed));
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                atomic_saturating_sub(&connections);
                if num_established == 0 {
                    write_lock(&connected_peers).remove(&peer_id);
                } else {
                    write_lock(&connected_peers).insert(peer_id, num_established);
                }
                info!("- {peer_id} ({})", connections.load(Ordering::Relaxed));
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(ev)) => match ev {
                relay::Event::ReservationReqAccepted {
                    src_peer_id,
                    renewed,
                    ..
                } => {
                    if !renewed {
                        reservations.fetch_add(1, Ordering::Relaxed);
                    }
                    write_lock(&reserved_peers).insert(src_peer_id);
                    if renewed {
                        info!("Reservation renewed: {src_peer_id}");
                    } else {
                        info!("Reservation accepted: {src_peer_id}");
                    }
                }
                relay::Event::ReservationTimedOut { src_peer_id } => {
                    atomic_saturating_sub(&reservations);
                    write_lock(&reserved_peers).remove(&src_peer_id);
                    write_lock(&rendezvous_opt_in).remove(&src_peer_id);
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
