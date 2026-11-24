// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::bearer_wifi_aware::BearerWifiAwareRouter;
use crate::custody::{
    CustodyConfig, CustodyForwardPlan, CustodyPendingStatus, CustodyQueue, CustodyRoutingContext,
    DEFAULT_MAX_CUSTODY_SPRAY,
};
use crate::encounter_log::EncounterLogStore;
use crate::forwarding::{
    traffic_class_for_direct, EnergyPolicy, ForwardDecision, ForwardingPolicy, IngestPipeline,
    ProbabilisticForwardingPolicy, SeenCache, SimpleEnergyPolicy, TrafficClass,
};
use crate::ingest_guard::{IngestGuard, IngestReject};
use crate::security::{self, PeerDiversityConfig, Sd030DropReason};
use crate::storage::SledMessageStore;
use crate::transport::Libp2pTransport;
use libp2p::{identity, Multiaddr, PeerId};
use mycena_core::bootstrap::load_bootstrap_peers;
use mycena_core::crowd_density::CrowdDensityConfig;
use mycena_core::crypto::EncryptionKeypair;
use mycena_core::crypto::{self};
use mycena_core::data::{now_ms, Priority};
use mycena_core::energy::{EnergyProfile, MovementIndicator, NodeState, ThermalState};
use mycena_core::lock_or_recover;
use mycena_core::privacy_shaping::{
    self, BatchingScheduler, PrivacyShapingConfig, PrivacyShapingStats,
};
use mycena_core::protocol_capabilities::{NegotiatedProtocol, ProtocolCapabilities};
use mycena_core::sync::BloomFilter;
use mycena_core::topic_shard;
use mycena_core::transport::{
    ConnectivityMode, DirectMessage, MeshTransport, MessageStore, Scope, StoreStats,
    TransportEvent, WireMessage,
};
use mycena_core::transport_score::prefer_wifi_aware_for_discovery;
use mycena_core::transport_score::TransportPath;
use mycena_core::webrtc_signaling::{WebRtcSignalInbound, WebRtcSignalKind, MAX_SIGNALING_PAYLOAD};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info, warn};

type BleWireOutboundReply = tokio::sync::oneshot::Sender<Vec<(String, Vec<u8>)>>;
type BearerOutboundReply = tokio::sync::oneshot::Sender<
    Vec<(String, Vec<u8>, mycena_core::bearer_transport::BearerMedium)>,
>;
type BleStreamOutboundReply = tokio::sync::oneshot::Sender<Vec<Vec<u8>>>;
type E2eEncryptReply = tokio::sync::oneshot::Sender<anyhow::Result<(Vec<u8>, u8)>>;

/// Scopes subscribed on the libp2p gossip layer at node startup.
pub const SYSTEM_SCOPES: &[&str] = &[
    "mycena/chat",
    "mycena/coin/v1",
    "mycena/appstore/v1",
    "mycena/identity/v1",
];

const RELAY_KEY_ROTATION_SECS: u64 = 86_400;

fn default_max_peers() -> usize {
    50
}

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub listen_addr: Multiaddr,
    pub db_path: String,
    pub keypair_path: Option<String>,
    pub forwarding_interval_ms: u64,
    pub sync_interval_secs: u64,
    /// Multiaddrs dialed when [`ConnectivityMode::Internet`]. Empty means
    /// [`NodeRunner::new`] fills this from [`mycena_core::bootstrap::load_bootstrap_peers`].
    pub bootstrap_peers: Vec<String>,
    /// When set, the libp2p transport emits [`TransportEvent::ConnectivityChanged`] on mode flips.
    pub connectivity_rx: Option<tokio::sync::watch::Receiver<ConnectivityMode>>,
    /// Optional display name included in [`WireMessage::PeerInfo`].
    pub display_name: Option<String>,
    /// Optional 32-byte master key for at-rest encryption (Android Keystore-backed prefs).
    pub storage_key: Option<[u8; 32]>,
    /// Max relay candidates when the destination peer is not directly connected.
    pub max_relay_fanout: usize,
    /// When true, register on relay rendezvous and dial other opted-in peers.
    pub rendezvous_enabled: bool,
    /// When false, stay in local-mesh mode even if the public relay is reachable.
    pub internet_enabled: Arc<AtomicBool>,
    /// Sender paired with [`Self::connectivity_rx`] for runtime mode updates (opt-out, probe).
    pub connectivity_mode_tx: Option<tokio::sync::watch::Sender<ConnectivityMode>>,
    /// Bulletin-Board-Scopes die der Nutzer explizit abonniert hat.
    /// Leer = kein Bulletin Board (default).
    pub bulletin_subscriptions: Vec<String>,
    /// Max direkte Peer-Verbindungen. Default 50.
    pub max_peers: usize,
    /// Festival / dense LAN: cap mDNS, dense gossip, lower peer cap (Phase D3).
    pub crowd_mode: bool,
    /// Geohash prefix for location-scoped gossip shards (SOS / bulletin).
    pub local_geohash_prefix: Option<String>,
    /// Auto crowd backoff when visible peers ≥ threshold (0 = disabled).
    pub crowd_density_enter: usize,
    /// Hysteresis: leave density backoff when visible peers ≤ this.
    pub crowd_density_clear: usize,
    /// Enable DTN custody queue (store-and-forward separate from gossip epidemic).
    pub custody_enabled: bool,
    /// Spray-and-wait copy budget per custody message.
    pub max_custody_spray: u8,
    /// SD-070: optional padding, batching, scope bucketing, cover traffic (default off).
    pub privacy_shaping: PrivacyShapingConfig,
    /// Override advertised protocol capabilities (integration tests).
    pub protocol_capabilities_override:
        Option<mycena_core::protocol_capabilities::ProtocolCapabilities>,
}

fn default_crowd_density_enter() -> usize {
    25
}

fn default_crowd_density_clear() -> usize {
    20
}

fn local_geohash_from_env() -> Option<String> {
    std::env::var("MYCENA_LOCAL_GEOHASH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn default_custody_enabled() -> bool {
    true
}

fn default_max_custody_spray() -> u8 {
    DEFAULT_MAX_CUSTODY_SPRAY
}

fn default_max_relay_fanout() -> usize {
    3
}

fn default_internet_enabled() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

fn invariant_default_listen_addr() -> libp2p::Multiaddr {
    const ADDR: &str = "/ip4/0.0.0.0/tcp/0";
    match ADDR.parse() {
        Ok(addr) => addr,
        // INVARIANT: compile-time constant multiaddr; parse cannot fail.
        Err(e) => panic!("default listen addr must parse: {e}"),
    }
}

/// Shared host-stack options (CLI / desktop / UniFFI) so boot literals cannot drift.
pub struct HostNodeOptions {
    pub listen_addr: Multiaddr,
    pub db_path: String,
    pub display_name: Option<String>,
    pub bootstrap_peers: Vec<String>,
    pub storage_key: Option<[u8; 32]>,
    pub rendezvous_enabled: bool,
    pub internet_enabled: Arc<AtomicBool>,
    pub connectivity_rx: Option<tokio::sync::watch::Receiver<ConnectivityMode>>,
    pub connectivity_mode_tx: Option<tokio::sync::watch::Sender<ConnectivityMode>>,
    pub bulletin_subscriptions: Vec<String>,
    pub crowd_mode: bool,
    pub keypair_path: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen_addr: invariant_default_listen_addr(),
            db_path: ".mycena-node".to_string(),
            keypair_path: Some(".mycena-node/identity".to_string()),
            forwarding_interval_ms: 500,
            sync_interval_secs: 30,
            bootstrap_peers: Vec::new(),
            connectivity_rx: None,
            display_name: None,
            storage_key: None,
            max_relay_fanout: default_max_relay_fanout(),
            rendezvous_enabled: true,
            internet_enabled: default_internet_enabled(),
            connectivity_mode_tx: None,
            bulletin_subscriptions: Vec::new(),
            max_peers: default_max_peers(),
            crowd_mode: false,
            local_geohash_prefix: local_geohash_from_env(),
            crowd_density_enter: default_crowd_density_enter(),
            crowd_density_clear: default_crowd_density_clear(),
            custody_enabled: default_custody_enabled(),
            max_custody_spray: default_max_custody_spray(),
            privacy_shaping: PrivacyShapingConfig::disabled(),
            protocol_capabilities_override: None,
        }
    }
}

impl NodeConfig {
    /// Sensible defaults; `bootstrap_peers` stays empty so [`NodeRunner::new`] loads
    /// peers via [`mycena_core::bootstrap::load_bootstrap_peers`].
    pub fn with_defaults(db_path: &str) -> Self {
        Self {
            listen_addr: invariant_default_listen_addr(),
            db_path: db_path.to_string(),
            keypair_path: Some(format!("{db_path}/identity")),
            forwarding_interval_ms: 500,
            sync_interval_secs: 30,
            bootstrap_peers: Vec::new(),
            connectivity_rx: None,
            display_name: None,
            storage_key: None,
            max_relay_fanout: default_max_relay_fanout(),
            rendezvous_enabled: true,
            internet_enabled: default_internet_enabled(),
            connectivity_mode_tx: None,
            bulletin_subscriptions: Vec::new(),
            max_peers: default_max_peers(),
            crowd_mode: false,
            local_geohash_prefix: local_geohash_from_env(),
            crowd_density_enter: default_crowd_density_enter(),
            crowd_density_clear: default_crowd_density_clear(),
            custody_enabled: default_custody_enabled(),
            max_custody_spray: default_max_custody_spray(),
            privacy_shaping: PrivacyShapingConfig::disabled(),
            protocol_capabilities_override: None,
        }
    }

    pub fn for_host(opts: HostNodeOptions) -> Self {
        let keypair_path = opts
            .keypair_path
            .or_else(|| Some(format!("{}/identity", opts.db_path)));
        Self {
            listen_addr: opts.listen_addr,
            db_path: opts.db_path,
            keypair_path,
            forwarding_interval_ms: 500,
            sync_interval_secs: 30,
            bootstrap_peers: opts.bootstrap_peers,
            connectivity_rx: opts.connectivity_rx,
            display_name: opts.display_name,
            storage_key: opts.storage_key,
            max_relay_fanout: default_max_relay_fanout(),
            rendezvous_enabled: opts.rendezvous_enabled,
            internet_enabled: opts.internet_enabled,
            connectivity_mode_tx: opts.connectivity_mode_tx,
            bulletin_subscriptions: opts.bulletin_subscriptions,
            max_peers: default_max_peers(),
            crowd_mode: opts.crowd_mode,
            local_geohash_prefix: local_geohash_from_env(),
            crowd_density_enter: default_crowd_density_enter(),
            crowd_density_clear: default_crowd_density_clear(),
            custody_enabled: default_custody_enabled(),
            max_custody_spray: default_max_custody_spray(),
            privacy_shaping: PrivacyShapingConfig::disabled(),
            protocol_capabilities_override: None,
        }
    }
}

#[derive(Clone)]
struct RelayIdentity {
    keypair: Arc<Mutex<identity::Keypair>>,
    created_at: Arc<Mutex<Instant>>,
}

impl RelayIdentity {
    fn new() -> Self {
        Self {
            keypair: Arc::new(Mutex::new(identity::Keypair::generate_ed25519())),
            created_at: Arc::new(Mutex::new(Instant::now())),
        }
    }

    fn peer_id(&self) -> String {
        // POISON-RECOVER: read-only keypair view; rotation replaces whole keypair atomically.
        lock_or_recover!(self.keypair, "relay keypair")
            .public()
            .to_peer_id()
            .to_string()
    }

    fn age_secs(&self) -> u64 {
        // POISON-RECOVER: reads Instant; worst case triggers early relay rotation.
        lock_or_recover!(self.created_at, "relay created_at")
            .elapsed()
            .as_secs()
    }

    fn maybe_rotate(&self) {
        if self.age_secs() > RELAY_KEY_ROTATION_SECS {
            // POISON-RECOVER: assigns fresh keypair + timestamp; no partial crypto state.
            *lock_or_recover!(self.keypair, "relay keypair") =
                identity::Keypair::generate_ed25519();
            *lock_or_recover!(self.created_at, "relay created_at") = Instant::now();
            info!("relay keypair rotated");
        }
    }

    fn signing_keypair(&self) -> identity::Keypair {
        // POISON-RECOVER: clones entire keypair; safe if prior holder panicked mid-read.
        lock_or_recover!(self.keypair, "relay keypair").clone()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BroadcastPeerFilter {
    #[default]
    All,
    /// Only peers that negotiated payment voucher v1.
    PaymentVoucherV1,
}

#[derive(Debug)]
pub enum NodeCommand {
    SendDirect {
        to_peer: String,
        body: String,
    },
    SendDirectPayload {
        to_peer: String,
        body: String,
        payload: Vec<u8>,
        priority: Option<Priority>,
        app_chat_id: Option<String>,
    },
    Broadcast {
        scope: String,
        body: String,
    },
    BroadcastPayload {
        scope: String,
        body: String,
        payload: Vec<u8>,
        peer_filter: BroadcastPeerFilter,
    },
    SendWire {
        to_peer: String,
        message: WireMessage,
    },
    AddBootstrapPeer {
        multiaddr: String,
    },
    /// Trust-on-first-use: seed X25519 key from invite v2 QR (no libp2p PeerInfo required).
    SeedPeerEncKey {
        peer_id: String,
        enc_pubkey_hex: String,
    },
    /// Seed ratchet prekey bundle from invite v3 tail (first-message v1 E2E).
    SeedPeerRatchetBundle {
        peer_id: String,
        bundle: mycena_core::crypto::ratchet::PrekeyBundle,
    },
    ForgetPeerEncKey {
        peer_id: String,
    },
    /// Query negotiated protocol levels for a peer (tests / coin gating).
    QueryPeerProtocol {
        peer: String,
        reply: tokio::sync::oneshot::Sender<Option<NegotiatedProtocol>>,
    },
    /// Build signed v3 invite including live ratchet prekeys from the runner.
    BuildSignedInviteV3 {
        peer_id: String,
        direct_multiaddr: Option<String>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<String>>,
    },
    SubscribeScope(String),
    UnsubscribeScope(String),
    SubscribeBulletinScope(String),
    UnsubscribeBulletinScope(String),
    GcNow {
        reply: Option<tokio::sync::oneshot::Sender<usize>>,
    },
    StoreStats {
        reply: Option<tokio::sync::oneshot::Sender<StoreStats>>,
    },
    ListPeers,
    ConnectedPeers {
        reply: Option<tokio::sync::oneshot::Sender<Vec<String>>>,
    },
    SetEnergyState(NodeState),
    SetThermalState(ThermalState),
    SetWifiAwareAvailable(bool),
    SetBleDiscoveryActive(bool),
    QueueDiscoveryAnnouncement {
        invite_uri: String,
    },
    IngestWifiAwareDiscovery {
        peer_hint: String,
        invite_uri: String,
        latency_ms: u64,
    },
    RecordBleDiscoveryObservation {
        latency_ms: u64,
        bytes: u32,
        success: bool,
    },
    QueryPreferWifiAwareDiscovery {
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    DrainDiscoveryAnnouncements {
        max: usize,
        reply: tokio::sync::oneshot::Sender<Vec<String>>,
    },
    QueryEnergyProfile {
        reply: tokio::sync::oneshot::Sender<EnergyProfile>,
    },
    SetRendezvousEnabled(bool),
    /// Voluntary opt-out of relay/internet dials (local mesh only).
    SetInternetEnabled(bool),
    /// Ingest a BLE side-channel frame (`mycena_core::ble_mesh`).
    IngestBleFrame {
        from_peer: String,
        frame: Vec<u8>,
    },
    /// Take queued BLE outbound frames for a destination peer (F2).
    DrainBleOutbound {
        to_peer: Option<String>,
        max: usize,
        reply: Option<BleWireOutboundReply>,
    },
    /// Send wire message over libp2p if connected, else queue for BLE (F2).
    SendWireOrQueueBle {
        to_peer: String,
        message: WireMessage,
    },
    /// Encrypt a wrapped E2E direct payload (ratchet when possible, else legacy v0).
    EncryptE2ePayload {
        to_peer: String,
        wrapped: Vec<u8>,
        reply: Option<E2eEncryptReply>,
    },
    /// Register BLE libp2p overlay link (F3).
    RegisterBleLink {
        peer_id: String,
        device_id: String,
    },
    /// Ingest reassembled MYLP stream frame from host GATT (F3).
    BleStreamInbound {
        from_peer: String,
        frame: Vec<u8>,
    },
    /// Take queued F3 stream frames for a peer.
    DrainBleStreamOutbound {
        to_peer: String,
        max: usize,
        reply: Option<BleStreamOutboundReply>,
    },
    /// Take queued bearer frames (F2 + F3) for BLE relay hosts.
    DrainBearerOutbound {
        to_peer: Option<String>,
        max: usize,
        reply: Option<BearerOutboundReply>,
    },
    /// Queue a bulk payload for Wi‑Fi Direct (mini-app bundle, forced upgrade).
    QueueBearerBulk {
        to_peer: String,
        payload: Vec<u8>,
    },
    SetCrowdMode(bool),
    /// SD-070: padding, batching, scope bucketing, cover traffic.
    SetPrivacyShaping(PrivacyShapingConfig),
    /// Local endorsement boosts for relay selection (from UserTrustStore).
    SetLocalEndorsements(HashMap<String, f32>),
    /// Manual crisis mode override (auto when [`CrisisManualMode::FollowAuto`]).
    SetCrisisManual(mycena_core::crisis_mode::CrisisManualMode),
    /// Auto crisis signals from app (internet + trusted nearby SOS sources).
    UpdateCrisisAuto {
        internet_reachable: bool,
        listen_sos: bool,
        local_geohash_prefix: Option<String>,
    },
    /// Set local geohash prefix for SOS/bulletin gossip shards and co-presence hints.
    SetLocalGeohashPrefix(Option<String>),
    /// Follow a contact's signed transport PeerId rotation (SD-190).
    RemapTransportPeer {
        old_peer_id: String,
        new_peer_id: String,
    },
    /// Record a mesh fact-claim observation for corroboration scoring.
    RecordFactObservation {
        relay_peer: String,
        kind: mycena_core::information_trust::FactKind,
        geohash: String,
        category: Option<mycena_core::resource_map::ResourceCategory>,
        content: String,
        observed_at_ms: u64,
    },
    /// Query corroboration trust for a fact claim.
    QueryFactTrust {
        kind: mycena_core::information_trust::FactKind,
        geohash: String,
        category: Option<mycena_core::resource_map::ResourceCategory>,
        content: String,
        ttl_secs: u64,
        reply: tokio::sync::oneshot::Sender<mycena_core::information_trust::InformationTrust>,
    },
    /// Pending DTN custody messages with expected delivery estimates.
    QueryPendingCustody {
        reply: tokio::sync::oneshot::Sender<Vec<CustodyPendingStatus>>,
    },
    QueryConnectivityMode {
        reply: tokio::sync::oneshot::Sender<ConnectivityMode>,
    },
    SendEncryptedDirect {
        to_peer: String,
        inner_plaintext: Vec<u8>,
        priority: Priority,
    },
    EnqueueEncryptedDirectCustody {
        to_peer: String,
        inner_plaintext: Vec<u8>,
        priority: Priority,
        app_chat_id: Option<String>,
        display_body: String,
    },
    /// Host reports WebRTC stack availability (desktop / browser / Android).
    SetWebRtcAvailable(bool),
    /// Host reports RTCDataChannel open/closed toward peer.
    SetWebRtcLinkReady {
        peer: String,
        ready: bool,
    },
    /// Ingest MYBR frame from RTCDataChannel.
    IngestWebRtcFrame {
        from_peer: String,
        frame: Vec<u8>,
    },
    /// Send SDP/ICE over libp2p relay toward peer.
    PostWebRtcSignal {
        to_peer: String,
        session_id: String,
        kind: u8,
        payload: String,
        ice_mline_index: Option<u32>,
    },
    /// Host polls inbound signaling from mesh.
    DrainWebRtcSignalingInbound {
        max: usize,
        reply: tokio::sync::oneshot::Sender<Vec<WebRtcSignalInbound>>,
    },
    /// Host polls peers that need an SDP offer (lower libp2p PeerId initiates).
    DrainWebRtcNegotiationRequests {
        max: usize,
        reply: tokio::sync::oneshot::Sender<Vec<String>>,
    },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub messages_forwarded: u64,
    pub messages_dropped_ttl: u64,
    pub messages_dropped_hops: u64,
    pub messages_dropped_queue: u64,
    pub messages_dropped_invalid_sig: u64,
    pub messages_dropped_no_sig: u64,
    pub messages_dropped_rate_limit: u64,
    pub messages_dropped_replay: u64,
    pub messages_dropped_oversize: u64,
    pub messages_delivered_local: u64,
    pub pending_queue_size: usize,
    pub seen_cache_size: usize,
    pub connected_peers: usize,
    pub max_peers: usize,
    pub peer_cap_rejections: u64,
    pub custody_queue_depth: usize,
    pub custody_forwards: u64,
    pub privacy_shaping_stats: PrivacyShapingStats,
    pub crisis_active: bool,
    /// Auto density backoff active (≥ enter_threshold visible peers).
    pub crowd_density_active: bool,
    /// Background gossip ingest deferred while crisis mode is active.
    pub crisis_dropped_background_gossip: u64,
    /// Background messages held (not forwarded) while crisis mode is active.
    pub crisis_dropped_background_forward: u64,
    /// Background sync (bloom) skipped while crisis mode is active.
    pub crisis_dropped_background_sync: u64,
    /// RTCDataChannel links with negotiated MYBR path ready (ADR 010).
    pub webrtc_links_active: u64,
    /// Host reported WebRTC stack available (desktop / optional Android).
    pub webrtc_host_available: bool,
    /// Wi‑Fi Aware / NSD discovery host available (ADR 009).
    pub wifi_aware_available: bool,
    /// Cumulative Wi‑Fi Aware discovery peer ingest events.
    pub discovery_events_wifi_aware: u64,
    /// Cumulative successful BLE discovery observations.
    pub discovery_events_ble: u64,
    /// Snapshot: prefer Wi‑Fi Aware over BLE for discovery at metrics flush.
    pub prefer_wifi_aware_discovery: bool,
}

use crate::peer_trust::{PeerReputationSnapshot, PeerTrust};

#[derive(Clone)]
pub struct NodeHandle {
    tx: mpsc::Sender<NodeCommand>,
    metrics: Arc<RwLock<NodeMetrics>>,
    incoming_tx: broadcast::Sender<DirectMessage>,
    peers: Arc<RwLock<Vec<String>>>,
    reputations: Arc<crate::peer_trust::PeerTrustStore>,
    scopes: Arc<RwLock<HashSet<String>>>,
    bulletin_subscriptions: Arc<RwLock<Vec<String>>>,
    relay_identity: RelayIdentity,
    enc_pubkey_hex: String,
    peer_x25519: Arc<RwLock<HashMap<String, [u8; 32]>>>,
    listen_addrs: Arc<RwLock<Vec<String>>>,
    account_id: String,
    transport_epoch: u64,
    account_signing_key: Arc<identity::Keypair>,
    invite_direct_cache: Arc<RwLock<Option<String>>>,
}

impl NodeHandle {
    pub async fn send(&self, cmd: NodeCommand) -> anyhow::Result<()> {
        self.tx.send(cmd).await?;
        Ok(())
    }

    pub async fn metrics(&self) -> NodeMetrics {
        self.metrics.read().await.clone()
    }

    pub fn subscribe_incoming(&self) -> broadcast::Receiver<DirectMessage> {
        self.incoming_tx.subscribe()
    }

    pub async fn known_peers(&self) -> Vec<String> {
        self.peers
            .read()
            .await
            .iter()
            .filter(|p| !mycena_core::bootstrap::is_relay_peer(p))
            .cloned()
            .collect()
    }

    /// Peers with an active libp2p or BLE transport path (not just cached in peer cap).
    pub async fn connected_peers(&self) -> Vec<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .tx
            .send(NodeCommand::ConnectedPeers { reply: Some(tx) })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn is_peer_connected(&self, peer: &str) -> bool {
        if mycena_core::bootstrap::is_relay_peer(peer) {
            return false;
        }
        self.connected_peers().await.iter().any(|p| p == peer)
    }

    pub async fn listen_addrs(&self) -> Vec<String> {
        self.listen_addrs.read().await.clone()
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn transport_epoch(&self) -> u64 {
        self.transport_epoch
    }

    pub async fn peer_reputation(&self, peer_id: &str) -> Option<PeerReputationSnapshot> {
        let now = now_ms();
        self.reputations
            .get_cached(peer_id)
            .map(|t| PeerReputationSnapshot::from_trust(&t, now))
    }

    pub async fn subscribed_scopes(&self) -> Vec<String> {
        self.scopes.read().await.iter().cloned().collect()
    }

    pub fn relay_peer_id(&self) -> String {
        self.relay_identity.peer_id()
    }

    pub fn relay_keypair_age_secs(&self) -> u64 {
        self.relay_identity.age_secs()
    }

    pub async fn bulletin_subscriptions(&self) -> Vec<String> {
        self.bulletin_subscriptions.read().await.clone()
    }

    pub async fn subscribe_bulletin_scope(&self, scope: String) -> anyhow::Result<()> {
        self.send(NodeCommand::SubscribeBulletinScope(scope)).await
    }

    pub async fn unsubscribe_bulletin_scope(&self, scope: String) -> anyhow::Result<()> {
        self.send(NodeCommand::UnsubscribeBulletinScope(scope))
            .await
    }

    pub async fn is_bulletin_enabled(&self) -> bool {
        !self.bulletin_subscriptions.read().await.is_empty()
    }

    pub async fn set_crisis_manual(
        &self,
        manual: mycena_core::crisis_mode::CrisisManualMode,
    ) -> anyhow::Result<()> {
        self.send(NodeCommand::SetCrisisManual(manual)).await
    }

    pub async fn update_crisis_auto(
        &self,
        internet_reachable: bool,
        listen_sos: bool,
        local_geohash_prefix: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send(NodeCommand::UpdateCrisisAuto {
            internet_reachable,
            listen_sos,
            local_geohash_prefix: local_geohash_prefix.map(str::to_string),
        })
        .await
    }

    pub async fn set_local_geohash_prefix(&self, prefix: Option<&str>) -> anyhow::Result<()> {
        self.send(NodeCommand::SetLocalGeohashPrefix(
            prefix.map(str::to_string),
        ))
        .await
    }

    pub async fn record_fact_observation(
        &self,
        relay_peer: &str,
        kind: mycena_core::information_trust::FactKind,
        geohash: &str,
        category: Option<mycena_core::resource_map::ResourceCategory>,
        content: &str,
        observed_at_ms: u64,
    ) -> anyhow::Result<()> {
        self.send(NodeCommand::RecordFactObservation {
            relay_peer: relay_peer.to_string(),
            kind,
            geohash: geohash.to_string(),
            category,
            content: content.to_string(),
            observed_at_ms,
        })
        .await
    }

    pub async fn query_fact_trust(
        &self,
        kind: mycena_core::information_trust::FactKind,
        geohash: &str,
        category: Option<mycena_core::resource_map::ResourceCategory>,
        content: &str,
        ttl_secs: u64,
    ) -> mycena_core::information_trust::InformationTrust {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .send(NodeCommand::QueryFactTrust {
                kind,
                geohash: geohash.to_string(),
                category,
                content: content.to_string(),
                ttl_secs,
                reply: tx,
            })
            .await
            .is_err()
        {
            return mycena_core::information_trust::InformationTrust {
                corroboration_count: 0,
                trusted_corroboration_count: 0,
                source_diversity: 0.0,
                geographic_consistency: 1.0,
                staleness_factor: 0.0,
                trust_score: 0.0,
                status: mycena_core::information_trust::CorroborationStatus::Unverified,
            };
        }
        rx.await
            .unwrap_or(mycena_core::information_trust::InformationTrust {
                corroboration_count: 0,
                trusted_corroboration_count: 0,
                source_diversity: 0.0,
                geographic_consistency: 1.0,
                staleness_factor: 0.0,
                trust_score: 0.0,
                status: mycena_core::information_trust::CorroborationStatus::Unverified,
            })
    }

    pub async fn pending_custody_statuses(&self) -> Vec<CustodyPendingStatus> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .send(NodeCommand::QueryPendingCustody { reply: tx })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn connectivity_mode(&self) -> ConnectivityMode {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .send(NodeCommand::QueryConnectivityMode { reply: tx })
            .await
            .is_err()
        {
            return ConnectivityMode::MeshOnly;
        }
        rx.await.unwrap_or(ConnectivityMode::MeshOnly)
    }

    pub async fn mesh_send_encrypted_direct(
        &self,
        to_peer: String,
        inner_plaintext: Vec<u8>,
        priority: Priority,
    ) -> anyhow::Result<()> {
        self.send(NodeCommand::SendEncryptedDirect {
            to_peer,
            inner_plaintext,
            priority,
        })
        .await
    }

    pub async fn set_thermal_state(&self, thermal: ThermalState) -> anyhow::Result<()> {
        self.send(NodeCommand::SetThermalState(thermal)).await
    }

    pub async fn energy_profile(&self) -> EnergyProfile {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .send(NodeCommand::QueryEnergyProfile { reply: tx })
            .await
            .is_err()
        {
            return EnergyProfile::default();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn prefer_wifi_aware_for_discovery(&self) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .send(NodeCommand::QueryPreferWifiAwareDiscovery { reply: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub fn local_enc_pubkey_hex(&self) -> String {
        self.enc_pubkey_hex.clone()
    }

    /// Stable signed v3 invite using the account key and live ratchet prekeys from the runner.
    pub async fn encode_signed_invite_v3(
        &self,
        peer_id: &str,
        direct_multiaddr: Option<&str>,
    ) -> anyhow::Result<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(NodeCommand::BuildSignedInviteV3 {
                peer_id: peer_id.to_string(),
                direct_multiaddr: direct_multiaddr.map(String::from),
                reply: tx,
            })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("invite build response channel closed"))?
    }

    pub fn build_contact_hello(
        &self,
        peer_id: &str,
        display_name: &str,
    ) -> anyhow::Result<mycena_core::contact_hello::ContactHello> {
        mycena_core::contact_hello::ContactHello::sign(
            peer_id,
            &self.account_id,
            display_name,
            &self.enc_pubkey_hex,
            &self.account_signing_key,
        )
    }

    /// First stable LAN multiaddr seen; cached so the signed v3 invite QR does not flip.
    pub async fn stable_direct_multiaddr(&self, local_peer_id: &str) -> Option<String> {
        if let Some(cached) = self.invite_direct_cache.read().await.clone() {
            return Some(cached);
        }
        let listen = self.listen_addrs().await;
        let picked = mycena_core::bootstrap::pick_stable_lan_multiaddr(&listen, local_peer_id);
        if let Some(ref addr) = picked {
            *self.invite_direct_cache.write().await = Some(addr.clone());
        }
        picked
    }

    pub async fn has_enc_key_for(&self, peer: &str) -> bool {
        self.peer_x25519.read().await.contains_key(peer)
    }

    pub async fn encrypt_e2e_payload(
        &self,
        to_peer: &str,
        wrapped: Vec<u8>,
    ) -> anyhow::Result<(Vec<u8>, u8)> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(NodeCommand::EncryptE2ePayload {
                to_peer: to_peer.to_string(),
                wrapped,
                reply: Some(tx),
            })
            .await?;
        rx.await?
    }

    pub async fn seed_peer_enc_key(&self, peer: &str, enc_pubkey_hex: &str) -> anyhow::Result<()> {
        self.mesh_seed_invite_enc_key(
            peer.to_string(),
            enc_pubkey_hex.to_string(),
            mycena_core::consistency::OP_INVITE_PARSE,
        )
        .await
    }

    pub async fn forget_peer_enc_key(&self, peer: &str) -> anyhow::Result<()> {
        self.send(NodeCommand::ForgetPeerEncKey {
            peer_id: peer.to_string(),
        })
        .await
    }

    pub async fn seed_peer_ratchet_bundle(
        &self,
        peer: &str,
        bundle: mycena_core::crypto::ratchet::PrekeyBundle,
    ) -> anyhow::Result<()> {
        self.send(NodeCommand::SeedPeerRatchetBundle {
            peer_id: peer.to_string(),
            bundle,
        })
        .await
    }

    pub async fn query_negotiated_protocol(
        &self,
        peer: &str,
    ) -> anyhow::Result<Option<NegotiatedProtocol>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(NodeCommand::QueryPeerProtocol {
            peer: peer.to_string(),
            reply: tx,
        })
        .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("protocol query response channel closed"))
    }

    pub async fn peer_payment_voucher_v1_allowed(&self, peer: &str) -> bool {
        self.query_negotiated_protocol(peer)
            .await
            .ok()
            .flatten()
            .is_some_and(|n| n.payment_voucher_v1_allowed())
    }

    pub async fn peer_x25519_public(&self, peer: &str) -> Option<crypto::X25519PublicKey> {
        self.peer_x25519
            .read()
            .await
            .get(peer)
            .map(|b| crypto::X25519PublicKey::from(*b))
    }

    pub async fn store_stats(&self) -> anyhow::Result<StoreStats> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(NodeCommand::StoreStats { reply: Some(tx) })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("store stats response channel closed"))
    }

    pub async fn gc_now(&self) -> anyhow::Result<usize> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(NodeCommand::GcNow { reply: Some(tx) }).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("gc response channel closed"))
    }

    pub async fn remap_transport_peer(
        &self,
        old_peer_id: &str,
        new_peer_id: &str,
    ) -> anyhow::Result<()> {
        self.send(NodeCommand::RemapTransportPeer {
            old_peer_id: old_peer_id.to_string(),
            new_peer_id: new_peer_id.to_string(),
        })
        .await
    }
}

pub struct NodeRunner {
    transport: Box<dyn MeshTransport>,
    keypair: Option<identity::Keypair>,
    local_peer_id: String,
    store: Arc<dyn MessageStore>,
    ingest: IngestPipeline,
    seen_cache: BasicSeenCache,
    node_state: NodeState,
    thermal_state: ThermalState,
    wifi_aware: BearerWifiAwareRouter,
    ble_discovery_active: bool,
    forwarding_policy: ProbabilisticForwardingPolicy,
    energy_policy: SimpleEnergyPolicy,
    pending: VecDeque<PendingMessage>,
    pending_cap: usize,
    forwarding_interval_ms: u64,
    sync_interval_secs: u64,
    metrics: Arc<RwLock<NodeMetrics>>,
    incoming_tx: broadcast::Sender<DirectMessage>,
    peers: Arc<RwLock<Vec<String>>>,
    subscribed_scopes: Arc<RwLock<HashSet<String>>>,
    peer_scopes: HashMap<String, HashSet<String>>,
    /// Negotiated protocol levels per peer (after `PeerInfo` exchange).
    peer_protocol_caps: HashMap<String, NegotiatedProtocol>,
    peer_trust: Arc<crate::peer_trust::PeerTrustStore>,
    cmd_rx: mpsc::Receiver<NodeCommand>,
    enc_keypair: EncryptionKeypair,
    display_name: Option<String>,
    peer_x25519: Arc<RwLock<HashMap<String, [u8; 32]>>>,
    listen_addrs: Arc<RwLock<Vec<String>>>,
    rendezvous_enabled: Arc<AtomicBool>,
    internet_enabled: Arc<AtomicBool>,
    connectivity_mode_tx: Option<tokio::sync::watch::Sender<ConnectivityMode>>,
    db_path: String,
    relay_identity: RelayIdentity,
    bulletin_subscriptions: Arc<RwLock<Vec<String>>>,
    ingest_guard: IngestGuard,
    peer_enc_store: Arc<crate::peer_enc_keys::PeerEncKeyStore>,
    account_id: String,
    transport_epoch: u64,
    account_signing_key: Arc<identity::Keypair>,
    pending_identity_rotation: Option<mycena_core::identity_rotation::IdentityRotationGossip>,
    identity_rotation_pending_peers: HashSet<String>,
    crowd_mode: Arc<AtomicBool>,
    rendezvous_dial_bucket: u32,
    ratchet_store: Arc<crate::ratchet_store::RatchetSessionStore>,
    local_prekeys: Arc<Mutex<mycena_core::crypto::ratchet::LocalPrekeyStore>>,
    encounter_log: EncounterLogStore,
    custody_queue: CustodyQueue,
    custody_enabled: bool,
    local_geohash_prefix: Option<String>,
    crowd_density_config: CrowdDensityConfig,
    crowd_density_active: bool,
    baseline_max_relay_fanout: usize,
    privacy_shaping: PrivacyShapingConfig,
    batch_scheduler: BatchingScheduler,
    privacy_stats: PrivacyShapingStats,
    last_cover_at: Option<Instant>,
    local_endorsements: Arc<RwLock<HashMap<String, f32>>>,
    crisis_state: mycena_core::crisis_mode::CrisisModeState,
    crisis_params: mycena_core::crisis_mode::CrisisRuntimeParams,
    crisis_auto_config: mycena_core::crisis_mode::CrisisAutoConfig,
    sos_crisis_tracker: crate::sos_crisis_gate::SosCrisisTracker,
    information_trust_tracker: crate::information_trust_tracker::InformationTrustTracker,
    protocol_capabilities_override: Option<ProtocolCapabilities>,
    last_forward_at: Instant,
    last_sync_at: Instant,
}

async fn resolve_logical_sender(
    peer_x25519: &Arc<RwLock<HashMap<String, [u8; 32]>>>,
    sender_enc_pubkey: &str,
    from_peer: &str,
) -> String {
    if let Ok(pk) = crypto::parse_x25519_public_hex(sender_enc_pubkey) {
        let target = pk.as_bytes();
        let map = peer_x25519.read().await;
        if let Some(id) = map
            .iter()
            .find(|(_, v)| v.as_slice() == target)
            .map(|(k, _)| k.clone())
        {
            return id;
        }
    }
    from_peer.to_string()
}

async fn persist_peer_enc_key(
    peer_x25519: Arc<RwLock<HashMap<String, [u8; 32]>>>,
    peer_enc_store: Arc<crate::peer_enc_keys::PeerEncKeyStore>,
    peer_id: String,
    key: [u8; 32],
) {
    peer_x25519.write().await.insert(peer_id.clone(), key);
    if let Err(e) = peer_enc_store.save(&peer_id, &key) {
        warn!("failed to persist enc key for {peer_id}: {e}");
    }
}

impl NodeRunner {
    pub fn new(config: NodeConfig) -> anyhow::Result<(Self, NodeHandle)> {
        if let Err(e) = mycena_core::bootstrap::sanitize_persisted_dial_peers(&config.db_path) {
            tracing::warn!("could not sanitize dial peers: {e}");
        }
        let mut bootstrap_peers = if config.bootstrap_peers.is_empty() {
            load_bootstrap_peers(&config.db_path)
        } else {
            config.bootstrap_peers.clone()
        };
        for extra in mycena_core::bootstrap::load_persisted_dial_peers(&config.db_path) {
            if !bootstrap_peers.iter().any(|p| p == &extra) {
                bootstrap_peers.push(extra);
            }
        }
        // At most one stored circuit target (latest QR scan).
        let mut last_circuit: Option<String> = None;
        bootstrap_peers.retain(|p| {
            if p.contains("/p2p-circuit/") {
                last_circuit = Some(p.clone());
                false
            } else {
                true
            }
        });
        if let Some(circuit) = last_circuit {
            bootstrap_peers.push(circuit);
        }
        let keypair_path = config
            .keypair_path
            .clone()
            .or_else(|| Some(format!("{}/identity", config.db_path)));
        let identity = crate::secrets::bootstrap_identities(
            &config.db_path,
            keypair_path.as_deref().unwrap_or("identity"),
            config.storage_key,
        )?;
        let transport = Libp2pTransport::new(
            config.listen_addr.clone(),
            &config.db_path,
            keypair_path,
            bootstrap_peers,
            config.connectivity_rx.clone(),
            config.storage_key,
            config.max_peers,
            config.crowd_mode,
            config.internet_enabled.clone(),
        )?;
        Self::new_with_transport(config, Box::new(transport), identity)
    }

    pub fn new_with_transport(
        config: NodeConfig,
        transport: Box<dyn MeshTransport>,
        identity: crate::secrets::IdentityBootstrap,
    ) -> anyhow::Result<(Self, NodeHandle)> {
        let keypair = transport.local_keypair();
        let local_peer_id = transport.local_peer_id();
        let enc_keypair =
            crate::secrets::load_or_create_encryption_keypair(&config.db_path, config.storage_key)?;
        let enc_pubkey_hex = enc_keypair.public_hex();
        let peer_enc_store = Arc::new(crate::peer_enc_keys::PeerEncKeyStore::open(
            &config.db_path,
        )?);
        let initial_enc = peer_enc_store.load_all().unwrap_or_else(|e| {
            warn!("could not load persisted peer enc keys: {e}");
            HashMap::new()
        });
        if !initial_enc.is_empty() {
            info!("loaded {} persisted peer enc key(s)", initial_enc.len());
        }
        let peer_x25519 = Arc::new(RwLock::new(initial_enc));
        let ratchet_store = Arc::new(crate::ratchet_store::RatchetSessionStore::open(
            &config.db_path,
        )?);
        let local_prekeys = match ratchet_store.load_local_prekeys()? {
            Some(pk) => pk,
            None => {
                let pk = mycena_core::crypto::ratchet::LocalPrekeyStore::generate(&enc_keypair, 32);
                ratchet_store.save_local_prekeys(&pk)?;
                pk
            }
        };
        let local_prekeys = Arc::new(Mutex::new(local_prekeys));
        let listen_addrs = Arc::new(RwLock::new(Vec::new()));
        let display_name = config.display_name.clone();
        let store = Arc::new(SledMessageStore::open(&config.db_path)?);
        let ingest = IngestPipeline::new(store.clone());
        let metrics = Arc::new(RwLock::new(NodeMetrics::default()));
        let peers = Arc::new(RwLock::new(Vec::new()));
        let peer_trust = Arc::new(crate::peer_trust::PeerTrustStore::open(&config.db_path)?);
        if !peer_trust.load_all().is_empty() {
            info!(
                "loaded {} persisted peer trust record(s)",
                peer_trust.load_all().len()
            );
        }
        let mut initial_scopes: HashSet<String> =
            SYSTEM_SCOPES.iter().map(|s| (*s).to_string()).collect();
        initial_scopes.insert("mycena/group/*".to_string());
        let scopes = Arc::new(RwLock::new(initial_scopes));
        let bulletin_subscriptions = Arc::new(RwLock::new(config.bulletin_subscriptions.clone()));
        let relay_identity = RelayIdentity::new();
        let (incoming_tx, _) = broadcast::channel(512);
        let (tx, cmd_rx) = mpsc::channel(64);
        let rendezvous_enabled = Arc::new(AtomicBool::new(config.rendezvous_enabled));
        let crowd_mode = Arc::new(AtomicBool::new(config.crowd_mode));
        let account_id = identity.account_id.clone();
        let transport_epoch = identity.transport_meta.epoch.max(1);
        let pending_identity_rotation = identity.pending_rotation;
        let account_signing_key = Arc::new(identity.account_keypair);
        let invite_direct_cache = Arc::new(RwLock::new(None));
        let encounter_log = EncounterLogStore::open(&config.db_path)?;
        let custody_queue = CustodyQueue::open(
            &config.db_path,
            CustodyConfig {
                max_spray_copies: config.max_custody_spray,
                ..CustodyConfig::default()
            },
        )?;
        let crowd_density_config = CrowdDensityConfig {
            enter_threshold: config.crowd_density_enter,
            clear_threshold: config.crowd_density_clear,
            ..CrowdDensityConfig::default()
        };
        let baseline_max_relay_fanout = config.max_relay_fanout;
        Ok((
            Self {
                local_peer_id,
                transport,
                keypair,
                store,
                ingest,
                seen_cache: BasicSeenCache::new(16_384),
                node_state: NodeState::Active,
                thermal_state: ThermalState::default(),
                wifi_aware: BearerWifiAwareRouter::new(),
                ble_discovery_active: false,
                forwarding_policy: ProbabilisticForwardingPolicy,
                energy_policy: SimpleEnergyPolicy,
                pending: VecDeque::new(),
                pending_cap: 1024,
                forwarding_interval_ms: config.forwarding_interval_ms,
                sync_interval_secs: config.sync_interval_secs,
                metrics: metrics.clone(),
                incoming_tx: incoming_tx.clone(),
                peers: peers.clone(),
                subscribed_scopes: scopes.clone(),
                peer_scopes: HashMap::new(),
                peer_protocol_caps: HashMap::new(),
                peer_trust: peer_trust.clone(),
                cmd_rx,
                enc_keypair,
                display_name,
                peer_x25519: peer_x25519.clone(),
                listen_addrs: listen_addrs.clone(),
                rendezvous_enabled: rendezvous_enabled.clone(),
                internet_enabled: config.internet_enabled.clone(),
                connectivity_mode_tx: config.connectivity_mode_tx.clone(),
                db_path: config.db_path.clone(),
                relay_identity: relay_identity.clone(),
                bulletin_subscriptions: bulletin_subscriptions.clone(),
                ingest_guard: IngestGuard::new(),
                peer_enc_store,
                account_id: account_id.clone(),
                transport_epoch,
                account_signing_key: account_signing_key.clone(),
                pending_identity_rotation,
                identity_rotation_pending_peers: HashSet::new(),
                crowd_mode: crowd_mode.clone(),
                rendezvous_dial_bucket: 0,
                ratchet_store: ratchet_store.clone(),
                local_prekeys: local_prekeys.clone(),
                encounter_log,
                custody_queue,
                custody_enabled: config.custody_enabled,
                local_geohash_prefix: config.local_geohash_prefix.clone(),
                crowd_density_config,
                crowd_density_active: false,
                baseline_max_relay_fanout,
                privacy_shaping: config.privacy_shaping.clone(),
                batch_scheduler: BatchingScheduler::default(),
                privacy_stats: PrivacyShapingStats::default(),
                last_cover_at: None,
                local_endorsements: Arc::new(RwLock::new(HashMap::new())),
                crisis_state: mycena_core::crisis_mode::CrisisModeState::default(),
                crisis_params: mycena_core::crisis_mode::CrisisRuntimeParams::from_baseline(
                    mycena_core::crisis_mode::CrisisBaseline {
                        forwarding_interval_ms: config.forwarding_interval_ms,
                        sync_interval_secs: config.sync_interval_secs,
                        max_relay_fanout: baseline_max_relay_fanout,
                    },
                ),
                crisis_auto_config: mycena_core::crisis_mode::CrisisAutoConfig::default(),
                sos_crisis_tracker: crate::sos_crisis_gate::SosCrisisTracker::default(),
                information_trust_tracker:
                    crate::information_trust_tracker::InformationTrustTracker::open(
                        &config.db_path,
                    )?,
                protocol_capabilities_override: config.protocol_capabilities_override,
                last_forward_at: Instant::now(),
                last_sync_at: Instant::now(),
            },
            NodeHandle {
                tx,
                metrics,
                incoming_tx,
                peers,
                reputations: peer_trust,
                scopes,
                bulletin_subscriptions,
                relay_identity,
                enc_pubkey_hex,
                peer_x25519,
                listen_addrs,
                account_id,
                transport_epoch,
                account_signing_key,
                invite_direct_cache,
            },
        ))
    }

    pub fn local_peer_id(&self) -> &str {
        &self.local_peer_id
    }

    pub fn relay_peer_id(&self) -> String {
        self.relay_identity.peer_id()
    }

    fn maybe_rotate_relay_keypair(&self) {
        self.relay_identity.maybe_rotate();
    }

    /// Test helper: backdate relay key creation.
    #[doc(hidden)]
    pub fn set_relay_keypair_age_for_test(&self, age_secs: u64) {
        let created = Instant::now() - Duration::from_secs(age_secs);
        // POISON-RECOVER: test-only backdate; assigns whole Instant value.
        *lock_or_recover!(self.relay_identity.created_at, "relay created_at") = created;
    }

    /// Rotate the relay keypair when it is older than 24 hours.
    #[doc(hidden)]
    pub fn rotate_relay_keypair_if_due(&self) {
        self.relay_identity.maybe_rotate();
    }

    fn apply_relay_forward_mask(&self, message: &mut DirectMessage) {
        let relay_id = self.relay_identity.peer_id();
        if message.envelope.author_peer.is_none() {
            message.envelope.author_peer = Some(message.envelope.from_peer.clone());
        }
        message.envelope.from_peer = relay_id;
    }

    fn crisis_active(&self) -> bool {
        self.crisis_state.is_active()
    }

    fn effective_forwarding_interval_ms(&self) -> u64 {
        let profile = self.energy_profile();
        if self.crisis_active() {
            let params =
                mycena_core::crisis_mode::adjust_runtime_params(self.crisis_params, &profile);
            params.forwarding_interval_ms
        } else if profile.thermal == ThermalState::Critical {
            self.forwarding_interval_ms.saturating_mul(2)
        } else if profile.thermal == ThermalState::Warm {
            self.forwarding_interval_ms
                .saturating_add(self.forwarding_interval_ms / 4)
        } else {
            self.forwarding_interval_ms
        }
    }

    fn effective_sync_interval_secs(&self) -> u64 {
        if self.crisis_active() {
            self.crisis_params.sync_interval_secs
        } else {
            self.sync_interval_secs
        }
    }

    fn effective_crisis_batch_window(&self) -> (u64, u64) {
        if self.crisis_active() {
            (
                self.crisis_params.batch_min_ms,
                self.crisis_params.batch_max_ms,
            )
        } else {
            (
                self.privacy_shaping.batch_min_ms,
                self.privacy_shaping.batch_max_ms,
            )
        }
    }

    fn effective_privacy_shaping(&self) -> PrivacyShapingConfig {
        if !self.crisis_active() {
            return self.privacy_shaping.clone();
        }
        let mut cfg = self.privacy_shaping.clone();
        let (min, max) = self.effective_crisis_batch_window();
        cfg.batch_min_ms = min;
        cfg.batch_max_ms = max;
        cfg
    }

    fn update_transport_metrics(&self, metrics: &mut NodeMetrics) {
        metrics.connected_peers = self.transport.connected_peer_count();
        metrics.max_peers = self.transport.max_direct_peers();
        metrics.peer_cap_rejections = self.transport.peer_cap_rejections();
        metrics.webrtc_links_active = self.transport.webrtc_links_active();
        metrics.webrtc_host_available = self.transport.webrtc_host_available();
        metrics.wifi_aware_available = self.wifi_aware.is_available();
        metrics.discovery_events_wifi_aware = self.wifi_aware.discovery_events_wifi_aware();
        metrics.discovery_events_ble = self.wifi_aware.discovery_events_ble();
        metrics.prefer_wifi_aware_discovery = prefer_wifi_aware_for_discovery(
            self.wifi_aware.is_available(),
            self.ble_discovery_active,
            self.wifi_aware.transport_scores(),
            now_ms(),
        );
    }

    fn energy_profile(&self) -> EnergyProfile {
        let movement = self
            .encounter_log
            .estimate_movement_indicator(now_ms())
            .unwrap_or(MovementIndicator::Unknown);
        EnergyProfile {
            battery: self.node_state,
            thermal: self.thermal_state,
            movement,
        }
    }

    fn sync_energy_profile_to_transport(&mut self) {
        let profile = self.energy_profile();
        self.transport.set_energy_profile(profile);
    }

    fn effective_forwarding_state(&self) -> NodeState {
        self.energy_profile()
            .effective_forwarding_state(self.crowd_density_active)
    }

    fn effective_crisis_params(&self) -> mycena_core::crisis_mode::CrisisRuntimeParams {
        mycena_core::crisis_mode::adjust_runtime_params(self.crisis_params, &self.energy_profile())
    }

    fn effective_max_relay_fanout(&self) -> usize {
        let base = if self.crowd_density_active {
            self.crowd_density_config
                .max_relay_fanout
                .min(self.baseline_max_relay_fanout)
        } else {
            self.baseline_max_relay_fanout
        };
        if self.crisis_active() {
            mycena_core::crisis_mode::effective_relay_fanout_with_profile(
                base,
                &self.effective_crisis_params(),
                &self.energy_profile(),
            )
        } else {
            base
        }
    }

    fn local_protocol_capabilities(&self) -> ProtocolCapabilities {
        if let Some(caps) = self.protocol_capabilities_override {
            return caps;
        }
        ProtocolCapabilities::current(self.custody_enabled, true)
    }

    fn relay_diversity_config_for(&self, dest_peer: &str) -> PeerDiversityConfig {
        if !self
            .negotiated_protocol_for(dest_peer)
            .is_some_and(|n| n.relay_diversity_allowed())
        {
            return PeerDiversityConfig::score_only();
        }
        if self.crowd_density_active {
            PeerDiversityConfig::dense_local_sybil()
        } else {
            PeerDiversityConfig::default()
        }
    }

    fn peer_payment_voucher_v1_allowed(&self, peer: &str) -> bool {
        self.negotiated_protocol_for(peer)
            .is_some_and(|n| n.payment_voucher_v1_allowed())
    }

    fn apply_negotiated_bearer_caps(&mut self, peer: &str, negotiated: NegotiatedProtocol) {
        let local_wire = self.local_protocol_capabilities().bearer_wire_max;
        let wire = negotiated.bearer_wire.min(local_wire);
        let style = if wire >= mycena_core::protocol_capabilities::bearer::V1_MYBR {
            mycena_core::bearer_transport::BearerWireVersion::V1
        } else {
            mycena_core::bearer_transport::BearerWireVersion::Legacy
        };
        let wifi = negotiated.wifi_direct_allowed();
        self.transport.set_peer_bearer_caps(peer, style, wifi);
    }

    fn maybe_schedule_webrtc_negotiation(&mut self, peer: &str) {
        if !self.transport.webrtc_host_available() {
            return;
        }
        if self.transport.webrtc_link_ready(peer) {
            return;
        }
        if !self
            .peer_protocol_caps
            .get(peer)
            .is_some_and(|n| n.webrtc_allowed())
        {
            return;
        }
        if !self.transport.is_peer_connected(peer) {
            return;
        }
        if self.local_peer_id.as_str() >= peer {
            return;
        }
        self.transport.schedule_webrtc_negotiation(peer);
    }

    fn peer_allowed_for_broadcast(&self, peer: &str, filter: BroadcastPeerFilter) -> bool {
        match filter {
            BroadcastPeerFilter::All => true,
            BroadcastPeerFilter::PaymentVoucherV1 => self.peer_payment_voucher_v1_allowed(peer),
        }
    }

    fn negotiated_protocol_for(&self, peer: &str) -> Option<NegotiatedProtocol> {
        self.peer_protocol_caps.get(peer).copied()
    }

    fn peer_custody_allowed(&self, peer: &str) -> bool {
        if !self.custody_enabled {
            return false;
        }
        self.negotiated_protocol_for(peer)
            .map(|n| n.custody_v1_allowed())
            .unwrap_or(false)
    }

    fn build_routing_meta(
        &self,
        peers: &[String],
        peer_trust: &HashMap<String, crate::peer_trust::PeerTrust>,
    ) -> anyhow::Result<HashMap<String, security::PeerRoutingMeta>> {
        let mut encounter_first = HashMap::new();
        let mut transport_hints = HashMap::new();
        for p in peers {
            if let Ok(Some(rec)) = self.encounter_log.record(p) {
                encounter_first.insert(p.clone(), rec.first_seen_ms);
            }
            transport_hints.insert(p.clone(), self.transport.peer_transport_hint(p));
        }
        Ok(security::build_routing_meta_map(
            peers,
            peer_trust,
            &encounter_first,
            &transport_hints,
        ))
    }

    async fn update_crowd_density_backoff(&mut self) {
        let visible = self.transport.connected_peer_count();
        let next = self
            .crowd_density_config
            .should_activate(visible, self.crowd_density_active);
        if next == self.crowd_density_active {
            return;
        }
        self.crowd_density_active = next;
        self.transport
            .set_density_backoff(next, self.crowd_density_config.max_direct_peers);
        self.metrics.write().await.crowd_density_active = next;
        if next {
            info!("auto crowd density backoff active ({visible} visible peers)");
        } else {
            info!("auto crowd density backoff cleared ({visible} visible peers)");
        }
    }

    fn normalize_geohash_prefix(prefix: &str) -> Option<String> {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            return None;
        }
        mycena_core::topic_shard::shard_prefix(trimmed).or_else(|| {
            if trimmed.len() >= 4 {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
    }

    async fn apply_local_geohash_prefix(&mut self, prefix: Option<String>) {
        self.local_geohash_prefix = prefix.as_deref().and_then(Self::normalize_geohash_prefix);
        let subs = self.bulletin_subscriptions.read().await.clone();
        for scope in subs {
            if mycena_core::topic_shard::supports_geohash_sharding(&scope) {
                if let Err(e) = self.subscribe_gossip_for_scope(&scope).await {
                    warn!("refresh geohash shard subscription for {scope}: {e}");
                }
            }
        }
    }

    async fn subscribe_gossip_for_scope(&mut self, scope: &str) -> anyhow::Result<()> {
        let local_gh = self.local_geohash_prefix.as_deref();
        let topics = topic_shard::resolve_subscribe_topics(scope, local_gh);
        if topic_shard::supports_geohash_sharding(scope) {
            let pattern = topic_shard::subscribe_pattern(scope);
            self.subscribed_scopes.write().await.insert(pattern);
        }
        if topics.is_empty() {
            if topic_shard::supports_geohash_sharding(scope) {
                tracing::warn!(
                    "bulletin scope {scope} requires local_geohash_prefix for gossip shards"
                );
            } else if let Err(e) = self.transport.subscribe_scope(scope.to_string()).await {
                warn!("failed to subscribe to bulletin scope {scope}: {e}");
            } else {
                info!("bulletin subscribed: {scope}");
            }
            return Ok(());
        }
        for topic in topics {
            if let Err(e) = self.transport.subscribe_scope(topic.clone()).await {
                warn!("failed to subscribe to gossip shard {topic}: {e}");
            } else {
                info!("bulletin subscribed (shard): {topic}");
            }
        }
        Ok(())
    }

    async fn find_peer_via_kad(&mut self, target_peer_id: &str) {
        self.transport.kad_find_peer(target_peer_id);
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        info!("node started with peer_id={}", self.local_peer_id);
        let crowd = self.crowd_mode.load(Ordering::Relaxed);
        let bulletin_scopes = self.bulletin_subscriptions.read().await.clone();
        if crowd {
            for scope in bulletin_scopes {
                if let Err(e) = self.subscribe_gossip_for_scope(&scope).await {
                    warn!("failed to subscribe to bulletin scope {scope}: {e}");
                }
                self.subscribed_scopes.write().await.insert(scope);
            }
        }
        for scope in SYSTEM_SCOPES {
            if let Err(e) = self.transport.subscribe_scope(scope.to_string()).await {
                warn!("failed to subscribe to {scope}: {e}");
            }
        }
        if !crowd {
            let bulletin_scopes = self.bulletin_subscriptions.read().await.clone();
            for scope in bulletin_scopes {
                if let Err(e) = self.subscribe_gossip_for_scope(&scope).await {
                    warn!("failed to subscribe to bulletin scope {scope}: {e}");
                }
                self.subscribed_scopes.write().await.insert(scope);
            }
        }
        self.update_crowd_density_backoff().await;
        let mut fast_tick = interval(Duration::from_millis(50));
        fast_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut gc_tick = interval(Duration::from_secs(6 * 60 * 60));
        gc_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let (rendezvous_tx, mut rendezvous_rx) = mpsc::channel::<Vec<String>>(8);
        let rendezvous_kp = self.keypair.clone();
        let rendezvous_flag = self.rendezvous_enabled.clone();
        let internet_flag = self.internet_enabled.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(45));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if !rendezvous_flag.load(Ordering::Relaxed)
                    || !internet_flag.load(Ordering::Relaxed)
                {
                    continue;
                }
                let kp = rendezvous_kp.clone();
                let remote_ids = tokio::task::spawn_blocking(move || {
                    if let Some(ref key) = kp {
                        mycena_core::rendezvous::set_relay_rendezvous_registration(key, true, None);
                    }
                    mycena_core::rendezvous::fetch_relay_rendezvous(None)
                })
                .await
                .unwrap_or_default();
                if rendezvous_tx.send(remote_ids).await.is_err() {
                    break;
                }
            }
        });

        self.transport.redial_stored_targets();
        if let Some(rot) = self.pending_identity_rotation.take() {
            let failed = self.announce_identity_rotation_e2e(rot.clone()).await;
            if !failed.is_empty() {
                self.pending_identity_rotation = Some(rot);
                self.identity_rotation_pending_peers = failed.into_iter().collect();
            }
        }
        let mut dial_tick = interval(Duration::from_secs(15));
        dial_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut circuit_dial_tick = interval(Duration::from_secs(5));
        circuit_dial_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                transport_event = self.transport.next_event() => {
                    match transport_event {
                        Ok(event) => {
                            if let Err(e) = self.handle_transport_event(event).await {
                                warn!("transport event handler error: {e:#}");
                            }
                        }
                        Err(e) => warn!("transport event stream error: {e:#}"),
                    }
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    if let Err(e) = self.handle_command(cmd).await {
                        warn!("command handler error: {e:#}");
                    }
                }
                Some(remote_ids) = rendezvous_rx.recv() => {
                    self.dial_rendezvous_list(remote_ids).await;
                }
                _ = fast_tick.tick() => {
                    let now = Instant::now();
                    if now.duration_since(self.last_forward_at)
                        >= Duration::from_millis(self.effective_forwarding_interval_ms())
                    {
                        self.last_forward_at = now;
                        if let Err(e) = self.forward_once().await {
                            warn!("forward tick error: {e:#}");
                        }
                    }
                    if !self.crisis_active()
                        && now.duration_since(self.last_sync_at)
                            >= Duration::from_secs(self.effective_sync_interval_secs())
                    {
                        self.last_sync_at = now;
                        if let Err(e) = self.sync_known_peers().await {
                            warn!("sync tick error: {e:#}");
                        }
                    }
                }
                _ = gc_tick.tick() => {
                    match self.store.gc_expired().await {
                        Ok(deleted) if deleted > 0 => {
                            info!("GC: deleted {} expired messages", deleted);
                        }
                        Ok(_) => {}
                        Err(e) => warn!("GC tick error: {e:#}"),
                    }
                }
                _ = dial_tick.tick() => {
                    self.transport.redial_stored_targets();
                }
                _ = circuit_dial_tick.tick() => {
                    self.transport.try_deferred_circuit_dials();
                }
            }
        }
    }

    /// Dial peers returned by relay rendezvous (HTTP runs off the transport loop).
    /// Bucketed selective dial — contacts/QR first, then hash bucket (Phase D4).
    async fn dial_rendezvous_list(&mut self, remote_ids: Vec<String>) {
        if remote_ids.is_empty() {
            return;
        }
        let priority: Vec<String> =
            mycena_core::bootstrap::load_persisted_dial_peers(&self.db_path)
                .iter()
                .filter_map(|addr| mycena_core::bootstrap::peer_id_from_multiaddr(addr))
                .collect();
        let connected = self.transport.known_peers();
        let plan = mycena_core::rendezvous_dial::default_rendezvous_plan(
            &self.local_peer_id,
            &remote_ids,
            &priority,
            &connected,
            self.rendezvous_dial_bucket,
        );
        self.rendezvous_dial_bucket = self.rendezvous_dial_bucket.wrapping_add(1);
        if plan.to_dial.is_empty() {
            tracing::debug!(
                "rendezvous bucket {}/{}: {} listed, no dials this tick",
                plan.bucket,
                plan.num_buckets,
                remote_ids.len()
            );
            return;
        }
        tracing::info!(
            "rendezvous bucket {}/{}: dialing {} of {} listed peer(s)",
            plan.bucket,
            plan.num_buckets,
            plan.to_dial.len(),
            remote_ids.len()
        );
        for peer in plan.to_dial {
            let Some(circuit) = mycena_core::bootstrap::relay_circuit_multiaddr(&peer) else {
                continue;
            };
            if let Err(e) = self.transport.remember_and_dial(circuit).await {
                tracing::debug!("rendezvous dial {peer}: {e}");
            }
        }
    }

    async fn handle_command(&mut self, cmd: NodeCommand) -> anyhow::Result<()> {
        match cmd {
            NodeCommand::SendDirect { to_peer, body } => {
                self.send_direct_internal(
                    to_peer,
                    body.clone(),
                    body.into_bytes(),
                    Priority::Normal,
                    None,
                )
                .await?;
            }
            NodeCommand::SendDirectPayload {
                to_peer,
                body,
                payload,
                priority,
                app_chat_id,
            } => {
                self.send_direct_internal(
                    to_peer,
                    body,
                    payload,
                    priority.unwrap_or(Priority::Normal),
                    app_chat_id,
                )
                .await?;
            }
            NodeCommand::Broadcast { scope, body } => {
                self.broadcast_scoped(
                    scope,
                    body.clone(),
                    body.into_bytes(),
                    BroadcastPeerFilter::All,
                )
                .await?;
            }
            NodeCommand::BroadcastPayload {
                scope,
                body,
                payload,
                peer_filter,
            } => {
                self.broadcast_scoped(scope, body, payload, peer_filter)
                    .await?;
            }
            NodeCommand::SendWire {
                to_peer,
                mut message,
            } => {
                if !self.transport.is_peer_connected(&to_peer) {
                    return Err(crate::SendPathError::NotConnected {
                        to_peer: to_peer.clone(),
                    }
                    .into());
                }
                if let WireMessage::EncryptedDirect { .. } = &mut message {
                    match &self.keypair {
                        Some(keypair) => {
                            mycena_core::transport::sign_encrypted_direct(&mut message, keypair)?;
                        }
                        None => anyhow::bail!("cannot send EncryptedDirect without local keypair"),
                    }
                }
                self.transport.send_direct(to_peer, message).await?;
            }
            NodeCommand::SendWireOrQueueBle {
                to_peer,
                mut message,
            } => {
                if let WireMessage::EncryptedDirect { .. } = &mut message {
                    match &self.keypair {
                        Some(keypair) => {
                            mycena_core::transport::sign_encrypted_direct(&mut message, keypair)?;
                        }
                        None => anyhow::bail!("cannot send EncryptedDirect without local keypair"),
                    }
                }
                if self.transport.is_peer_connected(&to_peer) {
                    self.transport.send_direct(to_peer, message).await?;
                } else {
                    return Err(crate::SendPathError::NoDeliveryPath {
                        to_peer: to_peer.clone(),
                    }
                    .into());
                }
            }
            NodeCommand::EncryptE2ePayload {
                to_peer,
                wrapped,
                reply,
            } => {
                let legacy_pk = self.peer_x25519.read().await.get(&to_peer).copied();
                let result = self.encrypt_e2e_for_peer_sync(&to_peer, &wrapped, legacy_pk);
                if let Some(tx) = reply {
                    let _ = tx.send(result);
                }
            }
            NodeCommand::IngestBleFrame { from_peer, frame } => {
                if self.ingest_bearer_frame(&from_peer, &frame).await? {
                    return Ok(());
                }
            }
            NodeCommand::DrainBleOutbound {
                to_peer,
                max,
                reply,
            } => {
                let drained = self
                    .transport
                    .drain_bearer_side_outbound(to_peer.as_deref(), max);
                if let Some(tx) = reply {
                    let _ = tx.send(drained);
                }
            }
            NodeCommand::RegisterBleLink { peer_id, device_id } => {
                if self
                    .transport
                    .register_ble_link(peer_id.clone(), &device_id)
                    .is_some()
                {
                    for addr in self.transport.ble_peer_addrs() {
                        if addr.contains(&peer_id) {
                            let mut addrs = self.listen_addrs.write().await;
                            if !addrs.iter().any(|a| a == &addr) {
                                addrs.push(addr);
                            }
                        }
                    }
                    self.handle_transport_event(TransportEvent::PeerUp { peer_id })
                        .await?;
                }
            }
            NodeCommand::BleStreamInbound { from_peer, frame } => {
                self.transport.ingest_ble_stream_frame(&from_peer, &frame)?;
                for ev in self.transport.drain_pending_ble_events() {
                    self.handle_transport_event(ev).await?;
                }
            }
            NodeCommand::DrainBleStreamOutbound {
                to_peer,
                max,
                reply,
            } => {
                let drained = self.transport.drain_ble_stream_outbound(&to_peer, max);
                if let Some(tx) = reply {
                    let _ = tx.send(drained);
                }
            }
            NodeCommand::DrainBearerOutbound {
                to_peer,
                max,
                reply,
            } => {
                let drained = self
                    .transport
                    .drain_bearer_outbound(to_peer.as_deref(), max);
                if let Some(tx) = reply {
                    let _ = tx.send(drained);
                }
            }
            NodeCommand::QueueBearerBulk { to_peer, payload } => {
                if let Err(e) = self.transport.queue_bearer_bulk(to_peer, payload) {
                    warn!("queue bearer bulk: {e:#}");
                }
            }
            NodeCommand::ListPeers => {
                let peers = self.transport.known_peers();
                *self.peers.write().await = peers.clone();
                info!("known peers: {}", peers.len());
                for peer in peers {
                    info!("peer {}", peer);
                }
            }
            NodeCommand::ConnectedPeers { reply } => {
                let peers: Vec<String> = self
                    .transport
                    .known_peers()
                    .into_iter()
                    .filter(|p| self.transport.is_peer_connected(p))
                    .collect();
                if let Some(tx) = reply {
                    let _ = tx.send(peers);
                }
            }
            NodeCommand::AddBootstrapPeer { multiaddr } => {
                if let Err(e) = mycena_core::bootstrap::persist_dial_peer(&self.db_path, &multiaddr)
                {
                    warn!("could not persist dial peer: {e}");
                }
                self.transport.remember_and_dial(multiaddr).await?;
            }
            NodeCommand::SeedPeerEncKey {
                peer_id,
                enc_pubkey_hex,
            } => {
                let existing = self.peer_x25519.read().await.get(&peer_id).copied();
                match crypto::verify_enc_pubkey_update(existing.as_ref(), &enc_pubkey_hex) {
                    Ok(bytes) => {
                        persist_peer_enc_key(
                            self.peer_x25519.clone(),
                            self.peer_enc_store.clone(),
                            peer_id.clone(),
                            bytes,
                        )
                        .await;
                        let now = now_ms();
                        let _ = self.peer_trust.with_peer(&peer_id, now, |t| {
                            t.note_confirmed_contact(now);
                        });
                        info!("seeded enc key for {peer_id} from invite");
                        if self.transport.is_peer_connected(&peer_id) {
                            if let Err(e) = self.send_peer_info(peer_id.clone()).await {
                                warn!(
                                    "could not send PeerInfo after invite seed for {peer_id}: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!("invalid enc_pubkey_hex in invite for {peer_id}: {e}");
                    }
                }
            }
            NodeCommand::ForgetPeerEncKey { peer_id } => {
                self.peer_x25519.write().await.remove(&peer_id);
                if let Err(e) = self.peer_enc_store.remove(&peer_id) {
                    warn!("forget enc key for {peer_id}: {e}");
                }
            }
            NodeCommand::SeedPeerRatchetBundle { peer_id, bundle } => {
                use mycena_core::crypto::ratchet;
                if !ratchet::verify_prekey_bundle(&bundle) {
                    warn!("rejecting invalid ratchet bundle seed for {peer_id}");
                    return Ok(());
                }
                if let Some(enc) = self.peer_x25519.read().await.get(&peer_id).copied() {
                    if bundle.identity_key != enc {
                        warn!(
                            "rejecting ratchet bundle for {peer_id}: identity_key != seeded enc key"
                        );
                        return Ok(());
                    }
                }
                if let Err(e) = self.ratchet_store.save_peer_bundle(&peer_id, &bundle) {
                    warn!("failed to persist ratchet bundle for {peer_id}: {e}");
                } else {
                    info!("seeded ratchet bundle for {peer_id} from invite");
                }
            }
            NodeCommand::QueryPeerProtocol { peer, reply } => {
                let caps = self.negotiated_protocol_for(&peer);
                let _ = reply.send(caps);
            }
            NodeCommand::BuildSignedInviteV3 {
                peer_id,
                direct_multiaddr,
                reply,
            } => {
                // POISON-RECOVER: reads prekey bundle snapshot; invite encode is atomic.
                let bundle = lock_or_recover!(self.local_prekeys, "local prekeys")
                    .public_bundle(&self.enc_keypair);
                let result = mycena_core::invite::encode_signed_invite_v3(
                    &self.account_id,
                    &peer_id,
                    &self.enc_keypair.public_hex(),
                    self.transport_epoch,
                    self.account_signing_key.as_ref(),
                    direct_multiaddr.as_deref(),
                    Some(&bundle),
                );
                let _ = reply.send(result);
            }
            NodeCommand::SubscribeScope(scope) => {
                self.subscribed_scopes.write().await.insert(scope.clone());
                if !scope.contains('*') {
                    if let Err(e) = self.transport.subscribe_scope(scope).await {
                        warn!("failed to subscribe to gossip topic: {e}");
                    }
                }
            }
            NodeCommand::UnsubscribeScope(scope) => {
                self.subscribed_scopes.write().await.remove(&scope);
                if !scope.contains('*') {
                    if let Err(e) = self.transport.unsubscribe_scope(scope).await {
                        warn!("failed to unsubscribe from gossip topic: {e}");
                    }
                }
            }
            NodeCommand::SubscribeBulletinScope(scope) => {
                let scope = scope.trim().to_string();
                if scope.is_empty() {
                    return Ok(());
                }
                {
                    let mut subs = self.bulletin_subscriptions.write().await;
                    if !subs.iter().any(|s| s == &scope) {
                        subs.push(scope.clone());
                    }
                }
                self.subscribed_scopes.write().await.insert(scope.clone());
                if !scope.contains('*') {
                    if let Err(e) = self.subscribe_gossip_for_scope(&scope).await {
                        warn!("failed to subscribe to bulletin scope: {e}");
                    }
                }
                self.announce_subscribed_scopes_to_peers().await?;
            }
            NodeCommand::UnsubscribeBulletinScope(scope) => {
                self.bulletin_subscriptions
                    .write()
                    .await
                    .retain(|s| s != &scope);
                self.subscribed_scopes.write().await.remove(&scope);
                if !scope.contains('*') {
                    if let Err(e) = self.transport.unsubscribe_scope(scope).await {
                        warn!("failed to unsubscribe from bulletin scope: {e}");
                    }
                }
            }
            NodeCommand::GcNow { reply } => {
                let deleted = self.store.gc_expired().await?;
                info!("manual GC deleted {} messages", deleted);
                if let Some(tx) = reply {
                    let _ = tx.send(deleted);
                }
            }
            NodeCommand::StoreStats { reply } => {
                let stats = self.store.stats().await?;
                info!(
                    "store stats: count={} oldest_ms={}",
                    stats.count, stats.oldest_ms
                );
                if let Some(tx) = reply {
                    let _ = tx.send(stats);
                }
            }
            NodeCommand::SetEnergyState(state) => {
                self.node_state = state;
                info!("energy state updated to {:?}", self.node_state);
                self.sync_energy_profile_to_transport();
            }
            NodeCommand::SetThermalState(thermal) => {
                self.thermal_state = thermal;
                info!("thermal state updated to {:?}", self.thermal_state);
                self.sync_energy_profile_to_transport();
            }
            NodeCommand::QueryEnergyProfile { reply } => {
                let _ = reply.send(self.energy_profile());
            }
            NodeCommand::SetWifiAwareAvailable(available) => {
                self.wifi_aware.set_available(available);
                info!("Wi-Fi Aware discovery available={available}");
            }
            NodeCommand::SetBleDiscoveryActive(active) => {
                self.ble_discovery_active = active;
            }
            NodeCommand::QueueDiscoveryAnnouncement { invite_uri } => {
                self.wifi_aware.queue_announcement(invite_uri);
            }
            NodeCommand::IngestWifiAwareDiscovery {
                peer_hint,
                invite_uri,
                latency_ms,
            } => {
                self.wifi_aware
                    .ingest_peer(peer_hint, invite_uri, latency_ms);
            }
            NodeCommand::RecordBleDiscoveryObservation {
                latency_ms,
                bytes,
                success,
            } => {
                self.wifi_aware
                    .record_ble_discovery_observation(latency_ms, bytes, success);
            }
            NodeCommand::QueryPreferWifiAwareDiscovery { reply } => {
                let now = now_ms();
                let prefer = prefer_wifi_aware_for_discovery(
                    self.wifi_aware.is_available(),
                    self.ble_discovery_active,
                    self.wifi_aware.transport_scores(),
                    now,
                );
                let _ = reply.send(prefer);
            }
            NodeCommand::DrainDiscoveryAnnouncements { max, reply } => {
                let out: Vec<String> = self
                    .wifi_aware
                    .drain_announcements(max)
                    .into_iter()
                    .map(|a| a.invite_uri)
                    .collect();
                let _ = reply.send(out);
            }
            NodeCommand::SetRendezvousEnabled(enabled) => {
                self.rendezvous_enabled.store(enabled, Ordering::Relaxed);
                let kp = self.keypair.clone();
                tokio::task::spawn_blocking(move || {
                    if let Some(ref key) = kp {
                        mycena_core::rendezvous::set_relay_rendezvous_registration(
                            key, enabled, None,
                        );
                    }
                });
                info!(
                    "rendezvous discovery {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }
            NodeCommand::SetInternetEnabled(enabled) => {
                self.internet_enabled.store(enabled, Ordering::Relaxed);
                if !enabled {
                    self.rendezvous_enabled.store(false, Ordering::Relaxed);
                    let kp = self.keypair.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Some(ref key) = kp {
                            mycena_core::rendezvous::set_relay_rendezvous_registration(
                                key, false, None,
                            );
                        }
                    });
                }
                if let Some(tx) = &self.connectivity_mode_tx {
                    let mode = if enabled {
                        ConnectivityMode::Internet
                    } else {
                        ConnectivityMode::MeshOnly
                    };
                    let _ = tx.send(mode);
                }
                info!(
                    "internet connectivity {}",
                    if enabled {
                        "enabled (relay + mesh)"
                    } else {
                        "disabled — local mesh only"
                    }
                );
            }
            NodeCommand::SetCrowdMode(enabled) => {
                self.crowd_mode.store(enabled, Ordering::Relaxed);
                info!(
                    "crowd mode {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }
            NodeCommand::SetPrivacyShaping(config) => {
                let cover = config.cover_traffic_enabled;
                self.privacy_shaping = config;
                self.batch_scheduler = BatchingScheduler::default();
                self.last_cover_at = None;
                info!(
                    "privacy shaping {} (cover={cover})",
                    if self.privacy_shaping.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
            }
            NodeCommand::SetLocalEndorsements(map) => {
                *self.local_endorsements.write().await = map;
            }
            NodeCommand::SetCrisisManual(manual) => {
                self.crisis_state.manual = manual;
                let active = self.crisis_state.is_active();
                info!("crisis mode manual={manual:?} active={active}");
                self.metrics.write().await.crisis_active = active;
            }
            NodeCommand::UpdateCrisisAuto {
                internet_reachable,
                listen_sos,
                local_geohash_prefix,
            } => {
                if local_geohash_prefix.is_some() {
                    self.apply_local_geohash_prefix(local_geohash_prefix.clone())
                        .await;
                }
                let trusted_nearby = self
                    .local_geohash_prefix
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|prefix| {
                        self.sos_crisis_tracker
                            .trusted_independent_sources_nearby(prefix, now_ms())
                    })
                    .unwrap_or(0);
                self.crisis_state.auto_active = mycena_core::crisis_mode::evaluate_auto_activation(
                    &self.crisis_auto_config,
                    internet_reachable,
                    listen_sos,
                    trusted_nearby,
                );
                let active = self.crisis_state.is_active();
                self.metrics.write().await.crisis_active = active;
            }
            NodeCommand::SetLocalGeohashPrefix(prefix) => {
                self.apply_local_geohash_prefix(prefix).await;
            }
            NodeCommand::RemapTransportPeer {
                old_peer_id,
                new_peer_id,
            } => {
                if let Err(e) = self.remap_transport_peer(&old_peer_id, &new_peer_id) {
                    warn!("remap transport peer {old_peer_id} -> {new_peer_id}: {e:#}");
                }
            }
            NodeCommand::RecordFactObservation {
                relay_peer,
                kind,
                geohash,
                category,
                content,
                observed_at_ms,
            } => {
                let trust = self.peer_trust.get(&relay_peer, observed_at_ms);
                match kind {
                    mycena_core::information_trust::FactKind::Resource => {
                        if let Some(category) = category {
                            let entry = mycena_core::resource_map::ResourceMapEntry {
                                geohash,
                                category,
                                description: content,
                                created_at_ms: observed_at_ms,
                                ttl_secs: mycena_core::resource_map::RESOURCE_DEFAULT_TTL_SECS,
                            };
                            self.information_trust_tracker.record_resource(
                                &relay_peer,
                                &entry,
                                &trust,
                                observed_at_ms,
                            );
                        }
                    }
                    mycena_core::information_trust::FactKind::Sos => {
                        let payload = mycena_core::sos::SosPayload {
                            geohash,
                            message: content,
                            created_at_ms: observed_at_ms,
                        };
                        self.information_trust_tracker.record_sos(
                            &relay_peer,
                            &payload,
                            &trust,
                            observed_at_ms,
                        );
                    }
                }
            }
            NodeCommand::QueryFactTrust {
                kind,
                geohash,
                category,
                content,
                ttl_secs,
                reply,
            } => {
                let now = now_ms();
                let trust = match kind {
                    mycena_core::information_trust::FactKind::Resource => {
                        if let Some(category) = category {
                            let entry = mycena_core::resource_map::ResourceMapEntry {
                                geohash,
                                category,
                                description: content,
                                created_at_ms: now,
                                ttl_secs,
                            };
                            self.information_trust_tracker.query_resource_trust(
                                &entry,
                                now,
                                &self.peer_trust.load_all(),
                            )
                        } else {
                            mycena_core::information_trust::InformationTrust {
                                corroboration_count: 0,
                                trusted_corroboration_count: 0,
                                source_diversity: 0.0,
                                geographic_consistency: 1.0,
                                staleness_factor: 0.0,
                                trust_score: 0.0,
                                status:
                                    mycena_core::information_trust::CorroborationStatus::Unverified,
                            }
                        }
                    }
                    mycena_core::information_trust::FactKind::Sos => {
                        let payload = mycena_core::sos::SosPayload {
                            geohash,
                            message: content,
                            created_at_ms: now,
                        };
                        self.information_trust_tracker.query_sos_trust(
                            &payload,
                            now,
                            &self.peer_trust.load_all(),
                        )
                    }
                };
                let _ = reply.send(trust);
            }
            NodeCommand::QueryPendingCustody { reply } => {
                let now = now_ms();
                let connected: Vec<String> = self
                    .transport
                    .known_peers()
                    .into_iter()
                    .filter(|p| self.transport.is_peer_connected(p))
                    .collect();
                let statuses = self
                    .custody_queue
                    .list_pending_statuses(
                        &self.local_peer_id,
                        &connected,
                        &self.encounter_log,
                        now,
                    )
                    .unwrap_or_default();
                let _ = reply.send(statuses);
            }
            NodeCommand::QueryConnectivityMode { reply } => {
                let _ = reply.send(self.transport.connectivity_mode());
            }
            NodeCommand::SendEncryptedDirect {
                to_peer,
                inner_plaintext,
                priority,
            } => {
                self.send_encrypted_direct_internal(&to_peer, &inner_plaintext, priority)
                    .await?;
            }
            NodeCommand::EnqueueEncryptedDirectCustody {
                to_peer,
                inner_plaintext,
                priority,
                app_chat_id,
                display_body,
            } => {
                self.enqueue_encrypted_outbound_custody(
                    &to_peer,
                    &inner_plaintext,
                    priority,
                    app_chat_id,
                    &display_body,
                )
                .await?;
            }
            NodeCommand::SetWebRtcAvailable(available) => {
                self.transport.set_webrtc_host_available(available);
            }
            NodeCommand::SetWebRtcLinkReady { peer, ready } => {
                self.transport.set_webrtc_link_ready(&peer, ready);
            }
            NodeCommand::IngestWebRtcFrame { from_peer, frame } => {
                let _ = self.ingest_bearer_frame(&from_peer, &frame).await;
            }
            NodeCommand::PostWebRtcSignal {
                to_peer,
                session_id,
                kind,
                payload,
                ice_mline_index,
            } => {
                if payload.len() > MAX_SIGNALING_PAYLOAD {
                    warn!("webrtc signal to {to_peer} exceeds max payload");
                } else if WebRtcSignalKind::from_u8(kind).is_some() {
                    let wire = WireMessage::WebRtcSignal {
                        to_peer: to_peer.clone(),
                        session_id,
                        kind,
                        payload,
                        ice_mline_index,
                    };
                    if let Err(e) = self.transport.send_direct(to_peer, wire).await {
                        warn!("post webrtc signal: {e:#}");
                    }
                }
            }
            NodeCommand::DrainWebRtcSignalingInbound { max, reply } => {
                let drained = self.transport.drain_webrtc_inbound_signals(max);
                let _ = reply.send(drained);
            }
            NodeCommand::DrainWebRtcNegotiationRequests { max, reply } => {
                let drained = self.transport.drain_webrtc_negotiation_requests(max);
                let _ = reply.send(drained);
            }
        }
        Ok(())
    }

    async fn ingest_bearer_frame(&mut self, from_peer: &str, frame: &[u8]) -> anyhow::Result<bool> {
        use crate::bearer_ble::{BearerBleRouter, BearerIngestAction};
        match BearerBleRouter::decode_ingest(frame) {
            Ok(BearerIngestAction::StreamFrame(payload)) => {
                let encoded = mycena_core::ble_stream::encode_stream_frame(&payload)?;
                self.transport
                    .ingest_ble_stream_frame(from_peer, &encoded)?;
                for ev in self.transport.drain_pending_ble_events() {
                    self.handle_transport_event(ev).await?;
                }
                Ok(true)
            }
            Ok(BearerIngestAction::Wire(wire)) => {
                self.handle_incoming_direct(from_peer.to_string(), wire)
                    .await?;
                Ok(true)
            }
            Err(e) => {
                warn!("bearer frame decode from {from_peer}: {e:#}");
                Ok(false)
            }
        }
    }

    fn queue_ble_wire(&mut self, to_peer: String, wire: WireMessage) {
        match self.transport.queue_bearer_side_wire(to_peer.clone(), wire) {
            Ok(()) => info!("queued bearer side frame for {to_peer}"),
            Err(e) => warn!("bearer side queue for {to_peer}: {e:#}"),
        }
    }

    fn sign_envelope(&self, envelope: &mut mycena_core::data::Envelope) -> anyhow::Result<()> {
        let Some(keypair) = &self.keypair else {
            anyhow::bail!("cannot sign envelope without local keypair");
        };
        envelope.sign(keypair)
    }

    async fn send_direct_internal(
        &mut self,
        to_peer: String,
        body: String,
        payload: Vec<u8>,
        priority: Priority,
        app_chat_id: Option<String>,
    ) -> anyhow::Result<()> {
        let mut envelope = mycena_core::data::Envelope::new(
            self.local_peer_id.clone(),
            Some(to_peer.clone()),
            payload,
        );
        envelope.priority = priority;
        self.sign_envelope(&mut envelope)?;
        let msg = DirectMessage {
            envelope,
            body: body.clone(),
        };
        let wire = WireMessage::Data(msg.clone());
        let _ = self
            .ingest
            .ingest(&mut self.seen_cache, &msg, false)
            .await?;
        let peers = self.transport.known_peers();
        let now = now_ms();
        let payload_len = msg.envelope.payload.len().saturating_add(body.len());
        let wifi_cap = self
            .peer_protocol_caps
            .get(&to_peer)
            .is_some_and(|n| n.wifi_direct_allowed());
        let availability =
            self.transport
                .transport_availability_for_peer(&to_peer, wifi_cap, !peers.is_empty());
        let path = self
            .transport
            .decide_outbound_path(&availability, payload_len, false, now);

        match path {
            TransportPath::CustodyQueue => {
                debug!("no live bearer toward {to_peer}, relying on custody queue");
            }
            TransportPath::InternetRelay => {
                self.forward_direct_internet(&to_peer, &msg, &wire, &peers)
                    .await?;
            }
            TransportPath::WebRtc => {
                self.forward_direct_webrtc(&to_peer, &wire).await?;
            }
            TransportPath::WifiDirect | TransportPath::BleTrickle => {
                self.forward_direct_mesh_bearer(&to_peer, &msg, &wire, &peers)
                    .await?;
            }
        }
        if self.custody_enabled
            && !self.transport.is_peer_connected(&to_peer)
            && self.peer_custody_allowed(&to_peer)
        {
            self.enqueue_outbound_custody(&to_peer, &msg, &body, app_chat_id)
                .await?;
        }
        Ok(())
    }

    async fn forward_direct_webrtc(
        &mut self,
        to_peer: &str,
        wire: &WireMessage,
    ) -> anyhow::Result<()> {
        if let Err(e) = self.transport.queue_webrtc_wire(to_peer, wire.clone()) {
            warn!("webrtc queue for {to_peer}: {e:#}");
        } else {
            info!("direct message queued for {to_peer} (WebRTC data channel)");
        }
        Ok(())
    }

    async fn forward_direct_internet(
        &mut self,
        to_peer: &str,
        msg: &DirectMessage,
        wire: &WireMessage,
        peers: &[String],
    ) -> anyhow::Result<()> {
        if peers.iter().any(|p| p == to_peer) {
            self.enqueue(to_peer.to_string(), msg.clone()).await?;
            info!("direct message queued for {to_peer} (internet path)");
            return Ok(());
        }
        if peers.is_empty() {
            warn!("internet path unavailable: no relay candidates for {to_peer}");
            return Ok(());
        }
        let now = now_ms();
        let peer_trust = self.peer_trust.load_all();
        let endorsement_boosts = self.local_endorsements.read().await.clone();
        let routing_meta = self.build_routing_meta(peers, &peer_trust)?;
        let diversity = self.relay_diversity_config_for(to_peer);
        let relay_candidates =
            security::select_relay_candidates_with_diversity(&security::RelaySelectionRequest {
                peers,
                to_peer,
                peer_trust: &peer_trust,
                endorsement_boost: Some(&endorsement_boosts),
                now_ms: now,
                max_fanout: self.effective_max_relay_fanout(),
                routing_meta: Some(&routing_meta),
                diversity: Some(&diversity),
            });
        if relay_candidates.is_empty() {
            warn!("SD-031: no suitable relay candidates for {to_peer}");
        } else {
            info!(
                "SD-031: relaying to {} candidate(s) toward {to_peer} (internet path)",
                relay_candidates.len()
            );
            for peer in relay_candidates {
                self.enqueue(peer, msg.clone()).await?;
            }
        }
        info!("message queued for relay toward {to_peer}");
        let _ = wire;
        Ok(())
    }

    async fn forward_direct_mesh_bearer(
        &mut self,
        to_peer: &str,
        msg: &DirectMessage,
        wire: &WireMessage,
        peers: &[String],
    ) -> anyhow::Result<()> {
        if peers.iter().any(|p| p == to_peer) {
            self.enqueue(to_peer.to_string(), msg.clone()).await?;
            info!("direct message queued for {to_peer} (mesh bearer)");
            return Ok(());
        }
        if peers.is_empty() {
            warn!("no mesh peers available for {to_peer}");
            self.queue_ble_wire(to_peer.to_string(), wire.clone());
            return Ok(());
        }
        let now = now_ms();
        let peer_trust = self.peer_trust.load_all();
        let endorsement_boosts = self.local_endorsements.read().await.clone();
        let routing_meta = self.build_routing_meta(peers, &peer_trust)?;
        let diversity = self.relay_diversity_config_for(to_peer);
        let relay_candidates =
            security::select_relay_candidates_with_diversity(&security::RelaySelectionRequest {
                peers,
                to_peer,
                peer_trust: &peer_trust,
                endorsement_boost: Some(&endorsement_boosts),
                now_ms: now,
                max_fanout: self.effective_max_relay_fanout(),
                routing_meta: Some(&routing_meta),
                diversity: Some(&diversity),
            });
        if relay_candidates.is_empty() {
            warn!("SD-031: no suitable relay candidates for {to_peer}");
        } else {
            for peer in relay_candidates {
                self.enqueue(peer, msg.clone()).await?;
            }
        }
        if !self.transport.is_peer_connected(to_peer) {
            self.queue_ble_wire(to_peer.to_string(), wire.clone());
        }
        Ok(())
    }

    async fn enqueue_outbound_custody(
        &mut self,
        to_peer: &str,
        msg: &DirectMessage,
        body: &str,
        app_chat_id: Option<String>,
    ) -> anyhow::Result<()> {
        if !self.custody_origin_allowed(to_peer) {
            return Ok(());
        }
        let mut envelope = msg.envelope.clone();
        envelope.custody = true;
        envelope.max_hops = mycena_core::data::CUSTODY_DEFAULT_MAX_HOPS;
        envelope.ttl_secs = 48 * 60 * 60;
        self.sign_envelope(&mut envelope)?;
        let custody_msg = DirectMessage {
            envelope,
            body: body.to_string(),
        };
        if self
            .custody_queue
            .enqueue_with_meta(&custody_msg, app_chat_id, None)?
        {
            debug!(
                "custody enqueued outbound message_id={} target={to_peer}",
                custody_msg.envelope.id.0
            );
        }
        Ok(())
    }

    async fn enqueue_encrypted_outbound_custody(
        &mut self,
        to_peer: &str,
        inner_plaintext: &[u8],
        priority: Priority,
        app_chat_id: Option<String>,
        display_body: &str,
    ) -> anyhow::Result<()> {
        if !self.custody_enabled || !self.peer_custody_allowed(to_peer) {
            return Ok(());
        }
        if !self.custody_origin_allowed(to_peer) {
            return Ok(());
        }
        let wrapped =
            mycena_core::e2e_direct_wrap::wrap_inner(&self.local_peer_id, inner_plaintext);
        let legacy_pk = self.peer_x25519.read().await.get(to_peer).copied();
        let (encrypted_payload, e2e_version) =
            self.encrypt_e2e_for_peer_sync(to_peer, &wrapped, legacy_pk)?;
        let sender_enc_pubkey = self.enc_keypair.public_hex();
        let mut wire = WireMessage::EncryptedDirect {
            to_peer: to_peer.to_string(),
            sender_enc_pubkey,
            encrypted_payload,
            mesh_signature: None,
            hop_count: 0,
            max_hops: 8,
            e2e_version,
            priority,
        };
        if let Some(keypair) = &self.keypair {
            mycena_core::transport::sign_encrypted_direct(&mut wire, keypair)?;
        } else {
            anyhow::bail!("cannot enqueue EncryptedDirect custody without local keypair");
        }
        let mut envelope = mycena_core::data::Envelope::new(
            self.local_peer_id.clone(),
            Some(to_peer.to_string()),
            Vec::new(),
        );
        envelope.custody = true;
        envelope.max_hops = mycena_core::data::CUSTODY_DEFAULT_MAX_HOPS;
        envelope.ttl_secs = 48 * 60 * 60;
        envelope.priority = priority;
        self.sign_envelope(&mut envelope)?;
        let custody_msg = DirectMessage {
            envelope,
            body: display_body.to_string(),
        };
        if self
            .custody_queue
            .enqueue_with_meta(&custody_msg, app_chat_id, Some(wire))?
        {
            debug!(
                "custody enqueued encrypted outbound message_id={} target={to_peer}",
                custody_msg.envelope.id.0
            );
        }
        Ok(())
    }

    async fn forward_custody_plan(&mut self, plan: CustodyForwardPlan) -> anyhow::Result<()> {
        if let Some(wire) = plan.wire {
            self.transport
                .send_direct(plan.to_peer.clone(), wire)
                .await?;
        } else {
            self.enqueue(plan.to_peer, plan.message).await?;
        }
        Ok(())
    }

    fn custody_origin_allowed(&self, to_peer: &str) -> bool {
        if self
            .peer_x25519
            .try_read()
            .ok()
            .is_some_and(|m| m.contains_key(to_peer))
        {
            return true;
        }
        self.peer_trust
            .load_all()
            .get(to_peer)
            .is_some_and(|t| t.mutual_contacts > 0)
    }

    fn identity_rotation_recipients(&self) -> Vec<String> {
        let mut targets = HashSet::new();
        if let Ok(map) = self.peer_x25519.try_read() {
            targets.extend(map.keys().cloned());
        }
        if let Ok(ids) = self.ratchet_store.session_peer_ids() {
            targets.extend(ids);
        }
        for (peer, trust) in self.peer_trust.load_all() {
            if trust.mutual_contacts > 0 {
                targets.insert(peer);
            }
        }
        targets.retain(|p| p != &self.local_peer_id);
        targets.into_iter().collect()
    }

    /// Signed re-announce to contacts over E2E direct (opaque to LAN/relay observers).
    /// Returns peer ids that still need delivery (offline / send failure).
    async fn announce_identity_rotation_e2e(
        &mut self,
        rot: mycena_core::identity_rotation::IdentityRotationGossip,
    ) -> Vec<String> {
        let inner = match rot.encode_e2e_announce() {
            Ok(v) => v,
            Err(e) => {
                warn!("encode identity rotation E2E announce: {e:#}");
                return Vec::new();
            }
        };
        let recipients = if self.identity_rotation_pending_peers.is_empty() {
            self.identity_rotation_recipients()
        } else {
            self.identity_rotation_pending_peers
                .iter()
                .cloned()
                .collect()
        };
        if recipients.is_empty() {
            info!(
                "transport rotated to {} — no E2E contacts to announce (epoch {})",
                rot.new_peer_id, rot.epoch
            );
            return Vec::new();
        }
        let mut sent = 0usize;
        let mut failed = Vec::new();
        for to_peer in recipients {
            match self
                .send_encrypted_direct_internal(&to_peer, &inner, Priority::Normal)
                .await
            {
                Ok(()) => sent += 1,
                Err(e) => {
                    warn!("identity rotation E2E to {to_peer}: {e:#}");
                    failed.push(to_peer);
                }
            }
        }
        info!(
            "transport identity rotation epoch={} announced to {sent} contact(s) via E2E ({} pending)",
            rot.epoch,
            failed.len()
        );
        failed
    }

    async fn retry_identity_rotation_to_peer(&mut self, peer_id: &str) {
        if !self.identity_rotation_pending_peers.contains(peer_id) {
            return;
        }
        let Some(rot) = self.pending_identity_rotation.clone() else {
            self.identity_rotation_pending_peers.remove(peer_id);
            return;
        };
        let inner = match rot.encode_e2e_announce() {
            Ok(v) => v,
            Err(e) => {
                warn!("encode identity rotation retry: {e:#}");
                return;
            }
        };
        match self
            .send_encrypted_direct_internal(peer_id, &inner, Priority::Normal)
            .await
        {
            Ok(()) => {
                self.identity_rotation_pending_peers.remove(peer_id);
                if self.identity_rotation_pending_peers.is_empty() {
                    self.pending_identity_rotation = None;
                    info!(
                        "transport identity rotation epoch={} fully delivered",
                        rot.epoch
                    );
                }
            }
            Err(e) => warn!("identity rotation retry to {peer_id}: {e:#}"),
        }
    }

    async fn retry_pending_identity_rotations(&mut self) {
        if self.identity_rotation_pending_peers.is_empty() {
            return;
        }
        let pending: Vec<String> = self
            .identity_rotation_pending_peers
            .iter()
            .cloned()
            .collect();
        for peer in pending {
            if self.transport.is_peer_connected(&peer) {
                self.retry_identity_rotation_to_peer(&peer).await;
            }
        }
    }

    async fn send_encrypted_direct_internal(
        &mut self,
        to_peer: &str,
        inner_plaintext: &[u8],
        priority: Priority,
    ) -> anyhow::Result<()> {
        let wrapped =
            mycena_core::e2e_direct_wrap::wrap_inner(&self.local_peer_id, inner_plaintext);
        let legacy_pk = self.peer_x25519.read().await.get(to_peer).copied();
        let (encrypted_payload, e2e_version) =
            self.encrypt_e2e_for_peer_sync(to_peer, &wrapped, legacy_pk)?;
        let sender_enc_pubkey = self.enc_keypair.public_hex();
        let mut wire = WireMessage::EncryptedDirect {
            to_peer: to_peer.to_string(),
            sender_enc_pubkey,
            encrypted_payload,
            mesh_signature: None,
            hop_count: 0,
            max_hops: 8,
            e2e_version,
            priority,
        };
        if let Some(keypair) = &self.keypair {
            mycena_core::transport::sign_encrypted_direct(&mut wire, keypair)?;
        } else {
            anyhow::bail!("cannot send EncryptedDirect without local keypair");
        }
        if self.transport.is_peer_connected(to_peer) {
            self.transport
                .send_direct(to_peer.to_string(), wire)
                .await?;
        } else {
            let peers = self.transport.known_peers();
            let now = now_ms();
            let peer_trust = self.peer_trust.load_all();
            let endorsement_boosts = self.local_endorsements.read().await.clone();
            let routing_meta = self.build_routing_meta(&peers, &peer_trust)?;
            let diversity = self.relay_diversity_config_for(to_peer);
            let relay_candidates = security::select_relay_candidates_with_diversity(
                &security::RelaySelectionRequest {
                    peers: &peers,
                    to_peer,
                    peer_trust: &peer_trust,
                    endorsement_boost: Some(&endorsement_boosts),
                    now_ms: now,
                    max_fanout: self.effective_max_relay_fanout(),
                    routing_meta: Some(&routing_meta),
                    diversity: Some(&diversity),
                },
            );
            if relay_candidates.is_empty() {
                return Err(crate::SendPathError::NoDeliveryPath {
                    to_peer: to_peer.to_string(),
                }
                .into());
            }
            for relay in &relay_candidates {
                self.transport
                    .send_direct(relay.clone(), wire.clone())
                    .await?;
            }
        }
        Ok(())
    }

    /// Remap node-side peer state after a contact rotates transport PeerId.
    fn remap_transport_peer(&mut self, old_peer_id: &str, new_peer_id: &str) -> anyhow::Result<()> {
        if old_peer_id.is_empty() || new_peer_id.is_empty() || old_peer_id == new_peer_id {
            return Ok(());
        }
        if let Ok(mut map) = self.peer_x25519.try_write() {
            if let Some(key) = map.remove(old_peer_id) {
                map.insert(new_peer_id.to_string(), key);
                if let Err(e) = self.peer_enc_store.save(new_peer_id, &key) {
                    warn!("persist remapped enc key: {e}");
                }
            }
        }
        if let Err(e) = self.ratchet_store.remap_peer_id(old_peer_id, new_peer_id) {
            warn!("ratchet remap {old_peer_id} -> {new_peer_id}: {e}");
        }
        if let Some(scopes) = self.peer_scopes.remove(old_peer_id) {
            self.peer_scopes.insert(new_peer_id.to_string(), scopes);
        }
        if let Some(caps) = self.peer_protocol_caps.remove(old_peer_id) {
            self.peer_protocol_caps
                .insert(new_peer_id.to_string(), caps);
        }
        let now = now_ms();
        if let Some(trust) = self.peer_trust.get_cached(old_peer_id) {
            let _ = self.peer_trust.with_peer(new_peer_id, now, |t| {
                *t = trust.clone();
            });
        }
        Ok(())
    }

    fn try_record_fact_at_ingest(
        &mut self,
        from_peer: &str,
        scope: &str,
        payload: &[u8],
        now_ms: u64,
    ) {
        let Some(claim) =
            mycena_core::fact_claim::extract_fact_claim_from_gossip_payload(scope, payload)
        else {
            return;
        };
        let trust = self.peer_trust.get(from_peer, now_ms);
        match claim {
            mycena_core::fact_claim::ExtractedFactClaim::Sos(payload) => {
                self.information_trust_tracker
                    .record_sos(from_peer, &payload, &trust, now_ms);
            }
            mycena_core::fact_claim::ExtractedFactClaim::Resource(entry) => {
                if !entry.is_expired(now_ms) {
                    self.information_trust_tracker
                        .record_resource(from_peer, &entry, &trust, now_ms);
                }
            }
        }
    }

    async fn broadcast_scoped(
        &mut self,
        scope: String,
        body: String,
        payload: Vec<u8>,
        peer_filter: BroadcastPeerFilter,
    ) -> anyhow::Result<()> {
        let local_peer_id = self.local_peer_id.clone();
        if crate::ingest_guard::is_sos_scope(&scope) {
            if let Some(gh) = mycena_core::sos::geohash_from_payload_bytes(&payload) {
                let now = now_ms();
                let trust = self
                    .peer_trust
                    .get_cached(&local_peer_id)
                    .unwrap_or_else(|| PeerTrust::new_peer(now));
                self.sos_crisis_tracker
                    .record_observation(&local_peer_id, &gh, &trust, now);
            }
        }
        if crate::ingest_guard::is_sos_scope(&scope)
            || scope.starts_with(mycena_core::resource_map::RESOURCE_MAP_SCOPE)
        {
            self.try_record_fact_at_ingest(&local_peer_id, &scope, &payload, now_ms());
        }
        if self.crisis_active() && mycena_core::resource_map::should_defer_gossip_in_crisis(&scope)
        {
            self.metrics.write().await.crisis_dropped_background_gossip += 1;
            return Ok(());
        }
        let mut targets: HashSet<String> = self.targets_for_scope(&scope).into_iter().collect();
        // Bulletin posts must reach every connected peer — ScopeAnnounce can be
        // missed when peer_cap has not recorded the sender yet (asymmetric LAN sync).
        if body.starts_with("[bulletin:") {
            if let Ok(peers) = self.peers.try_read() {
                for peer in peers.iter() {
                    if !mycena_core::bootstrap::is_relay_peer(peer) {
                        targets.insert(peer.clone());
                    }
                }
            }
        }
        for to_peer in targets {
            if !self.peer_allowed_for_broadcast(&to_peer, peer_filter) {
                continue;
            }
            let mut envelope =
                mycena_core::data::Envelope::new(self.local_peer_id.clone(), None, payload.clone());
            let essential = mycena_core::resource_map::is_essential_gossip_scope(&scope);
            if essential {
                envelope.priority = Priority::High;
                if self.crisis_active() {
                    envelope.max_hops = self.crisis_params.emergency_max_hops;
                }
            }
            if let Some(keypair) = &self.keypair {
                envelope.sign(keypair)?;
            } else {
                anyhow::bail!("cannot sign broadcast envelope without local keypair");
            }
            let msg = DirectMessage {
                envelope,
                body: body.clone(),
            };
            match self.ingest.ingest(&mut self.seen_cache, &msg, false).await {
                Ok(true) => {
                    if let Err(e) = self.enqueue(to_peer.clone(), msg).await {
                        warn!("broadcast enqueue to {to_peer}: {e:#}");
                    }
                }
                Ok(false) => {}
                Err(e) => warn!("broadcast ingest for {scope}: {e:#}"),
            }
        }
        // Gossip publish on geohash-sharded topics when applicable (AR-06).
        let gossip_topics = topic_shard::resolve_publish_topics_with_hint(
            &scope,
            &payload,
            self.local_geohash_prefix.as_deref(),
        );
        for topic in gossip_topics {
            if let Err(e) = self.transport.publish_scoped(topic, payload.clone()).await {
                warn!("gossip publish for bulletin scope (non-fatal): {e:#}");
            }
        }
        Ok(())
    }

    async fn announce_subscribed_scopes_to_peers(&mut self) -> anyhow::Result<()> {
        let scopes = Self::bucketed_scopes(
            self.subscribed_scopes
                .read()
                .await
                .iter()
                .cloned()
                .collect(),
            &self.privacy_shaping,
        );
        if scopes.is_empty() {
            return Ok(());
        }
        let peers: Vec<String> = self
            .transport
            .known_peers()
            .into_iter()
            .filter(|p| !mycena_core::bootstrap::is_relay_peer(p))
            .collect();
        for peer in peers {
            if let Err(err) = self
                .transport
                .send_direct(
                    peer.clone(),
                    WireMessage::ScopeAnnounce {
                        scopes: scopes.clone(),
                    },
                )
                .await
            {
                warn!("scope announce to {peer} failed: {err}");
            }
        }
        Ok(())
    }

    async fn handle_transport_event(&mut self, event: TransportEvent) -> anyhow::Result<()> {
        match event {
            TransportEvent::Listening { address } => {
                info!("listening on {address}");
                {
                    let mut addrs = self.listen_addrs.write().await;
                    if !addrs.iter().any(|a| a == &address) {
                        addrs.push(address);
                    }
                }
            }
            TransportEvent::PeerUp { peer_id } => {
                if mycena_core::bootstrap::is_relay_peer(&peer_id) {
                    return Ok(());
                }
                info!("discovered peer={peer_id}");
                let now = now_ms();
                let connected_others: Vec<String> = {
                    let peers = self.peers.read().await;
                    peers.iter().filter(|p| *p != &peer_id).cloned().collect()
                };
                if let Err(e) = self.encounter_log.begin_encounter(&peer_id, now) {
                    warn!("encounter log begin failed for {peer_id}: {e:#}");
                }
                for other in &connected_others {
                    if let Err(e) = self.encounter_log.begin_co_encounter(other, &peer_id, now) {
                        warn!("co-encounter begin failed for {other}/{peer_id}: {e:#}");
                    }
                }
                let transport_hint = self.transport.peer_transport_hint(&peer_id);
                let _ = self.peer_trust.with_peer(&peer_id, now, |t| {
                    t.note_transport_hint(&transport_hint);
                });
                if let Some(local_gh) = self.local_geohash_prefix.as_deref() {
                    self.peer_trust
                        .note_peer_co_presence_geohash(&peer_id, local_gh, now);
                }
                {
                    let mut peers = self.peers.write().await;
                    if !peers.iter().any(|p| p == &peer_id) {
                        peers.push(peer_id.clone());
                    }
                }
                self.send_scope_announce(peer_id.clone()).await?;
                self.send_peer_info(peer_id.clone()).await?;
                self.send_sync_bloom(peer_id.clone()).await?;
                self.flush_custody_to_peer(&peer_id).await?;
                self.retry_identity_rotation_to_peer(&peer_id).await;
                self.update_crowd_density_backoff().await;
                self.maybe_schedule_webrtc_negotiation(&peer_id);
            }
            TransportEvent::PeerDown { peer_id } => {
                info!("expired peer={peer_id}");
                let now = now_ms();
                let connected_others: Vec<String> = {
                    let peers = self.peers.read().await;
                    peers.iter().filter(|p| *p != &peer_id).cloned().collect()
                };
                for other in &connected_others {
                    if let Err(e) = self.encounter_log.end_co_encounter(other, &peer_id, now) {
                        warn!("co-encounter end failed for {other}/{peer_id}: {e:#}");
                    }
                }
                if let Err(e) = self.encounter_log.end_encounter(&peer_id, now) {
                    warn!("encounter log end failed for {peer_id}: {e:#}");
                }
                {
                    let mut peers = self.peers.write().await;
                    peers.retain(|p| p != &peer_id);
                }
                self.peer_scopes.remove(&peer_id);
                self.peer_protocol_caps.remove(&peer_id);
                self.transport.set_webrtc_link_ready(&peer_id, false);
                self.update_crowd_density_backoff().await;
            }
            TransportEvent::DirectReceived { from_peer, message } => {
                self.handle_incoming_direct(from_peer, message).await?;
            }
            TransportEvent::DirectAck { from_peer, ack } => {
                info!(
                    "delivery ack from {} message_id={} accepted={}",
                    from_peer, ack.message_id, ack.accepted
                );
                let now = now_ms();
                let _ = self.peer_trust.with_peer(&from_peer, now, |t| {
                    t.record_relay_outcome(now, ack.accepted);
                    if ack.accepted {
                        t.record_message_accepted(now);
                    }
                });
            }
            TransportEvent::SendFailure { to_peer, reason } => {
                warn!("direct send to {} failed: {}", to_peer, reason);
                let now = now_ms();
                let _ = self.peer_trust.with_peer(&to_peer, now, |t| {
                    t.record_relay_outcome(now, false);
                });
            }
            TransportEvent::ConnectivityChanged { mode } => {
                info!("connectivity mode from transport: {:?}", mode);
            }
            TransportEvent::ScopedReceived {
                from_peer,
                scope,
                payload,
            } => {
                let scope_obj = Scope(scope.clone());
                let subscribed = {
                    let scopes = self.subscribed_scopes.read().await;
                    scopes.iter().any(|pattern| scope_obj.matches(pattern))
                };
                if !subscribed {
                    return Ok(());
                }
                let now = now_ms();
                if self.crisis_active()
                    && mycena_core::resource_map::should_defer_gossip_in_crisis(&scope)
                {
                    self.metrics.write().await.crisis_dropped_background_gossip += 1;
                    return Ok(());
                }
                if crate::ingest_guard::is_sos_scope(&scope)
                    || scope.starts_with(mycena_core::resource_map::RESOURCE_MAP_SCOPE)
                {
                    let geohash = if crate::ingest_guard::is_sos_scope(&scope) {
                        mycena_core::sos::geohash_from_payload_bytes(&payload)
                    } else {
                        mycena_core::resource_map::geohash_from_resource_payload_bytes(&payload)
                    };
                    if crate::ingest_guard::is_sos_scope(&scope) {
                        let trust = self.peer_trust.get(&from_peer, now);
                        let security = self.sos_crisis_tracker.security_config();
                        if let Err(reason) = crate::sos_crisis_gate::check_sos_ingest(
                            &mut self.ingest_guard,
                            &from_peer,
                            geohash.as_deref(),
                            &trust,
                            now,
                            security,
                        ) {
                            self.record_sos_ingest_reject(reason);
                            return Ok(());
                        }
                        if let Some(gh) = geohash.as_deref() {
                            self.sos_crisis_tracker
                                .record_observation(&from_peer, gh, &trust, now);
                            self.peer_trust.note_peer_geohash(&from_peer, gh, now);
                        }
                    } else if let Err(reason) =
                        self.ingest_guard
                            .check_sos_gossip(&from_peer, geohash.as_deref(), now)
                    {
                        self.record_ingest_reject(reason);
                        return Ok(());
                    }
                    if let Some(gh) = geohash.as_deref() {
                        self.peer_trust.note_peer_geohash(&from_peer, gh, now);
                    }
                    self.try_record_fact_at_ingest(&from_peer, &scope, &payload, now);
                }
                if let Err(reason) =
                    self.ingest_guard
                        .check_gossip_payload(&from_peer, payload.len(), now)
                {
                    self.record_ingest_reject(reason);
                    return Ok(());
                }
                if scope.starts_with("mycena/group/")
                    && bincode::deserialize::<WireMessage>(&payload)
                        .map(|wm| matches!(wm, WireMessage::EncryptedGroup { .. }))
                        .unwrap_or(false)
                {
                    self.ingest_unsigned_gossip(
                        from_peer,
                        payload,
                        &scope,
                        "[mycena:group]".into(),
                        now,
                    )
                    .await?;
                    return Ok(());
                }
                // App store listings are bincode payloads; they may decode as UTF-8 by chance.
                // Always route this scope through the app pipeline with a stable body tag.
                if scope == "mycena/appstore/v1" {
                    self.ingest_unsigned_gossip(
                        from_peer,
                        payload,
                        &scope,
                        "[appstore]".into(),
                        now,
                    )
                    .await?;
                    return Ok(());
                }
                if scope == "mycena/proximity/v1" {
                    self.ingest_unsigned_gossip(
                        from_peer,
                        payload,
                        &scope,
                        "[proximity]".into(),
                        now,
                    )
                    .await?;
                    return Ok(());
                }
                if subscribed
                    && (payload.starts_with(b"enc1:") || std::str::from_utf8(&payload).is_err())
                {
                    let body = format!("[bulletin:{scope}]");
                    self.ingest_unsigned_gossip(from_peer, payload, &scope, body, now)
                        .await?;
                    return Ok(());
                }
                if let Ok(body) = String::from_utf8(payload.clone()) {
                    let processed = self
                        .ingest_unsigned_gossip(from_peer.clone(), payload, &scope, body, now)
                        .await?;
                    if processed {
                        info!("gossip recv from {from_peer} [{scope}]");
                    }
                }
            }
        }
        Ok(())
    }

    async fn ingest_unsigned_gossip(
        &mut self,
        from_peer: String,
        payload: Vec<u8>,
        scope: &str,
        body: String,
        now: u64,
    ) -> anyhow::Result<bool> {
        let envelope = mycena_core::data::Envelope::new_unsigned_gossip(
            from_peer.clone(),
            None,
            payload,
            scope,
        );
        if let Err(reason) = self.ingest_guard.check_gossip_message(
            &from_peer,
            &envelope.id.0,
            envelope.created_at_ms,
            now,
        ) {
            self.record_ingest_reject(reason);
            return Ok(false);
        }
        let stored = DirectMessage { envelope, body };
        let should_process = self
            .ingest
            .ingest(&mut self.seen_cache, &stored, false)
            .await?;
        if should_process {
            let _ = self.incoming_tx.send(stored);
        }
        Ok(should_process)
    }

    async fn handle_incoming_direct(
        &mut self,
        from_peer: String,
        message: WireMessage,
    ) -> anyhow::Result<()> {
        if self.is_throttled(&from_peer) {
            return Ok(());
        }
        if let WireMessage::EncryptedDirect { mesh_signature, .. } = &message {
            if mesh_signature.is_none() {
                warn!("DROPPING EncryptedDirect from {from_peer}: no signature");
                let mut metrics = self.metrics.write().await;
                metrics.messages_dropped_no_sig += 1;
                return Ok(());
            }
            let Ok(sender_id) = from_peer.parse::<PeerId>() else {
                warn!("DROPPING EncryptedDirect: unparseable peer_id {from_peer}");
                return Ok(());
            };
            if !mycena_core::transport::verify_encrypted_direct(&message, &sender_id) {
                warn!("DROPPING EncryptedDirect from {from_peer}: invalid signature");
                let mut metrics = self.metrics.write().await;
                metrics.messages_dropped_invalid_sig += 1;
                return Ok(());
            }
        }
        match message {
            WireMessage::Data(mut message) => {
                if message.body == privacy_shaping::COVER_BODY {
                    return Ok(());
                }
                if privacy_shaping::unpad_direct_message(&mut message) {
                    self.privacy_stats.bytes_padded_received += 1;
                }
                match security::validate_data_message_signature(&message, now_ms()) {
                    Err(Sd030DropReason::NoSignature) => {
                        warn!(
                            "SD-030: dropping unsigned message from {} (payload {} bytes)",
                            from_peer,
                            message.envelope.payload.len()
                        );
                        let mut metrics = self.metrics.write().await;
                        metrics.messages_dropped_no_sig += 1;
                        return Ok(());
                    }
                    Err(Sd030DropReason::InvalidSignature) => {
                        warn!("SD-030: dropping invalid-sig message from {}", from_peer);
                        let mut metrics = self.metrics.write().await;
                        metrics.messages_dropped_invalid_sig += 1;
                        self.register_strike(&from_peer);
                        return Ok(());
                    }
                    Err(Sd030DropReason::UnparseableAuthor) => {
                        warn!(
                            "SD-030: dropping message — unparseable author {}",
                            message.envelope.from_peer
                        );
                        let mut metrics = self.metrics.write().await;
                        metrics.messages_dropped_no_sig += 1;
                        return Ok(());
                    }
                    Ok(()) => {
                        if message.envelope.signature.is_none()
                            && !security::is_gossip_relay_body(&message.body)
                        {
                            warn!(
                                "SD-030: unsigned message from {} — will drop after grace period",
                                from_peer
                            );
                        }
                    }
                }
                if message.envelope.hop_count >= message.envelope.max_hops {
                    let mut metrics = self.metrics.write().await;
                    metrics.messages_dropped_hops += 1;
                    return Ok(());
                }
                let author = message.envelope.signature_author_peer();
                let now = now_ms();
                if let Err(reason) = self.ingest_guard.allow_direct(author, &message, now) {
                    self.record_ingest_reject(reason);
                    if matches!(reason, IngestReject::RateLimit | IngestReject::Replay) {
                        self.register_strike(&from_peer);
                    }
                    return Ok(());
                }
                let should_process = self
                    .ingest
                    .ingest(&mut self.seen_cache, &message, false)
                    .await?;
                if !should_process {
                    self.register_strike(&from_peer);
                    return Ok(());
                }
                self.reward_peer(&from_peer);
                let _ = self.incoming_tx.send(message.clone());
                info!("direct recv from {}: {}", from_peer, message.body);

                if message
                    .envelope
                    .to_peer
                    .as_deref()
                    .is_some_and(|to| to == self.local_peer_id)
                {
                    let mut metrics = self.metrics.write().await;
                    metrics.messages_delivered_local += 1;
                    let _ = self
                        .custody_queue
                        .mark_delivered_to_local(&message.envelope.id.0);
                    return Ok(());
                }
                if message.is_expired(now_ms()) {
                    let mut metrics = self.metrics.write().await;
                    metrics.messages_dropped_ttl += 1;
                    return Ok(());
                }
                let essential = traffic_class_for_direct(&message) == TrafficClass::Essential;
                if (!self.crisis_active() || !essential)
                    && !self
                        .energy_policy
                        .can_forward(self.effective_forwarding_state(), message.envelope.priority)
                {
                    return Ok(());
                }

                let mut peers = self.transport.known_peers();
                peers.retain(|peer| peer != &from_peer);
                if let Some(target) = message.envelope.to_peer.clone() {
                    if peers.iter().any(|p| p == &target) {
                        let mut forwarded = message.clone();
                        forwarded.envelope.hop_count =
                            forwarded.envelope.hop_count.saturating_add(1);
                        self.apply_relay_forward_mask(&mut forwarded);
                        self.enqueue(target, forwarded).await?;
                        return Ok(());
                    }
                    let crowd =
                        self.crowd_mode.load(Ordering::Relaxed) || self.crowd_density_active;
                    if !crowd || message.body.starts_with("[bulletin:") {
                        self.find_peer_via_kad(&target).await;
                    }
                    if message.envelope.custody || message.envelope.to_peer.is_some() {
                        let _ = self.maybe_enqueue_custody(&message).await;
                        return Ok(());
                    }
                }
                if message.envelope.custody {
                    let _ = self.maybe_enqueue_custody(&message).await;
                    return Ok(());
                }
                let decision = ForwardingPolicy::decide_crisis_aware(
                    &self.forwarding_policy,
                    &message,
                    &self.local_peer_id,
                    &peers,
                    self.effective_forwarding_state(),
                    self.crisis_active(),
                );
                debug!(
                    "forward decision for message_id={} is {:?}",
                    message.envelope.id.0, decision
                );
                match decision {
                    ForwardDecision::ForwardTo(targets) => {
                        for target in targets {
                            let mut forwarded = message.clone();
                            forwarded.envelope.hop_count =
                                forwarded.envelope.hop_count.saturating_add(1);
                            self.apply_relay_forward_mask(&mut forwarded);
                            self.enqueue(target, forwarded).await?;
                        }
                    }
                    ForwardDecision::Drop(reason) => {
                        warn!(
                            "dropping message_id={} reason={}",
                            message.envelope.id.0, reason
                        );
                    }
                    ForwardDecision::Hold => {
                        if self.crisis_active()
                            && traffic_class_for_direct(&message) == TrafficClass::Background
                        {
                            self.metrics.write().await.crisis_dropped_background_forward += 1;
                        }
                        let _ = self.maybe_enqueue_custody(&message).await;
                    }
                    ForwardDecision::DeliverLocal => {}
                }
            }
            WireMessage::SyncBloom { bloom, count } => {
                if self.crisis_active() {
                    self.metrics.write().await.crisis_dropped_background_sync += 1;
                    return Ok(());
                }
                let Some(remote_bloom) = BloomFilter::from_bytes(&bloom) else {
                    return Ok(());
                };

                let own_ids = self.store.list_ids_window(Duration::from_secs(600)).await?;

                let mut send_to_remote: Vec<String> = Vec::new();
                for id in &own_ids {
                    if !remote_bloom.contains(id) {
                        send_to_remote.push(id.clone());
                    }
                }

                if own_ids.len() as f64 > (count as f64 * 0.8) && !send_to_remote.is_empty() {
                    self.transport
                        .send_direct(
                            from_peer.clone(),
                            WireMessage::SyncIds {
                                ids: send_to_remote.into_iter().take(256).collect(),
                            },
                        )
                        .await?;
                } else if (own_ids.len() as f64) < (count as f64 * 0.8) {
                    let mut own_bloom = BloomFilter::new();
                    for id in &own_ids {
                        own_bloom.insert(id);
                    }
                    self.transport
                        .send_direct(
                            from_peer.clone(),
                            WireMessage::SyncBloom {
                                bloom: own_bloom.to_bytes(),
                                count: own_ids.len() as u64,
                            },
                        )
                        .await?;
                }
            }
            WireMessage::SyncIds { ids } => {
                let mut missing = Vec::new();
                for id in ids.iter().take(256) {
                    if !self.store.contains(id).await? {
                        missing.push(id.clone());
                    }
                }
                if !missing.is_empty() {
                    self.transport
                        .send_direct(from_peer, WireMessage::SyncRequest { ids: missing })
                        .await?;
                }
            }
            WireMessage::SyncRequest { ids } => {
                let mut messages = Vec::new();
                for id in ids.iter().take(128) {
                    if let Some(missing) = self.store.load_by_id(id).await? {
                        messages.push(missing);
                    }
                }
                if !messages.is_empty() {
                    self.transport
                        .send_direct(from_peer, WireMessage::SyncData { messages })
                        .await?;
                }
            }
            WireMessage::SyncData { messages } => {
                for message in messages {
                    let _ = self
                        .ingest
                        .ingest(&mut self.seen_cache, &message, false)
                        .await?;
                }
            }
            WireMessage::PeerInfo {
                enc_pubkey_hex,
                supported_scopes,
                ratchet_registration_id,
                ratchet_signed_prekey_id,
                ratchet_signed_prekey_hex,
                ratchet_signed_prekey_sig_hex,
                ratchet_one_time_prekey_id,
                ratchet_one_time_prekey_hex,
                protocol_capabilities,
                ..
            } => {
                let existing = self.peer_x25519.read().await.get(&from_peer).copied();
                match crypto::verify_enc_pubkey_update(existing.as_ref(), &enc_pubkey_hex) {
                    Ok(bytes) => {
                        persist_peer_enc_key(
                            self.peer_x25519.clone(),
                            self.peer_enc_store.clone(),
                            from_peer.clone(),
                            bytes,
                        )
                        .await;
                        let now = now_ms();
                        let _ = self.peer_trust.with_peer(&from_peer, now, |t| {
                            t.note_confirmed_contact(now);
                        });
                    }
                    Err(e) => {
                        warn!(
                            "rejecting PeerInfo enc key for {from_peer}: {e} (invite/TOFU conflict)"
                        );
                        return Ok(());
                    }
                }
                if let (Some(reg_id), Some(spk_id), Some(spk_hex), Some(sig_hex)) = (
                    ratchet_registration_id,
                    ratchet_signed_prekey_id,
                    ratchet_signed_prekey_hex.as_deref(),
                    ratchet_signed_prekey_sig_hex.as_deref(),
                ) {
                    if let Ok(identity_key) =
                        crypto::parse_x25519_public_hex(&enc_pubkey_hex).map(|pk| *pk.as_bytes())
                    {
                        match mycena_core::crypto::ratchet::prekey_bundle_from_peer_info(
                            reg_id,
                            identity_key,
                            spk_id,
                            spk_hex,
                            sig_hex,
                            ratchet_one_time_prekey_id,
                            ratchet_one_time_prekey_hex.as_deref(),
                        ) {
                            Ok(bundle)
                                if mycena_core::crypto::ratchet::verify_prekey_bundle(&bundle) =>
                            {
                                if let Err(e) =
                                    self.ratchet_store.save_peer_bundle(&from_peer, &bundle)
                                {
                                    warn!("failed to persist ratchet bundle for {from_peer}: {e}");
                                }
                            }
                            Ok(_) => {
                                warn!("rejecting invalid ratchet prekey sig from {from_peer}");
                            }
                            Err(e) => {
                                warn!("invalid ratchet bundle from {from_peer}: {e}");
                            }
                        }
                    }
                }
                let validated: HashSet<String> = supported_scopes
                    .into_iter()
                    .filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
                    .take(32)
                    .collect();
                if !validated.is_empty() {
                    self.peer_scopes
                        .entry(from_peer.clone())
                        .or_default()
                        .extend(validated);
                }
                let negotiated = ProtocolCapabilities::negotiate(
                    &self.local_protocol_capabilities(),
                    &protocol_capabilities,
                );
                self.peer_protocol_caps
                    .insert(from_peer.clone(), negotiated);
                self.apply_negotiated_bearer_caps(&from_peer, negotiated);
                info!(
                    "stored peer info for {from_peer} (ratchet={} custody={} relay={} webrtc={})",
                    negotiated.ratchet,
                    negotiated.custody,
                    negotiated.relay,
                    negotiated.webrtc_allowed()
                );
                self.maybe_schedule_webrtc_negotiation(&from_peer);
            }
            WireMessage::WebRtcSignal {
                to_peer,
                session_id,
                kind,
                payload,
                ice_mline_index,
            } => {
                if to_peer != self.local_peer_id {
                    return Ok(());
                }
                let Some(signal_kind) = WebRtcSignalKind::from_u8(kind) else {
                    return Ok(());
                };
                if payload.len() > MAX_SIGNALING_PAYLOAD {
                    return Ok(());
                }
                self.transport
                    .push_webrtc_inbound_signal(WebRtcSignalInbound {
                        from_peer: from_peer.clone(),
                        session_id,
                        kind: signal_kind,
                        payload,
                        ice_mline_index,
                    });
                if signal_kind == WebRtcSignalKind::DataChannelOpen {
                    self.transport.set_webrtc_link_ready(&from_peer, true);
                }
            }
            WireMessage::EncryptedDirect {
                to_peer,
                sender_enc_pubkey,
                encrypted_payload,
                mesh_signature,
                hop_count,
                max_hops,
                e2e_version,
                priority,
            } => {
                if to_peer == self.local_peer_id {
                    let logical =
                        resolve_logical_sender(&self.peer_x25519, &sender_enc_pubkey, &from_peer)
                            .await;
                    let plain = self.decrypt_e2e_sync(
                        &logical,
                        &from_peer,
                        &encrypted_payload,
                        e2e_version,
                    )?;
                    let (logical_from, app_payload) =
                        mycena_core::e2e_direct_wrap::unwrap_inner(&plain, &from_peer);
                    match crypto::parse_x25519_public_hex(&sender_enc_pubkey) {
                        Ok(pk) => {
                            let bytes = *pk.as_bytes();
                            persist_peer_enc_key(
                                self.peer_x25519.clone(),
                                self.peer_enc_store.clone(),
                                logical_from.clone(),
                                bytes,
                            )
                            .await;
                            let now = now_ms();
                            let _ = self.peer_trust.with_peer(&logical_from, now, |t| {
                                t.note_confirmed_contact(now);
                            });
                        }
                        Err(e) => {
                            warn!(
                                "invalid sender_enc_pubkey on EncryptedDirect from {from_peer}: {e}"
                            );
                        }
                    }
                    let delivered_e2e = mycena_core::crypto::ratchet::wire_e2e_version(
                        &encrypted_payload,
                        e2e_version,
                    );
                    let envelope = mycena_core::data::Envelope::new_with_e2e_version(
                        logical_from,
                        Some(self.local_peer_id.clone()),
                        app_payload,
                        delivered_e2e,
                    );
                    let dm = DirectMessage {
                        envelope,
                        body: "[encrypted]".into(),
                    };
                    let logical = dm.envelope.from_peer.clone();
                    let _ = self.incoming_tx.send(dm);
                    info!(
                        "encrypted direct delivered from {} (logical {})",
                        from_peer, logical
                    );
                    let mut metrics = self.metrics.write().await;
                    metrics.messages_delivered_local += 1;
                } else if hop_count < max_hops {
                    let mut forward = WireMessage::EncryptedDirect {
                        to_peer,
                        sender_enc_pubkey,
                        encrypted_payload,
                        mesh_signature,
                        hop_count: hop_count.saturating_add(1),
                        max_hops,
                        e2e_version,
                        priority,
                    };
                    let relay_kp = self.relay_identity.signing_keypair();
                    let _ = mycena_core::transport::sign_encrypted_direct(&mut forward, &relay_kp);
                    self.forward_wire_to_others(&from_peer, &forward).await?;
                } else {
                    let mut metrics = self.metrics.write().await;
                    metrics.messages_dropped_hops += 1;
                }
            }
            WireMessage::EncryptedGroup { .. } => {
                self.forward_wire_to_others(&from_peer, &message).await?;
            }
            WireMessage::ScopeAnnounce { scopes } => {
                if mycena_core::bootstrap::is_relay_peer(&from_peer) {
                    return Ok(());
                }
                let validated_scopes: HashSet<String> = scopes
                    .into_iter()
                    .filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
                    .take(32)
                    .collect();
                if validated_scopes.is_empty() {
                    return Ok(());
                }
                self.peer_scopes
                    .entry(from_peer)
                    .or_default()
                    .extend(validated_scopes);
            }
        }
        Ok(())
    }

    async fn forward_once(&mut self) -> anyhow::Result<()> {
        self.maybe_rotate_relay_keypair();
        self.evict_expired();
        self.sync_energy_profile_to_transport();
        self.flush_privacy_batch_if_due().await?;
        if !self.crisis_active() {
            self.maybe_send_cover_traffic().await?;
        }
        let profile = self.energy_profile();
        let crisis_params = self.effective_crisis_params();
        let budget = if self.crisis_active() {
            match self.effective_forwarding_state() {
                NodeState::Active => crisis_params.forward_budget_active,
                NodeState::Intermittent => (crisis_params.forward_budget_active / 2).max(2),
                NodeState::Passive => 2,
            }
        } else {
            match self.effective_forwarding_state() {
                NodeState::Active => 10,
                NodeState::Intermittent => 4,
                NodeState::Passive => 1,
            }
        };
        for _ in 0..budget {
            let Some(pending) = self.pending.pop_front() else {
                break;
            };
            if pending.message.is_expired(now_ms()) {
                continue;
            }
            let essential = traffic_class_for_direct(&pending.message) == TrafficClass::Essential;
            if profile.should_skip_nonessential_relay() && !essential {
                if self.crisis_active() {
                    self.metrics.write().await.crisis_dropped_background_forward += 1;
                }
                continue;
            }
            if (!self.crisis_active() || !essential)
                && !self.energy_policy.can_forward(
                    self.effective_forwarding_state(),
                    pending.message.envelope.priority,
                )
            {
                continue;
            }
            let mut outbound = pending.message.clone();
            let padded_bytes = self.apply_outbound_padding(&mut outbound);
            if let Err(err) = self
                .transport
                .send_direct(pending.to_peer.clone(), WireMessage::Data(outbound))
                .await
            {
                warn!("forward send failed to {}: {}", pending.to_peer, err);
            } else {
                let mut metrics = self.metrics.write().await;
                metrics.messages_forwarded += 1;
                metrics.privacy_shaping_stats = self.privacy_stats.clone();
                drop(metrics);
                if padded_bytes > 0 {
                    self.privacy_stats.bytes_padded_sent += padded_bytes as u64;
                }
            }
        }
        let mut metrics = self.metrics.write().await;
        metrics.pending_queue_size = self.pending.len();
        metrics.seen_cache_size = self.seen_cache.len();
        metrics.custody_queue_depth = self.custody_queue.len().unwrap_or(0);
        metrics.privacy_shaping_stats = self.privacy_stats.clone();
        metrics.crisis_active = self.crisis_active();
        self.update_transport_metrics(&mut metrics);
        drop(metrics);
        self.update_crowd_density_backoff().await;
        if let Err(e) = self.forward_custody_once().await {
            warn!("custody forward tick error: {e:#}");
        }
        Ok(())
    }

    async fn flush_privacy_batch_if_due(&mut self) -> anyhow::Result<()> {
        if !self.privacy_shaping.enabled {
            return Ok(());
        }
        if !self.batch_scheduler.is_due(Instant::now()) {
            return Ok(());
        }
        let drained = self.batch_scheduler.drain();
        if drained.is_empty() {
            return Ok(());
        }
        self.privacy_stats.batches_flushed += 1;
        self.privacy_stats.messages_batched += drained.len() as u64;
        for (to_peer, message) in drained {
            self.pending.push_back(PendingMessage { to_peer, message });
        }
        if !self.batch_scheduler.has_deadline() && !self.batch_scheduler.is_empty() {
            self.batch_scheduler
                .reschedule_flush(&self.effective_privacy_shaping());
        }
        Ok(())
    }

    fn apply_outbound_padding(&self, message: &mut DirectMessage) -> usize {
        if !self.privacy_shaping.enabled || !self.privacy_shaping.padding_enabled {
            return 0;
        }
        privacy_shaping::pad_direct_message(message)
    }

    async fn maybe_send_cover_traffic(&mut self) -> anyhow::Result<()> {
        let interval = privacy_shaping::cover_interval_for_state(
            &self.privacy_shaping,
            self.effective_forwarding_state(),
        );
        let Some(interval) = interval else {
            return Ok(());
        };
        let now = Instant::now();
        if self
            .last_cover_at
            .is_some_and(|t| now.duration_since(t) < interval)
        {
            return Ok(());
        }
        let peers: Vec<String> = self
            .transport
            .known_peers()
            .into_iter()
            .filter(|p| !mycena_core::bootstrap::is_relay_peer(p))
            .collect();
        let Some(to_peer) = peers.first().cloned() else {
            return Ok(());
        };
        let mut envelope = privacy_shaping::build_cover_envelope(&self.local_peer_id, &to_peer);
        envelope.priority = Priority::Low;
        self.sign_envelope(&mut envelope)?;
        let msg = DirectMessage {
            envelope,
            body: privacy_shaping::COVER_BODY.to_string(),
        };
        let wire_bytes = msg.envelope.payload.len() as u64;
        self.pending.push_back(PendingMessage {
            to_peer,
            message: msg,
        });
        self.last_cover_at = Some(now);
        self.privacy_stats.cover_packets_sent += 1;
        self.privacy_stats.cover_bytes_sent += wire_bytes;
        Ok(())
    }

    fn bucketed_scopes(scopes: Vec<String>, config: &PrivacyShapingConfig) -> Vec<String> {
        if config.enabled && config.scope_bucketing_enabled {
            privacy_shaping::bucket_scopes_for_announce(scopes)
        } else {
            scopes
        }
    }

    async fn maybe_enqueue_custody(&mut self, message: &DirectMessage) -> anyhow::Result<()> {
        if !self.custody_enabled {
            return Ok(());
        }
        if !CustodyQueue::eligible_for_custody(message) {
            return Ok(());
        }
        if message.envelope.from_peer == self.local_peer_id {
            if let Some(to) = message.envelope.to_peer.as_deref() {
                if !self.custody_origin_allowed(to) {
                    return Ok(());
                }
            }
        }
        if self.custody_queue.enqueue(message)? {
            debug!(
                "custody enqueued message_id={} target={:?}",
                message.envelope.id.0, message.envelope.to_peer
            );
        }
        Ok(())
    }

    async fn forward_custody_once(&mut self) -> anyhow::Result<()> {
        if !self.custody_enabled {
            return Ok(());
        }
        let now = now_ms();
        let _ = self.custody_queue.prune_expired(now);
        let connected = self.transport.known_peers();
        let peer_trust = self.peer_trust.load_all();
        let routing_meta = self.build_routing_meta(&connected, &peer_trust)?;
        let routing_ctx = CustodyRoutingContext {
            peer_trust,
            routing_meta,
            diversity: if self.crowd_density_active {
                PeerDiversityConfig::dense_local_sybil()
            } else {
                PeerDiversityConfig::default()
            },
            negotiated_caps: self.peer_protocol_caps.clone(),
            candidate_scoring: Default::default(),
        };
        if let Some(plan) = self.custody_queue.next_forward_plan_at(
            &self.local_peer_id,
            &connected,
            &self.encounter_log,
            now,
            Some(&routing_ctx),
        )? {
            self.forward_custody_plan(plan).await?;
            let mut metrics = self.metrics.write().await;
            metrics.custody_forwards += 1;
        }
        let extra = self.energy_profile().custody_extra_forwards();
        for _ in 0..extra {
            if let Some(plan) = self.custody_queue.next_forward_plan_at(
                &self.local_peer_id,
                &connected,
                &self.encounter_log,
                now,
                Some(&routing_ctx),
            )? {
                self.forward_custody_plan(plan).await?;
                let mut metrics = self.metrics.write().await;
                metrics.custody_forwards += 1;
            } else {
                break;
            }
        }
        let mut metrics = self.metrics.write().await;
        metrics.custody_queue_depth = self.custody_queue.len().unwrap_or(0);
        Ok(())
    }

    async fn flush_custody_to_peer(&mut self, peer: &str) -> anyhow::Result<()> {
        if !self.custody_enabled {
            return Ok(());
        }
        let plans = self
            .custody_queue
            .plans_for_encounter(&self.local_peer_id, peer, 2)?;
        for plan in plans {
            self.forward_custody_plan(plan).await?;
            let mut metrics = self.metrics.write().await;
            metrics.custody_forwards += 1;
        }
        Ok(())
    }

    async fn sync_known_peers(&mut self) -> anyhow::Result<()> {
        self.retry_pending_identity_rotations().await;
        let peers = self.transport.known_peers();
        for peer in peers {
            self.send_sync_bloom(peer).await?;
        }
        Ok(())
    }

    async fn send_sync_bloom(&mut self, peer_id: String) -> anyhow::Result<()> {
        let ids = self.store.list_ids_window(Duration::from_secs(600)).await?;
        if ids.is_empty() {
            return Ok(());
        }
        let mut bloom = BloomFilter::new();
        for id in &ids {
            bloom.insert(id);
        }
        self.transport
            .send_direct(
                peer_id,
                WireMessage::SyncBloom {
                    bloom: bloom.to_bytes(),
                    count: ids.len() as u64,
                },
            )
            .await
    }

    async fn send_scope_announce(&mut self, peer_id: String) -> anyhow::Result<()> {
        let scopes = Self::bucketed_scopes(
            self.subscribed_scopes
                .read()
                .await
                .iter()
                .cloned()
                .collect(),
            &self.privacy_shaping,
        );
        self.transport
            .send_direct(peer_id, WireMessage::ScopeAnnounce { scopes })
            .await
    }

    async fn send_peer_info(&mut self, peer_id: String) -> anyhow::Result<()> {
        let supported_scopes: Vec<String> = self
            .subscribed_scopes
            .read()
            .await
            .iter()
            .cloned()
            .collect();
        // POISON-RECOVER: reads prekey bundle snapshot for PeerInfo gossip.
        let bundle =
            lock_or_recover!(self.local_prekeys, "local prekeys").public_bundle(&self.enc_keypair);
        let info = WireMessage::PeerInfo {
            enc_pubkey_hex: self.enc_keypair.public_hex(),
            display_name: self.display_name.clone(),
            supported_scopes,
            account_id: Some(self.account_id.clone()),
            transport_epoch: self.transport_epoch,
            ratchet_registration_id: Some(bundle.registration_id),
            ratchet_signed_prekey_id: Some(bundle.signed_prekey_id),
            ratchet_signed_prekey_hex: Some(hex::encode(bundle.signed_prekey)),
            ratchet_signed_prekey_sig_hex: Some(hex::encode(bundle.signed_prekey_sig)),
            ratchet_one_time_prekey_id: bundle.one_time_prekey_id,
            ratchet_one_time_prekey_hex: bundle.one_time_prekey.map(hex::encode),
            protocol_capabilities: self.local_protocol_capabilities(),
        };
        self.transport.send_direct(peer_id, info).await
    }

    fn encrypt_e2e_for_peer_sync(
        &mut self,
        to_peer: &str,
        plaintext: &[u8],
        legacy_pk: Option<[u8; 32]>,
    ) -> anyhow::Result<(Vec<u8>, u8)> {
        use mycena_core::crypto::ratchet::{
            self, RatchetSession, E2E_VERSION_LEGACY, E2E_VERSION_RATCHET,
        };

        let ratchet_allowed = self
            .negotiated_protocol_for(to_peer)
            .map(|n| n.ratchet_v1_allowed())
            .unwrap_or(false);

        if ratchet_allowed {
            if let Some(mut session) = self.ratchet_store.load_session(to_peer)? {
                let wire = ratchet::ratchet_encrypt(&mut session, plaintext)?;
                self.ratchet_store.save_session(&session)?;
                return Ok((wire, E2E_VERSION_RATCHET));
            }
            if let Some(bundle) = self.ratchet_store.load_peer_bundle(to_peer)? {
                if ratchet::verify_prekey_bundle(&bundle) {
                    let (session, wire) = RatchetSession::init_outbound(
                        to_peer.to_string(),
                        &self.enc_keypair,
                        &bundle,
                        plaintext,
                    )?;
                    self.ratchet_store.save_session(&session)?;
                    return Ok((wire, E2E_VERSION_RATCHET));
                }
            }
        }
        let pk_bytes = legacy_pk.ok_or_else(|| anyhow::anyhow!("no enc key for peer {to_peer}"))?;
        let wire = crypto::encrypt_for(plaintext, &crypto::public_key_from_bytes(pk_bytes))?;
        Ok((wire, E2E_VERSION_LEGACY))
    }

    fn decrypt_e2e_sync(
        &mut self,
        logical: &str,
        from_peer: &str,
        payload: &[u8],
        e2e_version: u8,
    ) -> anyhow::Result<Vec<u8>> {
        use mycena_core::crypto::ratchet::{self, RatchetSession, E2E_VERSION_RATCHET};

        let use_ratchet = e2e_version == E2E_VERSION_RATCHET || ratchet::is_ratchet_wire(payload);
        if !use_ratchet {
            return crypto::decrypt_with(payload, &self.enc_keypair);
        }
        if ratchet::is_ratchet_init_frame(payload) {
            // POISON-RECOVER: ratchet init mutates prekeys then persists to sled on success.
            let mut prekeys = lock_or_recover!(self.local_prekeys, "local prekeys");
            let (session, plain) = RatchetSession::init_inbound(
                logical.to_string(),
                &self.enc_keypair,
                &mut prekeys,
                payload,
            )?;
            self.ratchet_store.save_local_prekeys(&prekeys)?;
            drop(prekeys);
            let (author, _) = mycena_core::e2e_direct_wrap::unwrap_inner(&plain, from_peer);
            let session = if author != logical {
                session.with_peer_id(author)
            } else {
                session
            };
            self.ratchet_store.save_session(&session)?;
            return Ok(plain);
        }
        let mut session = self
            .ratchet_store
            .load_session(logical)?
            .ok_or_else(|| anyhow::anyhow!("no ratchet session for {logical}"))?;
        let plain = ratchet::ratchet_decrypt(&mut session, payload)?;
        self.ratchet_store.save_session(&session)?;
        Ok(plain)
    }

    async fn forward_wire_to_others(
        &mut self,
        from_peer: &str,
        message: &WireMessage,
    ) -> anyhow::Result<()> {
        let mut peers = self.transport.known_peers();
        peers.retain(|p| p != from_peer);
        let targets = match message {
            WireMessage::EncryptedDirect {
                priority: Priority::High,
                to_peer,
                ..
            } if !peers.is_empty() => {
                let now = now_ms();
                let peer_trust = self.peer_trust.load_all();
                let boosts = self.local_endorsements.read().await.clone();
                let routing_meta = self.build_routing_meta(&peers, &peer_trust)?;
                let diversity = self.relay_diversity_config_for(to_peer);
                security::select_relay_candidates_with_diversity(&security::RelaySelectionRequest {
                    peers: &peers,
                    to_peer,
                    peer_trust: &peer_trust,
                    endorsement_boost: Some(&boosts),
                    now_ms: now,
                    max_fanout: self.effective_max_relay_fanout(),
                    routing_meta: Some(&routing_meta),
                    diversity: Some(&diversity),
                })
            }
            _ => peers,
        };
        for p in targets {
            let dest = p.clone();
            if let Err(e) = self.transport.send_direct(p, message.clone()).await {
                warn!("wire forward failed to {dest}: {e}");
            }
        }
        Ok(())
    }

    async fn enqueue(&mut self, to_peer: String, message: DirectMessage) -> anyhow::Result<()> {
        if message.is_expired(now_ms()) {
            return Ok(());
        }
        let essential =
            self.crisis_active() && traffic_class_for_direct(&message) == TrafficClass::Essential;
        let shaping = self.effective_privacy_shaping();
        if !essential && privacy_shaping::should_batch(message.envelope.priority, &shaping) {
            self.batch_scheduler.push(to_peer, message);
            if !self.batch_scheduler.has_deadline() {
                self.batch_scheduler.reschedule_flush(&shaping);
            }
            return Ok(());
        }
        if self.pending.len() >= self.pending_cap {
            self.evict_one();
        }
        if self.pending.len() >= self.pending_cap {
            let mut metrics = self.metrics.write().await;
            metrics.messages_dropped_queue += 1;
            return Err(anyhow::anyhow!("pending queue is full"));
        }
        self.pending.push_back(PendingMessage { to_peer, message });
        let mut metrics = self.metrics.write().await;
        metrics.pending_queue_size = self.pending.len();
        metrics.seen_cache_size = self.seen_cache.len();
        Ok(())
    }

    fn evict_expired(&mut self) {
        let now = now_ms();
        self.pending.retain(|item| !item.message.is_expired(now));
    }

    fn evict_one(&mut self) {
        self.evict_expired();
        if self.pending.len() < self.pending_cap {
            return;
        }

        let mut candidate_idx = None;
        let mut candidate_rank = (u8::MAX, u8::MAX);
        for (idx, item) in self.pending.iter().enumerate() {
            let rank = (
                traffic_eviction_rank(&item.message),
                priority_rank(item.message.envelope.priority),
            );
            if rank < candidate_rank {
                candidate_rank = rank;
                candidate_idx = Some(idx);
                if rank == (0, 0) {
                    break;
                }
            }
        }
        if let Some(idx) = candidate_idx {
            let _ = self.pending.remove(idx);
        } else {
            let _ = self.pending.pop_front();
        }
    }
}

impl NodeRunner {
    /// Test-only access for mutex-poison recovery integration tests.
    #[doc(hidden)]
    pub fn peer_trust_for_test(&self) -> &crate::peer_trust::PeerTrustStore {
        &self.peer_trust
    }

    fn targets_for_scope(&self, scope: &str) -> Vec<String> {
        let scope_obj = Scope(scope.to_string());
        self.peer_scopes
            .iter()
            .filter_map(|(peer, scopes)| {
                if scopes.iter().any(|pattern| scope_obj.matches(pattern)) {
                    Some(peer.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn is_throttled(&self, peer: &str) -> bool {
        self.peer_trust.is_throttled(peer, now_ms())
    }

    fn record_sos_ingest_reject(&self, reason: crate::sos_crisis_gate::SosIngestReject) {
        use crate::sos_crisis_gate::SosIngestReject;
        let metrics = self.metrics.clone();
        tokio::spawn(async move {
            let mut m = metrics.write().await;
            match reason {
                SosIngestReject::RateLimit | SosIngestReject::ThrottledPeer => {
                    m.messages_dropped_rate_limit += 1;
                }
            }
        });
    }

    fn record_ingest_reject(&self, reason: IngestReject) {
        let metrics = self.metrics.clone();
        tokio::spawn(async move {
            let mut m = metrics.write().await;
            match reason {
                IngestReject::RateLimit => m.messages_dropped_rate_limit += 1,
                IngestReject::Replay | IngestReject::Stale => m.messages_dropped_replay += 1,
                IngestReject::Oversize => m.messages_dropped_oversize += 1,
            }
        });
    }

    fn register_strike(&self, peer: &str) {
        let now = now_ms();
        let _ = self.peer_trust.with_peer(peer, now, |t| {
            t.record_message_rejected(now);
        });
    }

    fn reward_peer(&self, peer: &str) {
        let now = now_ms();
        let _ = self.peer_trust.with_peer(peer, now, |t| {
            t.record_message_accepted(now);
        });
    }
}

struct BasicSeenCache {
    cap: usize,
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl BasicSeenCache {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            order: VecDeque::new(),
            set: HashSet::new(),
        }
    }

    fn len(&self) -> usize {
        self.set.len()
    }
}

impl SeenCache for BasicSeenCache {
    fn contains(&self, message_id: &str) -> bool {
        self.set.contains(message_id)
    }

    fn insert(&mut self, message_id: String) {
        if self.set.insert(message_id.clone()) {
            self.order.push_back(message_id);
        }
        while self.order.len() > self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.set.remove(&evicted);
            }
        }
    }
}

#[derive(Clone)]
struct PendingMessage {
    to_peer: String,
    message: DirectMessage,
}

fn priority_rank(priority: Priority) -> u8 {
    match priority {
        Priority::Low => 0,
        Priority::Normal => 1,
        Priority::High => 2,
    }
}

fn traffic_eviction_rank(msg: &DirectMessage) -> u8 {
    match traffic_class_for_direct(msg) {
        TrafficClass::Background => 0,
        TrafficClass::Normal => 1,
        TrafficClass::Essential => 2,
    }
}
