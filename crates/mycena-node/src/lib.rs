// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
#![deny(clippy::unwrap_used, clippy::expect_used)]

mod bearer_ble;
mod bearer_mesh;
mod bearer_webrtc;
mod bearer_wifi_aware;
pub mod behaviour;
mod ble_transport;
pub mod connectivity;
mod custody;
mod encounter_log;
mod error;
mod forwarding;
mod gossip_mesh;
mod information_trust_store;
mod information_trust_tracker;
mod ingest_guard;
mod node;
mod node_consistency;
mod peer_enc_keys;
mod peer_trust;
mod ratchet_store;
pub mod secrets;
pub mod security;
mod sos_crisis_gate;
mod storage;
mod transport;
mod transport_meta;

pub use ratchet_store::RatchetSessionStore;
pub use transport::{DirectPeerCap, PeerAdmitAction};

pub use bearer_webrtc::BearerWebRtcRouter;
pub use bearer_wifi_aware::{
    BearerWifiAwareRouter, DiscoveryAnnouncement, DiscoveryPeerEvent, DISCOVERY_ANNOUNCE_MAX,
};
pub use connectivity::{kad_action_for_mode, ConnectivityMonitor, KadConnectivityAction};
pub use custody::{
    CustodyConfig, CustodyForwardPlan, CustodyPendingStatus, CustodyQueue, CustodyRoutingContext,
    ExpectedDelivery, DEFAULT_MAX_CUSTODY_SPRAY,
};
pub use encounter_log::{
    CoEncounterRecord, CustodyCandidateQuery, CustodyCandidateScoring, DeliveryProbabilityEstimate,
    EncounterLogStore, EncounterRecord, ExpectedDeliveryBasis, ExpectedEncounterWindow,
    DEFAULT_EXPECTED_ETA_MAX_MS, DEFAULT_EXPECTED_ETA_MIN_MS, ENCOUNTER_LOG_TTL_MS,
    MIN_EXPECTED_ETA_MS,
};
pub use error::{
    decode_peer_bearer_frame, decode_peer_ble_stream_frame, decode_peer_ble_wire_frame,
    decode_peer_wire_message, NodeError, NodeResult, SendPathError,
};
pub use information_trust_tracker::InformationTrustTracker;
pub use ingest_guard::{IngestGuard, IngestReject};
pub use mycena_core::at_rest::parse_storage_key_hex;
pub use mycena_core::transport::{ConnectivityMode, DirectMessage, StoreStats, WireMessage};
pub use mycena_core::transport_score::{
    decide_discovery_bearer, decide_transport_path, prefer_wifi_aware_for_discovery,
    TransportAvailability, TransportBearer, TransportPath, TransportScore, TransportScoreSample,
    TransportScoreStore,
};
pub use mycena_core::webrtc_signaling::WebRtcSignalInbound;
pub use mycena_l0_facade::EncKey;
pub use node::{
    BroadcastPeerFilter, HostNodeOptions, NodeCommand, NodeConfig, NodeHandle, NodeMetrics,
    NodeRunner, SYSTEM_SCOPES,
};
pub use peer_trust::PeerReputationSnapshot;
pub use secrets::{
    bootstrap_identities, encode_signed_invite_v3, load_or_create_enc_keypair, IdentityBootstrap,
    StorageKey,
};
pub use security::{
    apply_peer_diversity, build_routing_meta_map, corroboration_source_diversity,
    peer_routing_meta, select_relay_candidates, select_relay_candidates_with_diversity,
    PeerDiversityConfig, PeerRoutingMeta, PeerTrust, PeerTrustStore, RelaySelectionRequest,
};
pub use sos_crisis_gate::{
    check_sos_ingest, is_sos_source_sybil_suspicious, is_sos_source_trusted_for_crisis,
    sos_rate_limit_for_peer, SosCrisisTracker, SosIngestReject, SosSecurityConfig,
    COLD_SOS_RATE_PER_MINUTE, SUSPICIOUS_SOS_RATE_PER_MINUTE, TRUSTED_SOS_RATE_PER_MINUTE,
};

/// Load or create the libp2p identity using OS keyring / encrypted storage (no explicit storage key).
pub fn load_or_create_keypair(path: &str) -> anyhow::Result<libp2p::identity::Keypair> {
    secrets::load_or_create_keypair(path, None)
}

/// Like [`load_or_create_keypair`], but reads `MYCENA_STORAGE_KEY` (64 hex chars) when set.
/// Required on headless servers where the OS keyring does not persist across restarts.
pub fn load_or_create_keypair_from_env(path: &str) -> anyhow::Result<libp2p::identity::Keypair> {
    let storage_key = match std::env::var("MYCENA_STORAGE_KEY") {
        Ok(hex) => Some(parse_storage_key_hex(&hex)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => return Err(e.into()),
    };
    secrets::load_or_create_keypair(path, storage_key)
}
