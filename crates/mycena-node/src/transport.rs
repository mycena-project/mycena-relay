// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::bearer_mesh::BearerMeshRouter;
use crate::bearer_webrtc::BearerWebRtcRouter;
use crate::behaviour::{make_direct_response, DirectMessageRequest, MeshBehaviour};
use crate::connectivity::{kad_action_for_mode, KadConnectivityAction};
use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::{
    gossipsub, identify, identity, kad, mdns, noise, request_response,
    swarm::{Swarm, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, SwarmBuilder,
};
use mycena_core::ble_multiaddr::is_ble_multiaddr;
use mycena_core::bootstrap::peer_id_from_parsed_multiaddr;
use mycena_core::data::now_ms;
use mycena_core::mesh_profile::MeshProfile;
use mycena_core::rate_limit::FixedWindowRateLimiter;
use mycena_core::relay_diversity::RelayDiversityPolicy;
use mycena_core::relay_pool::RelayPool;
use mycena_core::relay_registry::{include_catalog_relays, load_relay_manifest};
use mycena_core::transport::{
    ConnectivityMode, MeshTransport, MessageAck, ScopeId, TransportEvent, WireMessage,
};
use mycena_core::transport_score::{
    decide_transport_path, TransportAvailability, TransportBearer, TransportPath,
    TransportScoreSample,
};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::warn;

type MeshEvent = <MeshBehaviour as libp2p::swarm::NetworkBehaviour>::ToSwarm;

/// Maximale Anzahl direkter Peer-Verbindungen.
const MAX_DIRECT_PEERS_DEFAULT: usize = 50;
/// Crowd mode caps LAN/mDNS fan-out (Phase D3).
const CROWD_MAX_DIRECT_PEERS: usize = 20;
const CROWD_MDNS_DIAL_CAP: u32 = 3;
const CROWD_MDNS_DIAL_WINDOW: Duration = Duration::from_secs(60);
/// Kademlia routing-table query cap per window (AR-06).
const KAD_QUERY_CAP: u32 = 8;
const KAD_QUERY_WINDOW: Duration = Duration::from_secs(60);

/// Result of attempting to admit a peer under the direct-peer cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAdmitAction {
    /// Peer admitted; no eviction needed.
    Admitted,
    /// Evict this peer before admitting the new one.
    Evict(PeerId),
    /// Reject the incoming connection.
    Reject,
}

/// Tracks direct peer connections with an LRU cap.
#[derive(Debug)]
pub struct DirectPeerCap {
    max_peers: usize,
    peers: HashSet<PeerId>,
    peer_last_seen: HashMap<PeerId, Instant>,
    pub rejections: u64,
}

impl DirectPeerCap {
    pub fn new(max_peers: usize) -> Self {
        Self {
            max_peers,
            peers: HashSet::new(),
            peer_last_seen: HashMap::new(),
            rejections: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn max_peers(&self) -> usize {
        self.max_peers
    }

    pub fn set_max_peers(&mut self, max_peers: usize) {
        self.max_peers = max_peers;
        while !self.is_empty() && self.len() > self.max_peers {
            if let Some(oldest) = self.find_least_recently_used_peer() {
                self.remove(&oldest);
            } else {
                break;
            }
        }
    }

    pub fn rejections(&self) -> u64 {
        self.rejections
    }

    pub fn contains(&self, peer_id: &PeerId) -> bool {
        self.peers.contains(peer_id)
    }

    pub fn touch(&mut self, peer_id: PeerId) {
        if self.peers.contains(&peer_id) {
            self.peer_last_seen.insert(peer_id, Instant::now());
        }
    }

    pub fn remove(&mut self, peer_id: &PeerId) {
        self.peers.remove(peer_id);
        self.peer_last_seen.remove(peer_id);
    }

    /// Decide whether `peer_id` may join the tracked peer set.
    pub fn admit(&mut self, peer_id: PeerId) -> PeerAdmitAction {
        if self.peers.contains(&peer_id) {
            self.touch(peer_id);
            return PeerAdmitAction::Admitted;
        }
        if self.peers.len() >= self.max_peers {
            if let Some(oldest) = self.find_least_recently_used_peer() {
                self.remove(&oldest);
                PeerAdmitAction::Evict(oldest)
            } else {
                self.rejections = self.rejections.saturating_add(1);
                tracing::debug!(
                    "peer cap full ({}/{}), rejecting",
                    self.len(),
                    self.max_peers
                );
                PeerAdmitAction::Reject
            }
        } else {
            PeerAdmitAction::Admitted
        }
    }

    pub fn insert(&mut self, peer_id: PeerId) {
        self.peers.insert(peer_id);
        self.peer_last_seen.insert(peer_id, Instant::now());
    }

    fn find_least_recently_used_peer(&self) -> Option<PeerId> {
        self.peer_last_seen
            .iter()
            .min_by_key(|(_, &last_seen)| last_seen)
            .map(|(&peer_id, _)| peer_id)
    }
}

pub struct Libp2pTransport {
    swarm: Swarm<MeshBehaviour>,
    local_peer_id: PeerId,
    keypair: identity::Keypair,
    peer_cap: DirectPeerCap,
    connectivity_rx: Option<watch::Receiver<ConnectivityMode>>,
    internet_enabled: Arc<AtomicBool>,
    bootstrap_peers: Vec<Multiaddr>,
    /// Set after `/p2p-circuit` listen is registered (must not race bootstrap dial).
    relay_inbound_enabled: bool,
    /// When inbound listen was enabled; circuit dials wait for a short grace after this.
    relay_inbound_since: Option<Instant>,
    /// Outbound circuit dials require an accepted relay reservation (not just listen_on).
    relay_reservation_confirmed: bool,
    last_circuit_dial_attempt: Option<Instant>,
    crowd_mode: bool,
    /// Gossip profile at transport init (before auto density backoff).
    baseline_mesh_profile: MeshProfile,
    /// Runtime density backoff (auto crowd when visible peers exceed threshold).
    density_backoff: bool,
    baseline_max_peers: usize,
    relay_pool: RelayPool,
    active_relay_index: usize,
    mdns_dials_in_window: u32,
    mdns_window_start: Option<Instant>,
    kad_query_limiter: FixedWindowRateLimiter,
    bearer: BearerMeshRouter,
    webrtc: BearerWebRtcRouter,
    connectivity_mode: ConnectivityMode,
    /// Throttle redials to stale LAN addrs (e.g. phone in flight mode).
    last_lan_dial: HashMap<String, Instant>,
}

/// Delay circuit dials until the relay client has finished reservation setup.
const RELAY_CIRCUIT_GRACE: Duration = Duration::from_millis(800);
/// Minimum gap between circuit dial batches (avoids relay client oneshot races).
const CIRCUIT_DIAL_COOLDOWN: Duration = Duration::from_secs(5);
const LAN_DIAL_COOLDOWN: Duration = Duration::from_secs(45);

impl Libp2pTransport {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        listen_addr: Multiaddr,
        db_path: &str,
        keypair_path: Option<String>,
        bootstrap_peers: Vec<String>,
        connectivity_rx: Option<watch::Receiver<ConnectivityMode>>,
        storage_key: crate::secrets::StorageKey,
        max_peers: usize,
        crowd_mode: bool,
        internet_enabled: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        let keypair_path = keypair_path.unwrap_or_else(|| ".mycena-node/identity".to_string());
        let key = crate::secrets::load_or_create_keypair(&keypair_path, storage_key)?;
        let local_peer_id = key.public().to_peer_id();
        let mesh_profile = if crowd_mode {
            MeshProfile::Crowd
        } else {
            MeshProfile::from_env()
        };
        let relay_pool = {
            let manifest = load_relay_manifest(db_path);
            let pool = RelayPool::from_registry_manifest(&manifest, include_catalog_relays());
            if pool.is_empty() {
                RelayPool::from_bootstrap_peers(&bootstrap_peers)
            } else {
                pool
            }
        };
        let active_relay_index = relay_pool.select_diverse_index_for(&local_peer_id.to_string());
        // DNS transport required for /dns4/ bootstrap + relay-circuit multiaddrs.
        // Use hickory defaults (Android has no /etc/resolv.conf for system resolver).
        let mesh_profile_for_behaviour = mesh_profile;
        let swarm_builder = SwarmBuilder::with_existing_identity(key.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_quic()
            .with_dns_config(
                libp2p::dns::ResolverConfig::default(),
                libp2p::dns::ResolverOpts::default(),
            );
        let mut swarm = swarm_builder
            .with_relay_client(noise::Config::new, yamux::Config::default)?
            .without_bandwidth_metrics()
            .with_behaviour(move |kp, relay_behaviour| {
                Ok(MeshBehaviour::new(
                    kp,
                    relay_behaviour,
                    mesh_profile_for_behaviour,
                ))
            })?
            .build();

        swarm.listen_on(listen_addr)?;
        let _ = swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?);

        let bootstrap_peers: Vec<Multiaddr> = bootstrap_peers
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        let max_peers = if crowd_mode {
            if max_peers == 0 {
                CROWD_MAX_DIRECT_PEERS
            } else {
                max_peers.min(CROWD_MAX_DIRECT_PEERS)
            }
        } else if max_peers == 0 {
            MAX_DIRECT_PEERS_DEFAULT
        } else {
            max_peers
        };

        let initial_connectivity = connectivity_rx
            .as_ref()
            .map(|rx| *rx.borrow())
            .unwrap_or(ConnectivityMode::MeshOnly);

        let mut transport = Self {
            swarm,
            local_peer_id,
            keypair: key,
            peer_cap: DirectPeerCap::new(max_peers),
            connectivity_rx,
            internet_enabled,
            bootstrap_peers,
            relay_inbound_enabled: false,
            relay_inbound_since: None,
            relay_reservation_confirmed: false,
            last_circuit_dial_attempt: None,
            crowd_mode,
            baseline_mesh_profile: mesh_profile,
            density_backoff: false,
            baseline_max_peers: max_peers,
            relay_pool,
            active_relay_index,
            mdns_dials_in_window: 0,
            mdns_window_start: None,
            kad_query_limiter: FixedWindowRateLimiter::new(
                KAD_QUERY_CAP,
                KAD_QUERY_WINDOW.as_millis() as u64,
            ),
            bearer: BearerMeshRouter::new(),
            webrtc: BearerWebRtcRouter::new(),
            connectivity_mode: initial_connectivity,
            last_lan_dial: HashMap::new(),
        };
        if transport.internet_dials_enabled() {
            transport.dial_all_bootstraps();
        } else {
            transport.dial_lan_bootstrap_peers();
        }
        transport.setup_kad();
        if crowd_mode {
            tracing::info!(
                "crowd mode: max_peers={} mdns_dial_cap={}/min mesh=crowd",
                transport.peer_cap.max_peers(),
                CROWD_MDNS_DIAL_CAP
            );
        } else if mesh_profile == MeshProfile::Dense {
            tracing::info!("gossip mesh profile: dense (MYCENA_MESH_PROFILE)");
        }
        Ok(transport)
    }

    fn internet_dials_enabled(&self) -> bool {
        self.internet_enabled.load(Ordering::Relaxed)
    }

    fn crowd_mdns_dial_allowed(&mut self) -> bool {
        if !self.crowd_mode && !self.density_backoff {
            return true;
        }
        let now = Instant::now();
        if self
            .mdns_window_start
            .is_none_or(|t| now.duration_since(t) >= CROWD_MDNS_DIAL_WINDOW)
        {
            self.mdns_window_start = Some(now);
            self.mdns_dials_in_window = 0;
        }
        if self.mdns_dials_in_window >= CROWD_MDNS_DIAL_CAP {
            tracing::debug!("crowd mode: mDNS dial cap reached");
            return false;
        }
        self.mdns_dials_in_window += 1;
        true
    }

    /// Toggle auto density backoff (tightens mDNS dial cap, direct peer cap, and gossip mesh).
    pub fn set_density_backoff(&mut self, active: bool, density_max_peers: usize) {
        if self.density_backoff == active {
            return;
        }
        self.density_backoff = active;
        let profile = if active {
            crate::gossip_mesh::density_backoff_profile(self.baseline_mesh_profile, self.crowd_mode)
        } else {
            self.baseline_mesh_profile
        };
        crate::gossip_mesh::apply_mesh_profile(&mut self.swarm.behaviour_mut().gossip, profile);
        if active {
            let cap = density_max_peers.min(self.baseline_max_peers);
            self.peer_cap.set_max_peers(cap);
            tracing::info!(
                "density backoff active: max_peers={} mdns_dial_cap={}/min gossip={profile:?}",
                cap,
                CROWD_MDNS_DIAL_CAP
            );
        } else {
            self.peer_cap.set_max_peers(self.baseline_max_peers);
            tracing::info!(
                "density backoff cleared: max_peers={} gossip={profile:?}",
                self.baseline_max_peers
            );
        }
    }

    fn kad_query_allowed(&mut self) -> bool {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if self.kad_query_limiter.allow("kad", now_ms) {
            true
        } else {
            tracing::debug!("kad: query rate cap reached ({KAD_QUERY_CAP}/min)");
            false
        }
    }

    fn rotate_active_relay(&mut self) {
        if !self.internet_dials_enabled() {
            return;
        }
        if self.relay_pool.len() <= 1 {
            return;
        }
        let prev = self.active_relay_index;
        self.active_relay_index = self.relay_pool.rotate_index(prev);
        self.relay_inbound_enabled = false;
        self.relay_inbound_since = None;
        self.relay_reservation_confirmed = false;
        tracing::warn!(
            "relay failover: rotating pool index {prev} -> {}",
            self.active_relay_index
        );
        if let Some(entry) = self.relay_pool.entry(self.active_relay_index) {
            if let Ok(addr) = entry.tcp_multiaddr.parse::<Multiaddr>() {
                if !self
                    .swarm
                    .is_connected(&PeerId::from_str(&entry.peer_id).unwrap_or(self.local_peer_id))
                {
                    if let Err(e) = self.swarm.dial(addr.clone()) {
                        tracing::warn!("relay failover dial {}: {e}", entry.tcp_multiaddr);
                    } else {
                        tracing::info!("relay failover dialing {}", entry.tcp_multiaddr);
                    }
                }
            }
        }
    }

    fn relay_failure_at(&mut self, failed_peer: &PeerId) {
        let pid = failed_peer.to_string();
        let Some(idx) = self
            .relay_pool
            .entries()
            .iter()
            .position(|e| e.peer_id == pid)
        else {
            return;
        };
        self.relay_pool.mark_failure(idx);
        if idx == self.active_relay_index {
            self.rotate_active_relay();
        }
    }

    fn failover_relay(&mut self) {
        self.relay_pool.mark_failure(self.active_relay_index);
        self.rotate_active_relay();
    }

    fn setup_kad(&mut self) {
        self.swarm
            .behaviour_mut()
            .kad
            .set_mode(Some(kad::Mode::Client));
        for addr in &self.bootstrap_peers {
            if let Some(peer_id) = peer_id_from_parsed_multiaddr(addr) {
                self.swarm
                    .behaviour_mut()
                    .kad
                    .add_address(&peer_id, addr.clone());
                tracing::info!("kad: added bootstrap peer {peer_id}");
            }
        }
        if !self.bootstrap_peers.is_empty() {
            match self.swarm.behaviour_mut().kad.bootstrap() {
                Ok(_) => tracing::debug!("kad: bootstrap started"),
                Err(e) => tracing::warn!("kad bootstrap failed: {e}"),
            }
        }
    }

    fn kad_bootstrap(&mut self) {
        if let Err(e) = self.swarm.behaviour_mut().kad.bootstrap() {
            tracing::warn!("kad bootstrap failed: {e}");
        }
    }

    fn set_kad_mode(&mut self, mode: kad::Mode) {
        self.swarm.behaviour_mut().kad.set_mode(Some(mode));
    }

    fn start_kad_find_peer(&mut self, peer_id: PeerId) {
        if !self.kad_query_allowed() {
            return;
        }
        tracing::debug!("kad: lookup closest peers to {peer_id}");
        self.swarm.behaviour_mut().kad.get_closest_peers(peer_id);
    }

    fn kad_maybe_dial(&mut self, peer: PeerId, addrs: impl IntoIterator<Item = Multiaddr>) {
        if peer == self.local_peer_id || self.is_known_relay_peer(&peer.to_string()) {
            return;
        }
        for addr in addrs {
            self.swarm
                .behaviour_mut()
                .kad
                .add_address(&peer, addr.clone());
            if !self.swarm.is_connected(&peer) {
                if let Err(e) = self.swarm.dial(addr) {
                    tracing::debug!("kad dial to {peer}: {e}");
                }
                break;
            }
        }
    }

    fn apply_kad_connectivity(&mut self, action: KadConnectivityAction) {
        self.set_kad_mode(kad::Mode::Client);
        match action {
            KadConnectivityAction::Activate => {
                if self.internet_dials_enabled() {
                    self.dial_all_bootstraps();
                    self.kad_bootstrap();
                }
                tracing::info!("switched to internet mode — kademlia active");
            }
            KadConnectivityAction::Pause => {
                tracing::info!("switched to mesh-only mode — kademlia paused");
            }
        }
    }

    fn is_circuit_dial(addr: &Multiaddr) -> bool {
        addr.iter().any(|p| matches!(p, Protocol::P2pCircuit))
    }

    fn is_known_relay_peer(&self, peer_id: &str) -> bool {
        self.relay_pool.is_relay_peer(peer_id)
    }

    fn active_relay_peer_id(&self) -> Option<PeerId> {
        self.relay_pool
            .entry(self.active_relay_index)
            .and_then(|e| PeerId::from_str(&e.peer_id).ok())
    }

    fn relay_connected(&self) -> bool {
        self.active_relay_peer_id()
            .is_some_and(|id| self.swarm.is_connected(&id))
    }

    /// Relay TCP/QUIC up, reservation accepted, `/p2p-circuit` listen registered, grace elapsed.
    fn relay_ready(&self) -> bool {
        self.relay_inbound_enabled
            && self.relay_connected()
            && self.relay_reservation_confirmed
            && self
                .relay_inbound_since
                .is_some_and(|t| t.elapsed() >= RELAY_CIRCUIT_GRACE)
    }

    fn note_relay_disconnected(&mut self) {
        if !self.relay_connected() {
            self.relay_inbound_enabled = false;
            self.relay_inbound_since = None;
            self.relay_reservation_confirmed = false;
        }
    }

    fn relay_circuit_listen_addr(&self) -> Option<Multiaddr> {
        if let Some(entry) = self.relay_pool.entry(self.active_relay_index) {
            return format!("{}/p2p-circuit", entry.tcp_multiaddr).parse().ok();
        }
        for addr in &self.bootstrap_peers {
            let addr_str = addr.to_string();
            if Self::is_circuit_dial(addr) || !addr_str.contains("/tcp/") {
                continue;
            }
            if peer_id_from_parsed_multiaddr(addr)
                .is_some_and(|pid| self.is_known_relay_peer(&pid.to_string()))
            {
                return format!("{addr_str}/p2p-circuit").parse().ok();
            }
        }
        mycena_core::bootstrap::standard_bootstrap_multiaddrs()
            .iter()
            .find(|addr| addr.contains("/tcp/") && addr.starts_with("/ip4/"))
            .and_then(|addr| format!("{addr}/p2p-circuit").parse().ok())
    }

    /// Listen on the public relay so other peers can reach us via `/p2p-circuit` (NAT traversal).
    /// Must run only after a direct connection to the relay exists; calling this alongside
    /// `dial(bootstrap)` races on the relay TCP port and yields EADDRINUSE / failed handshakes.
    fn enable_relay_inbound_listen(&mut self) {
        if !self.internet_dials_enabled() {
            return;
        }
        if self.relay_inbound_enabled {
            return;
        }
        let Some(listen_addr) = self.relay_circuit_listen_addr() else {
            tracing::warn!("relay inbound listen: no relay bootstrap tcp addr configured");
            return;
        };
        if let Err(e) = self.swarm.listen_on(listen_addr.clone()) {
            tracing::warn!("relay inbound listen on {listen_addr}: {e}");
            return;
        }
        self.relay_inbound_enabled = true;
        self.relay_inbound_since = Some(Instant::now());
        tracing::info!("relay inbound listen enabled on {listen_addr}");
    }

    fn dial_deferred_circuit_targets(&mut self) {
        if !self.relay_ready() {
            tracing::debug!("deferring circuit dials — relay not ready");
            return;
        }
        for addr in self.bootstrap_peers.clone() {
            if !Self::is_circuit_dial(&addr) {
                continue;
            }
            let Some(peer_id) = peer_id_from_parsed_multiaddr(&addr) else {
                continue;
            };
            if peer_id == self.local_peer_id || self.is_known_relay_peer(&peer_id.to_string()) {
                continue;
            }
            if self.swarm.is_connected(&peer_id) {
                continue;
            }
            if let Err(e) = self.swarm.dial(addr.clone()) {
                tracing::warn!("dial {addr}: {e}");
            } else {
                tracing::info!("dialing deferred circuit target {addr}");
            }
            // One circuit dial per batch — parallel dials race the relay client.
            break;
        }
    }

    fn dial_all_bootstraps(&mut self) {
        if !self.internet_dials_enabled() {
            self.dial_lan_bootstrap_peers();
            return;
        }
        let policy = RelayDiversityPolicy::default();
        let seed = self.local_peer_id.to_string();
        let diverse = self.relay_pool.select_parallel_dial_indices(&seed, &policy);
        let mut dialed_relay_indices = std::collections::HashSet::new();
        for idx in diverse {
            dialed_relay_indices.insert(idx);
            if let Some(entry) = self.relay_pool.entry(idx) {
                if let Ok(addr) = entry.tcp_multiaddr.parse::<Multiaddr>() {
                    if let Ok(peer_id) = PeerId::from_str(&entry.peer_id) {
                        if self.swarm.is_connected(&peer_id) {
                            continue;
                        }
                    }
                    if let Err(e) = self.swarm.dial(addr.clone()) {
                        tracing::warn!("dial relay {}: {e}", entry.tcp_multiaddr);
                    } else {
                        tracing::info!("dialing diverse relay {}", entry.tcp_multiaddr);
                    }
                }
            }
        }
        for (idx, entry) in self.relay_pool.entries().iter().enumerate() {
            if dialed_relay_indices.contains(&idx) {
                continue;
            }
            if let Ok(addr) = entry.tcp_multiaddr.parse::<Multiaddr>() {
                if let Ok(peer_id) = PeerId::from_str(&entry.peer_id) {
                    if self.swarm.is_connected(&peer_id) {
                        continue;
                    }
                }
                if let Err(e) = self.swarm.dial(addr.clone()) {
                    tracing::warn!("dial relay {}: {e}", entry.tcp_multiaddr);
                } else {
                    tracing::info!("dialing relay {}", entry.tcp_multiaddr);
                }
            }
        }
        for addr in self.bootstrap_peers.clone() {
            if Self::is_circuit_dial(&addr) {
                continue;
            }
            if let Some(peer_id) = peer_id_from_parsed_multiaddr(&addr) {
                if self.is_known_relay_peer(&peer_id.to_string()) {
                    continue;
                }
                if self.swarm.is_connected(&peer_id) {
                    continue;
                }
            }
            if let Err(e) = self.swarm.dial(addr.clone()) {
                tracing::warn!("dial {addr}: {e}");
            } else {
                tracing::info!("dialing {addr}");
            }
        }
    }

    /// Redial private LAN addresses from bootstrap/mDNS cache (Phase B1).
    fn dial_lan_bootstrap_peers(&mut self) {
        for addr in self.bootstrap_peers.clone() {
            if Self::is_circuit_dial(&addr) || !Self::is_private_lan_multiaddr(&addr) {
                continue;
            }
            let Some(peer_id) = peer_id_from_parsed_multiaddr(&addr) else {
                continue;
            };
            if peer_id == self.local_peer_id || self.is_known_relay_peer(&peer_id.to_string()) {
                continue;
            }
            if self.swarm.is_connected(&peer_id) {
                continue;
            }
            let key = addr.to_string();
            if self
                .last_lan_dial
                .get(&key)
                .is_some_and(|t| t.elapsed() < LAN_DIAL_COOLDOWN)
            {
                continue;
            }
            match self.swarm.dial(addr.clone()) {
                Ok(()) => {
                    self.last_lan_dial.insert(key, Instant::now());
                    tracing::debug!("LAN redial {addr}");
                }
                Err(e) => tracing::debug!("LAN redial {addr}: {e}"),
            }
        }
    }

    fn is_private_lan_multiaddr(addr: &Multiaddr) -> bool {
        use libp2p::multiaddr::Protocol;
        for p in addr.iter() {
            if let Protocol::Ip4(ip) = p {
                let o = ip.octets();
                return o[0] == 10
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 169 && o[1] == 254);
            }
        }
        false
    }

    fn handle_connection_established(&mut self, peer_id: PeerId) -> bool {
        if peer_id == self.local_peer_id || self.is_known_relay_peer(&peer_id.to_string()) {
            return false;
        }
        if self.peer_cap.contains(&peer_id) {
            self.peer_cap.touch(peer_id);
            return false;
        }
        match self.peer_cap.admit(peer_id) {
            PeerAdmitAction::Evict(evict) => {
                tracing::debug!(
                    "peer cap reached ({}), dropping LRU peer {evict}",
                    self.peer_cap.max_peers()
                );
                let _ = self.swarm.disconnect_peer_id(evict);
            }
            PeerAdmitAction::Reject => {
                tracing::debug!("peer cap reached, rejecting new connection from {peer_id}");
                let _ = self.swarm.disconnect_peer_id(peer_id);
                return false;
            }
            PeerAdmitAction::Admitted => {}
        }
        self.peer_cap.insert(peer_id);
        true
    }

    fn queue_dial_target(&mut self, multiaddr: String) -> anyhow::Result<()> {
        if is_ble_multiaddr(&multiaddr) {
            return self.bearer.queue_dial(&multiaddr);
        }
        let addr: Multiaddr = multiaddr.parse()?;
        if Self::is_circuit_dial(&addr) {
            self.bootstrap_peers
                .retain(|existing| !Self::is_circuit_dial(existing));
        }
        if !self.bootstrap_peers.contains(&addr) {
            self.bootstrap_peers.push(addr.clone());
            tracing::info!("queued dial target: {addr}");
        }
        if Self::is_circuit_dial(&addr) {
            if !self.internet_dials_enabled() {
                tracing::debug!("queued circuit dial (internet disabled): {addr}");
                return Ok(());
            }
            if !self.relay_ready() {
                tracing::debug!("queued circuit dial (relay not ready yet): {addr}");
                return Ok(());
            }
            self.try_deferred_circuit_dials();
            return Ok(());
        }
        self.swarm.dial(addr)?;
        Ok(())
    }

    fn handle_connectivity_change(&mut self, mode: ConnectivityMode) {
        self.connectivity_mode = mode;
        let action = kad_action_for_mode(mode);
        self.apply_kad_connectivity(action);
        match mode {
            ConnectivityMode::Internet => {}
            ConnectivityMode::MeshOnly => {
                self.dial_lan_bootstrap_peers();
            }
        }
    }
}

#[async_trait::async_trait]
impl MeshTransport for Libp2pTransport {
    fn local_peer_id(&self) -> String {
        self.local_peer_id.to_string()
    }

    fn known_peers(&self) -> Vec<String> {
        let mut peers: Vec<String> = self
            .peer_cap
            .peers
            .iter()
            .map(ToString::to_string)
            .filter(|p| !self.is_known_relay_peer(p))
            .collect();
        for p in self.bearer.known_peers() {
            if !peers.contains(&p) && !self.is_known_relay_peer(&p) {
                peers.push(p);
            }
        }
        peers
    }

    fn local_keypair(&self) -> Option<identity::Keypair> {
        Some(self.keypair.clone())
    }

    fn is_peer_connected(&self, peer_id: &str) -> bool {
        if self.bearer.is_connected(peer_id) {
            return true;
        }
        PeerId::from_str(peer_id)
            .ok()
            .is_some_and(|id| self.swarm.is_connected(&id))
    }

    fn connected_peer_count(&self) -> usize {
        self.known_peers()
            .into_iter()
            .filter(|p| self.is_peer_connected(p))
            .count()
    }

    fn peer_transport_hint(&self, peer_id: &str) -> String {
        if self.bearer.is_connected(peer_id) {
            return "ble".into();
        }
        if self.is_known_relay_peer(peer_id) {
            return "relay".into();
        }
        if PeerId::from_str(peer_id)
            .ok()
            .is_some_and(|id| self.swarm.is_connected(&id))
        {
            return "tcp".into();
        }
        "unknown".into()
    }

    fn max_direct_peers(&self) -> usize {
        self.peer_cap.max_peers()
    }

    fn peer_cap_rejections(&self) -> u64 {
        self.peer_cap.rejections()
    }

    fn connectivity_mode(&self) -> ConnectivityMode {
        self.connectivity_mode
    }

    fn internet_relay_available(&self) -> bool {
        self.internet_dials_enabled() && self.connectivity_mode == ConnectivityMode::Internet
    }

    fn bearer_is_connected(&self, peer_id: &str) -> bool {
        self.bearer.is_connected(peer_id)
    }

    fn transport_availability_for_peer(
        &self,
        peer_id: &str,
        wifi_direct_cap: bool,
        mesh_peers_nonempty: bool,
    ) -> TransportAvailability {
        let bearer_up = self.bearer.is_connected(peer_id);
        TransportAvailability {
            internet: self.internet_relay_available(),
            wifi_direct: wifi_direct_cap,
            ble: bearer_up || mesh_peers_nonempty,
            lora: false,
            wifi_aware: false,
            webrtc: self.webrtc.is_link_ready(peer_id),
        }
    }

    fn decide_outbound_path(
        &self,
        availability: &TransportAvailability,
        payload_bytes: usize,
        force_bulk: bool,
        now: u64,
    ) -> TransportPath {
        decide_transport_path(
            availability,
            self.bearer.transport_scores(),
            payload_bytes,
            force_bulk,
            now,
        )
    }

    async fn dial_peer(&mut self, multiaddr: String) -> anyhow::Result<()> {
        self.queue_dial_target(multiaddr)
    }

    async fn remember_and_dial(&mut self, multiaddr: String) -> anyhow::Result<()> {
        self.queue_dial_target(multiaddr)
    }

    fn redial_stored_targets(&mut self) {
        self.dial_lan_bootstrap_peers();
        if !self.internet_dials_enabled() {
            return;
        }
        self.try_deferred_circuit_dials();
        self.dial_all_bootstraps();
        self.dial_lan_bootstrap_peers();
        self.try_deferred_circuit_dials();
    }

    fn try_deferred_circuit_dials(&mut self) {
        if !self.internet_dials_enabled() {
            return;
        }
        if !self.relay_ready() {
            return;
        }
        if self
            .last_circuit_dial_attempt
            .is_some_and(|t| t.elapsed() < CIRCUIT_DIAL_COOLDOWN)
        {
            return;
        }
        self.last_circuit_dial_attempt = Some(Instant::now());
        self.dial_deferred_circuit_targets();
    }

    async fn send_direct(&mut self, to_peer: String, message: WireMessage) -> anyhow::Result<()> {
        if self.bearer.is_connected(&to_peer) {
            return self.bearer.send_direct(&to_peer, message);
        }
        let bytes = wire_message_bytes(&message);
        let now = now_ms();
        let latency = self
            .bearer
            .transport_scores()
            .estimated_latency_ms(TransportBearer::InternetRelay, bytes);
        self.bearer.record_observation(
            TransportBearer::InternetRelay,
            TransportScoreSample {
                latency_ms: latency,
                bytes: bytes.min(u32::MAX as usize) as u32,
                success: true,
                at_ms: now,
            },
        );
        let peer = PeerId::from_str(&to_peer)?;
        self.peer_cap.touch(peer);
        let request = DirectMessageRequest { message };
        self.swarm
            .behaviour_mut()
            .direct
            .send_request(&peer, request);
        Ok(())
    }

    async fn publish_scoped(&mut self, scope: ScopeId, payload: Vec<u8>) -> anyhow::Result<()> {
        let topic = gossipsub::IdentTopic::new(scope.clone());
        match self.swarm.behaviour_mut().gossip.publish(topic, payload) {
            Ok(_) => Ok(()),
            Err(e) => {
                // InsufficientPeers is normal before the gossip mesh forms; direct
                // delivery + DTN forwarding still carry the payload.
                warn!("gossip publish on {scope}: {e}");
                Ok(())
            }
        }
    }

    async fn subscribe_scope(&mut self, scope: ScopeId) -> anyhow::Result<()> {
        let topic = gossipsub::IdentTopic::new(scope.clone());
        match self.swarm.behaviour_mut().gossip.subscribe(&topic) {
            Ok(true) => tracing::info!("subscribed to gossip topic: {scope}"),
            Ok(false) => tracing::debug!("already subscribed to gossip topic: {scope}"),
            Err(e) => anyhow::bail!("failed to subscribe to gossip topic {scope}: {e}"),
        }
        Ok(())
    }

    async fn unsubscribe_scope(&mut self, scope: ScopeId) -> anyhow::Result<()> {
        let topic = gossipsub::IdentTopic::new(scope.clone());
        let _ = self.swarm.behaviour_mut().gossip.unsubscribe(&topic);
        tracing::info!("unsubscribed from gossip topic: {scope}");
        Ok(())
    }

    fn kad_find_peer(&mut self, target_peer_id: &str) {
        if let Ok(peer_id) = PeerId::from_str(target_peer_id) {
            self.start_kad_find_peer(peer_id);
        }
    }

    fn kad_on_connectivity_changed(&mut self, mode: ConnectivityMode) {
        self.apply_kad_connectivity(kad_action_for_mode(mode));
    }

    fn set_density_backoff(&mut self, active: bool, max_direct_peers: usize) {
        Libp2pTransport::set_density_backoff(self, active, max_direct_peers);
    }

    fn register_ble_link(&mut self, peer_id: String, device_id: &str) -> Option<TransportEvent> {
        Libp2pTransport::register_ble_link(self, peer_id, device_id)
    }

    fn remove_ble_link(&mut self, peer_id: &str) -> Option<TransportEvent> {
        Libp2pTransport::remove_ble_link(self, peer_id)
    }

    fn ingest_ble_stream_frame(&mut self, from_peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        Libp2pTransport::ingest_ble_stream_frame(self, from_peer, frame)
    }

    fn drain_ble_stream_outbound(&mut self, to_peer: &str, max: usize) -> Vec<Vec<u8>> {
        Libp2pTransport::drain_ble_stream_outbound(self, to_peer, max)
    }

    fn ble_peer_addrs(&self) -> Vec<String> {
        Libp2pTransport::ble_peer_addrs(self)
    }

    fn drain_pending_ble_events(&mut self) -> Vec<TransportEvent> {
        let mut out = Vec::new();
        while let Some(ev) = self.bearer.try_recv_event() {
            out.push(ev);
        }
        out
    }

    fn queue_bearer_side_wire(
        &mut self,
        to_peer: String,
        message: WireMessage,
    ) -> anyhow::Result<()> {
        self.bearer.queue_side_channel_wire(to_peer, &message)
    }

    fn drain_bearer_side_outbound(
        &mut self,
        to_peer: Option<&str>,
        max: usize,
    ) -> Vec<(String, Vec<u8>)> {
        self.bearer.drain_side_outbound(to_peer, max)
    }

    fn queue_bearer_bulk(&mut self, to_peer: String, payload: Vec<u8>) -> anyhow::Result<()> {
        self.bearer.queue_bulk_frame(to_peer, payload)
    }

    fn set_peer_bearer_caps(
        &mut self,
        peer: &str,
        wire: mycena_core::bearer_transport::BearerWireVersion,
        wifi_direct: bool,
    ) {
        self.bearer.set_peer_caps(peer, wire, wifi_direct);
    }

    fn set_energy_profile(&mut self, profile: mycena_core::energy::EnergyProfile) {
        self.bearer.set_energy_profile(profile);
    }

    fn set_webrtc_host_available(&mut self, available: bool) {
        Libp2pTransport::set_webrtc_host_available(self, available);
    }

    fn webrtc_host_available(&self) -> bool {
        self.webrtc.host_available()
    }

    fn webrtc_links_active(&self) -> u64 {
        self.webrtc.active_link_count() as u64
    }

    fn set_webrtc_link_ready(&mut self, peer: &str, ready: bool) {
        Libp2pTransport::set_webrtc_link_ready(self, peer, ready);
    }

    fn webrtc_link_ready(&self, peer: &str) -> bool {
        Libp2pTransport::webrtc_link_ready(self, peer)
    }

    fn schedule_webrtc_negotiation(&mut self, peer: &str) {
        self.webrtc.schedule_negotiation(peer);
    }

    fn drain_webrtc_negotiation_requests(&mut self, max: usize) -> Vec<String> {
        self.webrtc.drain_negotiation_requests(max)
    }

    fn push_webrtc_inbound_signal(
        &mut self,
        signal: mycena_core::webrtc_signaling::WebRtcSignalInbound,
    ) {
        self.webrtc.push_inbound_signal(signal);
    }

    fn drain_webrtc_inbound_signals(
        &mut self,
        max: usize,
    ) -> Vec<mycena_core::webrtc_signaling::WebRtcSignalInbound> {
        self.webrtc.drain_inbound_signals(max)
    }

    fn queue_webrtc_wire(&mut self, to_peer: &str, wire: WireMessage) -> anyhow::Result<()> {
        Libp2pTransport::queue_webrtc_wire(self, to_peer, wire)
    }

    fn drain_bearer_outbound(
        &mut self,
        to_peer: Option<&str>,
        max: usize,
    ) -> Vec<(String, Vec<u8>, mycena_core::bearer_transport::BearerMedium)> {
        let mut out = self.webrtc.drain_outbound(to_peer, max);
        let remaining = max.saturating_sub(out.len());
        if remaining > 0 {
            out.extend(self.bearer.drain_bearer_outbound(to_peer, remaining));
        }
        out
    }

    async fn next_event(&mut self) -> anyhow::Result<TransportEvent> {
        loop {
            if let Some(ev) = self.bearer.try_recv_event() {
                return Ok(ev);
            }
            if let Some(rx) = &mut self.connectivity_rx {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() {
                            self.connectivity_rx = None;
                            continue;
                        }
                        let mode = *rx.borrow_and_update();
                        self.handle_connectivity_change(mode);
                        return Ok(TransportEvent::ConnectivityChanged { mode });
                    }
                    event = self.swarm.select_next_some() => {
                        if let Some(ev) = self.handle_swarm_event(event)? {
                            return Ok(ev);
                        }
                    }
                }
            } else {
                let event = self.swarm.select_next_some().await;
                if let Some(ev) = self.handle_swarm_event(event)? {
                    return Ok(ev);
                }
            }
        }
    }
}

impl Libp2pTransport {
    pub fn register_ble_link(
        &mut self,
        peer_id: String,
        device_id: &str,
    ) -> Option<TransportEvent> {
        tracing::info!("BLE libp2p link registered for {peer_id} ({device_id})");
        self.bearer.register_link(peer_id, device_id)
    }

    pub fn remove_ble_link(&mut self, peer_id: &str) -> Option<TransportEvent> {
        self.bearer.remove_link(peer_id)
    }

    pub fn ingest_ble_stream_frame(&mut self, from_peer: &str, frame: &[u8]) -> anyhow::Result<()> {
        self.bearer.ingest_stream_frame(from_peer, frame)
    }

    pub fn drain_ble_stream_outbound(&mut self, to_peer: &str, max: usize) -> Vec<Vec<u8>> {
        self.bearer.drain_ble_stream_outbound(to_peer, max)
    }

    pub fn ble_peer_addrs(&self) -> Vec<String> {
        self.bearer
            .all_peer_infos()
            .into_iter()
            .map(|p| p.multiaddr)
            .collect()
    }

    pub fn set_webrtc_host_available(&mut self, available: bool) {
        self.webrtc.set_host_available(available);
    }

    pub fn set_webrtc_link_ready(&mut self, peer: &str, ready: bool) {
        self.webrtc.set_link_ready(peer, ready);
    }

    pub fn webrtc_link_ready(&self, peer: &str) -> bool {
        self.webrtc.is_link_ready(peer)
    }

    pub fn queue_webrtc_wire(&mut self, to_peer: &str, wire: WireMessage) -> anyhow::Result<()> {
        let payload = bincode::serialize(&wire)?;
        let style = mycena_core::bearer_transport::default_wire_version();
        let encoded = mycena_core::bearer_transport::encode_bearer_frame_with_limit(
            mycena_core::bearer_transport::BearerPayloadKind::SideChannelWire,
            &payload,
            style,
            mycena_core::bearer_transport::WEBRTC_MAX_FRAME_LEN,
        )?;
        self.webrtc.queue_frame(to_peer, encoded);
        Ok(())
    }

    fn handle_swarm_event(
        &mut self,
        event: SwarmEvent<MeshEvent>,
    ) -> anyhow::Result<Option<TransportEvent>> {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                return Ok(Some(TransportEvent::Listening {
                    address: address.to_string(),
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Mdns(mdns::Event::Discovered(list))) => {
                for (peer, addr) in list {
                    if peer == self.local_peer_id {
                        continue;
                    }
                    self.swarm.behaviour_mut().gossip.add_explicit_peer(&peer);
                    let mut dial_addr = addr.clone();
                    let has_peer = dial_addr
                        .iter()
                        .any(|p| matches!(p, Protocol::P2p(id) if id == peer));
                    if !has_peer {
                        dial_addr.push(Protocol::P2p(peer));
                    }
                    if !self.bootstrap_peers.contains(&dial_addr) {
                        self.bootstrap_peers.push(dial_addr.clone());
                    }
                    if !self.crowd_mdns_dial_allowed() {
                        tracing::debug!(
                            "mDNS discovered {peer} at {addr} (dial deferred — crowd cap)"
                        );
                        continue;
                    }
                    tracing::info!("mDNS discovered {peer} at {addr}, dialing {dial_addr}");
                    if let Err(e) = self.swarm.dial(dial_addr.clone()) {
                        tracing::debug!("mDNS dial to {peer}: {e}");
                    } else if !self.swarm.is_connected(&peer) {
                        if let Err(e) = self.swarm.dial(dial_addr) {
                            tracing::debug!("mDNS redial to {peer}: {e}");
                        }
                    }
                }
            }
            SwarmEvent::Behaviour(MeshEvent::Mdns(mdns::Event::Expired(list))) => {
                // mDNS TTL expiry ≠ TCP disconnect; only adjust gossip if not connected.
                for (peer, _) in list {
                    if peer == self.local_peer_id || self.is_known_relay_peer(&peer.to_string()) {
                        continue;
                    }
                    if self.swarm.is_connected(&peer) {
                        continue;
                    }
                    self.swarm
                        .behaviour_mut()
                        .gossip
                        .remove_explicit_peer(&peer);
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                if let Some(pid) = peer_id {
                    if self.relay_pool.is_relay_peer(&pid.to_string()) {
                        tracing::warn!("relay dial error peer={pid}: {error}");
                        self.relay_failure_at(&pid);
                    }
                }
                tracing::warn!("outgoing connection error peer={peer_id:?}: {error}");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. }
                if self.relay_pool.is_relay_peer(&peer_id.to_string()) =>
            {
                if let Some(idx) = self
                    .relay_pool
                    .entries()
                    .iter()
                    .position(|e| e.peer_id == peer_id.to_string())
                {
                    self.active_relay_index = idx;
                    self.relay_pool.mark_success(idx);
                }
                tracing::info!("connected to relay {peer_id}");
                self.enable_relay_inbound_listen();
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. }
                if self.handle_connection_established(peer_id) =>
            {
                self.swarm
                    .behaviour_mut()
                    .gossip
                    .add_explicit_peer(&peer_id);
                return Ok(Some(TransportEvent::PeerUp {
                    peer_id: peer_id.to_string(),
                }));
            }
            SwarmEvent::ConnectionClosed { peer_id, .. }
                if self.is_known_relay_peer(&peer_id.to_string()) =>
            {
                self.note_relay_disconnected();
                self.failover_relay();
            }
            SwarmEvent::ConnectionClosed { peer_id, .. }
                if !self.is_known_relay_peer(&peer_id.to_string())
                    && self.peer_cap.contains(&peer_id) =>
            {
                self.peer_cap.remove(&peer_id);
                self.swarm
                    .behaviour_mut()
                    .gossip
                    .remove_explicit_peer(&peer_id);
                return Ok(Some(TransportEvent::PeerDown {
                    peer_id: peer_id.to_string(),
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Identify(identify::Event::Received {
                peer_id,
                ..
            })) if self.handle_connection_established(peer_id) => {
                self.swarm
                    .behaviour_mut()
                    .gossip
                    .add_explicit_peer(&peer_id);
                return Ok(Some(TransportEvent::PeerUp {
                    peer_id: peer_id.to_string(),
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Direct(request_response::Event::Message {
                peer,
                message:
                    request_response::Message::Request {
                        request, channel, ..
                    },
                ..
            })) => {
                self.peer_cap.touch(peer);
                let message_id = message_id_for_wire(&request.message);
                let response = make_direct_response(message_id);
                if let Err(err_response) = self
                    .swarm
                    .behaviour_mut()
                    .direct
                    .send_response(channel, response)
                {
                    warn!(
                        "failed to send direct response for message_id={}",
                        err_response.message_id
                    );
                }
                return Ok(Some(TransportEvent::DirectReceived {
                    from_peer: peer.to_string(),
                    message: request.message,
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Direct(request_response::Event::Message {
                peer,
                message: request_response::Message::Response { response, .. },
                ..
            })) => {
                let ack = MessageAck {
                    message_id: response.message_id,
                    accepted: response.accepted,
                };
                return Ok(Some(TransportEvent::DirectAck {
                    from_peer: peer.to_string(),
                    ack,
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Direct(
                request_response::Event::OutboundFailure { peer, error, .. },
            )) => {
                return Ok(Some(TransportEvent::SendFailure {
                    to_peer: peer.to_string(),
                    reason: error.to_string(),
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Gossip(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => {
                return Ok(Some(TransportEvent::ScopedReceived {
                    from_peer: propagation_source.to_string(),
                    scope: message.topic.to_string(),
                    payload: message.data,
                }));
            }
            SwarmEvent::Behaviour(MeshEvent::Kad(kad_event)) => {
                self.handle_kad_event(kad_event);
            }
            SwarmEvent::Behaviour(MeshEvent::Relay(
                libp2p::relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) if self.is_known_relay_peer(&relay_peer_id.to_string()) => {
                self.relay_reservation_confirmed = true;
                tracing::info!("relay reservation accepted via {relay_peer_id}");
                self.try_deferred_circuit_dials();
            }
            SwarmEvent::Behaviour(MeshEvent::Relay(ev)) => {
                tracing::debug!("relay client event: {ev:?}");
            }
            SwarmEvent::Behaviour(MeshEvent::Dcutr(_)) => {}
            _ => {}
        }
        Ok(None)
    }

    fn handle_kad_event(&mut self, event: kad::Event) {
        match event {
            kad::Event::OutboundQueryProgressed {
                result:
                    kad::QueryResult::Bootstrap(Ok(kad::BootstrapOk {
                        num_remaining: 0, ..
                    })),
                ..
            } => {
                tracing::info!("kad: bootstrap complete");
            }
            kad::Event::RoutingUpdated {
                peer, addresses, ..
            } => {
                tracing::debug!("kad: routing table updated, peer={peer}");
                self.kad_maybe_dial(peer, addresses.iter().cloned());
            }
            kad::Event::OutboundQueryProgressed {
                result: kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk { peers, .. })),
                ..
            } => {
                tracing::debug!("kad: found {} close peers", peers.len());
                for info in peers {
                    self.kad_maybe_dial(info.peer_id, info.addrs);
                }
            }
            _ => {}
        }
    }
}

fn hash_joined(prefix: &str, parts: impl IntoIterator<Item = impl AsRef<[u8]>>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prefix.as_bytes());
    for part in parts {
        hasher.update(part.as_ref());
        hasher.update(&[0]);
    }
    format!("{prefix}:{}", hasher.finalize().to_hex())
}

fn message_id_for_wire(message: &WireMessage) -> String {
    match message {
        WireMessage::Data(msg) => msg.envelope.id.0.clone(),
        WireMessage::SyncBloom { bloom, count } => {
            format!("sync_bloom:{}:{}", count, blake3::hash(bloom).to_hex())
        }
        WireMessage::SyncIds { ids } => hash_joined("sync_ids", ids),
        WireMessage::SyncRequest { ids } => hash_joined("sync_req", ids),
        WireMessage::SyncData { messages } => hash_joined(
            "sync_data",
            messages.iter().map(|m| m.envelope.id.0.as_str()),
        ),
        WireMessage::ScopeAnnounce { scopes } => hash_joined("scope_announce", scopes),
        WireMessage::EncryptedDirect {
            encrypted_payload, ..
        } => {
            format!("enc_direct:{}", blake3::hash(encrypted_payload).to_hex())
        }
        WireMessage::EncryptedGroup {
            group_id,
            encrypted_payload,
        } => {
            format!(
                "enc_group:{}:{}",
                group_id,
                blake3::hash(encrypted_payload).to_hex()
            )
        }
        WireMessage::PeerInfo { enc_pubkey_hex, .. } => {
            format!(
                "peer_info:{}",
                blake3::hash(enc_pubkey_hex.as_bytes()).to_hex()
            )
        }
        WireMessage::WebRtcSignal {
            session_id,
            kind,
            payload,
            ..
        } => {
            format!(
                "webrtc_sig:{session_id}:{kind}:{}",
                blake3::hash(payload.as_bytes()).to_hex()
            )
        }
    }
}

fn wire_message_bytes(message: &WireMessage) -> usize {
    match message {
        WireMessage::Data(msg) => msg.envelope.payload.len().saturating_add(msg.body.len()),
        other => bincode::serialize(other).map(|v| v.len()).unwrap_or(256),
    }
}
