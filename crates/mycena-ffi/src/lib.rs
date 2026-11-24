#![deny(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::empty_line_after_doc_comments)]
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

use async_trait::async_trait;
use mycena_app::contacts::{Contact, ContactStatus as AppContactStatus};
use mycena_app::envelope::{
    AppMessage, BulletinPost as AppBulletinPost, ChatMessage as AppChatMessage,
    MailMessage as AppMailMessage,
};
use mycena_app::groups::Group;
use mycena_app::node::AppNode;
use mycena_app::notify::NotificationSink;
use mycena_app::proximity::PresenceProfile as AppPresenceProfile;
use mycena_app::storage::AppStorage;
use mycena_coin::{
    address_from_keypair, CoinNode, CoinTransport, HotWallet,
    HotWalletConfig as RustHotWalletConfig, LocalLedger, PaymentRequest, UserTrustStore,
    VoucherRiskLevel, VoucherSettlementRegistry, DEFAULT_PRIORITY_DEPOSIT_MUON,
};
use mycena_core::data::Priority;
use mycena_core::energy::NodeState as RustEnergyState;
use mycena_core::energy::{
    EnergyProfile as RustEnergyProfile, MovementIndicator as RustMovementIndicator,
    ThermalState as RustThermalState,
};
use mycena_core::lock_or_recover;
use mycena_core::privacy_shaping::{
    PrivacyShapingConfig as RustPrivacyShapingConfig,
    PrivacyShapingOverheadEstimate as RustPrivacyShapingOverheadEstimate,
    PrivacyShapingProfile as RustPrivacyShapingProfile,
};
use mycena_core::transport::ConnectivityMode as NetConnectivityMode;
use mycena_l1_facade::consistency::{OP_DIRECT_MESSAGE_SEND, OP_GOSSIP_BULLETIN_PUBLISH};
use mycena_node::{
    ConnectivityMonitor, HostNodeOptions, NodeCommand, NodeConfig as RustNodeConfig, NodeHandle,
    NodeRunner,
};
use once_cell::sync::OnceCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

/// Matches `[Error] interface MycenaException` in `mycena.udl`.
#[derive(Debug)]
pub enum MycenaException {
    InstallError { detail: String },
    ChatError { detail: String },
    HostError { detail: String },
}

impl std::fmt::Display for MycenaException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MycenaException::InstallError { detail } => write!(f, "{detail}"),
            MycenaException::ChatError { detail } => write!(f, "{detail}"),
            MycenaException::HostError { detail } => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for MycenaException {}

fn host_err(e: impl ToString) -> MycenaException {
    MycenaException::HostError {
        detail: e.to_string(),
    }
}

uniffi::include_scaffolding!("mycena");

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub db_path: String,
    pub listen_addr: String,
    pub display_name: String,
    /// Empty: [`NodeRunner::new`] loads peers via [`mycena_core::bootstrap::load_bootstrap_peers`].
    pub bootstrap_peers: Vec<String>,
    /// Optional 64-char hex master key for at-rest encryption (Android).
    pub storage_key_hex: Option<String>,
    pub rendezvous_enabled: bool,
    /// When false, use local mesh only (no relay/bootstrap dials).
    pub internet_enabled: bool,
    pub bulletin_subscriptions: Vec<String>,
    pub crowd_mode: bool,
}

#[derive(Debug, Clone)]
pub enum PrivacyShapingProfile {
    Fast,
    Balanced,
    Paranoid,
}

#[derive(Debug, Clone)]
pub struct PrivacyShapingConfig {
    pub enabled: bool,
    pub profile: PrivacyShapingProfile,
    pub batch_min_ms: u64,
    pub batch_max_ms: u64,
    pub padding_enabled: bool,
    pub scope_bucketing_enabled: bool,
    pub cover_traffic_enabled: bool,
    pub cover_interval_secs: u64,
}

#[derive(Debug, Clone)]
pub struct PrivacyShapingOverheadEstimate {
    pub padding_overhead_pct_typical: f64,
    pub batch_latency_ms_min: u64,
    pub batch_latency_ms_max: u64,
    pub cover_bytes_per_hour_active: u64,
    pub cover_bytes_per_hour_intermittent: u64,
    pub battery_impact_summary: String,
}

fn ffi_profile_to_rust(profile: PrivacyShapingProfile) -> RustPrivacyShapingProfile {
    match profile {
        PrivacyShapingProfile::Fast => RustPrivacyShapingProfile::Fast,
        PrivacyShapingProfile::Balanced => RustPrivacyShapingProfile::Balanced,
        PrivacyShapingProfile::Paranoid => RustPrivacyShapingProfile::Paranoid,
    }
}

fn privacy_config_to_rust(config: PrivacyShapingConfig) -> RustPrivacyShapingConfig {
    RustPrivacyShapingConfig {
        enabled: config.enabled,
        profile: ffi_profile_to_rust(config.profile),
        batch_min_ms: config.batch_min_ms,
        batch_max_ms: config.batch_max_ms,
        padding_enabled: config.padding_enabled,
        scope_bucketing_enabled: config.scope_bucketing_enabled,
        cover_traffic_enabled: config.cover_traffic_enabled,
        cover_interval_secs: config.cover_interval_secs,
    }
}

fn privacy_config_from_rust(config: RustPrivacyShapingConfig) -> PrivacyShapingConfig {
    let profile = match config.profile {
        RustPrivacyShapingProfile::Fast => PrivacyShapingProfile::Fast,
        RustPrivacyShapingProfile::Balanced => PrivacyShapingProfile::Balanced,
        RustPrivacyShapingProfile::Paranoid => PrivacyShapingProfile::Paranoid,
    };
    PrivacyShapingConfig {
        enabled: config.enabled,
        profile,
        batch_min_ms: config.batch_min_ms,
        batch_max_ms: config.batch_max_ms,
        padding_enabled: config.padding_enabled,
        scope_bucketing_enabled: config.scope_bucketing_enabled,
        cover_traffic_enabled: config.cover_traffic_enabled,
        cover_interval_secs: config.cover_interval_secs,
    }
}

fn overhead_from_rust(est: RustPrivacyShapingOverheadEstimate) -> PrivacyShapingOverheadEstimate {
    PrivacyShapingOverheadEstimate {
        padding_overhead_pct_typical: est.padding_overhead_pct_typical,
        batch_latency_ms_min: est.batch_latency_ms_min,
        batch_latency_ms_max: est.batch_latency_ms_max,
        cover_bytes_per_hour_active: est.cover_bytes_per_hour_active,
        cover_bytes_per_hour_intermittent: est.cover_bytes_per_hour_intermittent,
        battery_impact_summary: est.battery_impact_summary,
    }
}

#[derive(Debug, Clone)]
pub struct NodeMetrics {
    pub messages_forwarded: u64,
    pub messages_dropped_ttl: u64,
    pub messages_dropped_queue: u64,
    pub messages_delivered_local: u64,
    pub pending_queue_size: u64,
    pub seen_cache_size: u64,
    pub connected_peers: u64,
    pub max_peers: u64,
    pub peer_cap_rejections: u64,
    pub crisis_active: bool,
    pub crowd_density_active: bool,
    pub crisis_dropped_background_gossip: u64,
    pub crisis_dropped_background_forward: u64,
    pub crisis_dropped_background_sync: u64,
    pub webrtc_links_active: u64,
    pub webrtc_host_available: bool,
    pub wifi_aware_available: bool,
    pub discovery_events_wifi_aware: u64,
    pub discovery_events_ble: u64,
    pub prefer_wifi_aware_discovery: bool,
}

#[derive(Debug, Clone)]
pub enum ExpectedDeliveryBasis {
    DeliveryProbability,
    EncounterFrequency,
    NoCarriersNearby,
}

#[derive(Debug, Clone)]
pub struct ExpectedDeliveryData {
    pub eta_min_ms: u64,
    pub eta_max_ms: u64,
    pub confidence: f64,
    pub active_carrier_count: u32,
    pub best_carrier_peer: Option<String>,
    pub basis: ExpectedDeliveryBasis,
}

#[derive(Debug, Clone)]
pub struct CustodyPendingStatus {
    pub message_id: String,
    pub dest_peer: Option<String>,
    pub chat_body: String,
    pub app_chat_id: String,
    pub enqueued_at_ms: u64,
    pub sprays_released: u8,
    pub max_spray: u8,
    pub hop_count: u8,
    pub expected: ExpectedDeliveryData,
}

fn expected_delivery_basis_from_node(
    basis: mycena_node::ExpectedDeliveryBasis,
) -> ExpectedDeliveryBasis {
    match basis {
        mycena_node::ExpectedDeliveryBasis::DeliveryProbability => {
            ExpectedDeliveryBasis::DeliveryProbability
        }
        mycena_node::ExpectedDeliveryBasis::EncounterFrequency => {
            ExpectedDeliveryBasis::EncounterFrequency
        }
        mycena_node::ExpectedDeliveryBasis::NoCarriersNearby => {
            ExpectedDeliveryBasis::NoCarriersNearby
        }
    }
}

fn custody_pending_from_node(s: mycena_node::CustodyPendingStatus) -> CustodyPendingStatus {
    CustodyPendingStatus {
        message_id: s.message_id,
        dest_peer: s.dest_peer,
        chat_body: s.chat_body,
        app_chat_id: s.app_chat_id,
        enqueued_at_ms: s.enqueued_at_ms,
        sprays_released: s.sprays_released,
        max_spray: s.max_spray,
        hop_count: s.hop_count,
        expected: ExpectedDeliveryData {
            eta_min_ms: s.expected.eta_min_ms,
            eta_max_ms: s.expected.eta_max_ms,
            confidence: s.expected.confidence,
            active_carrier_count: s.expected.active_carrier_count,
            best_carrier_peer: s.expected.best_carrier_peer,
            basis: expected_delivery_basis_from_node(s.expected.basis),
        },
    }
}

#[derive(Debug, Clone)]
pub struct UserTrustSummary {
    pub relay_contribution: u32,
    pub uptime_secs: u64,
    pub successful_deliveries: u32,
    pub local_endorsements: u32,
    pub contribution_score: f64,
    pub contribution_tier: String,
}

impl From<mycena_coin::UserTrustSummary> for UserTrustSummary {
    fn from(s: mycena_coin::UserTrustSummary) -> Self {
        Self {
            relay_contribution: s.relay_contribution,
            uptime_secs: s.uptime_secs,
            successful_deliveries: s.successful_deliveries,
            local_endorsements: s.local_endorsements,
            contribution_score: s.contribution_score,
            contribution_tier: s.contribution_tier,
        }
    }
}

struct UserTrustSyncState {
    messages_forwarded: u64,
    messages_delivered_local: u64,
}

fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn priority_for_deposit(deposit_muon: u64) -> Priority {
    if deposit_muon >= DEFAULT_PRIORITY_DEPOSIT_MUON {
        Priority::High
    } else {
        Priority::Normal
    }
}

async fn sync_user_trust_metrics(
    handle: &NodeHandle,
    store: &UserTrustStore,
    sync: &mut UserTrustSyncState,
) {
    let m = handle.metrics().await;
    let now = wall_now_ms();
    if m.messages_forwarded > sync.messages_forwarded {
        let delta = (m.messages_forwarded - sync.messages_forwarded) as u32;
        let _ = store.record_relay_forward(now, delta);
        sync.messages_forwarded = m.messages_forwarded;
    }
    if m.messages_delivered_local > sync.messages_delivered_local {
        let delta = (m.messages_delivered_local - sync.messages_delivered_local) as u32;
        for _ in 0..delta {
            let _ = store.record_delivery(now);
        }
        sync.messages_delivered_local = m.messages_delivered_local;
    }
}

async fn push_endorsements_to_node(handle: &NodeHandle, store: &UserTrustStore) {
    let map = store.endorsement_boost_map();
    let _ = handle.send(NodeCommand::SetLocalEndorsements(map)).await;
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub from_peer: String,
    pub from_display_name: String,
    pub body: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone)]
pub struct BulletinPost {
    pub id: String,
    pub from_display_name: String,
    pub title: String,
    pub body: String,
    pub scope: String,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct InformationTrustData {
    pub corroboration_count: u32,
    pub trusted_corroboration_count: u32,
    pub source_diversity: f32,
    pub geographic_consistency: f32,
    pub staleness_factor: f32,
    pub trust_score: f32,
    pub status: String,
    pub display_label: String,
}

#[derive(Debug, Clone)]
pub struct SosPost {
    pub geohash: String,
    pub message: String,
    pub created_at_ms: u64,
    pub trust: InformationTrustData,
}

#[derive(Debug, Clone)]
pub struct ResourceMapEntry {
    pub geohash: String,
    pub category: String,
    pub description: String,
    pub created_at_ms: u64,
    pub ttl_secs: u64,
    pub expires_at_ms: u64,
    pub trust: InformationTrustData,
}

#[derive(Debug, Clone)]
pub struct VolunteerTaskEntry {
    pub task_id: String,
    pub geohash: String,
    pub category: String,
    pub description: String,
    pub slots_needed: u32,
    pub slots_open: u32,
    pub claims_count: u32,
    pub created_at_ms: u64,
    pub ttl_secs: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct MailMessage {
    pub id: String,
    pub from_peer: String,
    pub from_display_name: String,
    pub to_peer: String,
    pub subject: String,
    pub body: String,
    pub timestamp_ms: u64,
    pub is_read: bool,
}

#[derive(Debug, Clone)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub member_count: u32,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone)]
pub enum ContactStatus {
    Pending,
    Accepted,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct PeerReputationData {
    pub strikes: u8,
    pub last_strike_ms: u64,
    pub throttled_until_ms: u64,
    pub reliability: f32,
    pub spam_score: f32,
    pub relay_success_rate: f32,
    pub age_ms: u64,
    pub mutual_contacts: u16,
}

#[derive(Debug, Clone)]
pub struct ContactInfo {
    pub peer_id: String,
    pub display_name: String,
    pub added_at_ms: u64,
    pub status: ContactStatus,
    pub account_id: Option<String>,
}

fn contact_to_info(c: Contact) -> ContactInfo {
    let status = match c.status {
        AppContactStatus::Pending => ContactStatus::Pending,
        AppContactStatus::Accepted => ContactStatus::Accepted,
        AppContactStatus::Blocked => ContactStatus::Blocked,
    };
    ContactInfo {
        peer_id: c.peer_id,
        display_name: c.display_name,
        added_at_ms: c.added_at_ms,
        status,
        account_id: c.account_id,
    }
}

#[derive(Debug, Clone)]
pub struct PresenceProfileData {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub age: Option<u8>,
    pub gender: Option<String>,
    pub looking_for: Option<String>,
    pub interests: Vec<String>,
    pub photo_base64: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NearbyProfile {
    pub ephemeral_id: String,
    pub enc_pubkey_hex: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub age: Option<u8>,
    pub gender: Option<String>,
    pub looking_for: Option<String>,
    pub interests: Vec<String>,
    pub photo_base64: Option<String>,
    pub seen_at_ms: u64,
    pub has_expired: bool,
    pub interest_sent: bool,
    pub interest_received: bool,
    pub is_mutual: bool,
}

#[derive(Debug, Clone)]
pub struct ProximityMessageData {
    pub from_enc_pubkey_hex: String,
    pub body: String,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ScopeKeyInfo {
    pub scope: String,
    pub display_name: String,
    pub added_at_ms: u64,
}

fn profile_from_data(data: PresenceProfileData) -> AppPresenceProfile {
    AppPresenceProfile {
        display_name: data.display_name,
        bio: data.bio,
        age: data.age,
        gender: data.gender,
        looking_for: data.looking_for,
        interests: data.interests,
        photo_base64: data.photo_base64,
    }
}

fn entry_to_nearby(entry: mycena_app::proximity::ProximityNearbyEntry) -> NearbyProfile {
    let s = entry.signal;
    let has_expired = s.is_expired();
    NearbyProfile {
        ephemeral_id: s.ephemeral_id.to_string(),
        enc_pubkey_hex: s.enc_pubkey_hex,
        display_name: s.profile.display_name,
        bio: s.profile.bio,
        age: s.profile.age,
        gender: s.profile.gender,
        looking_for: s.profile.looking_for,
        interests: s.profile.interests,
        photo_base64: s.profile.photo_base64,
        seen_at_ms: s.created_at_ms,
        has_expired,
        interest_sent: entry.interest_sent,
        interest_received: entry.interest_received,
        is_mutual: entry.is_mutual,
    }
}

#[derive(Debug, Clone)]
pub struct WalletBalance {
    pub confirmed_muon: u64,
    pub pending_muon: u64,
    pub settled_muon: u64,
    pub unsettled_voucher_muon: u64,
    pub voucher_reserved_muon: u64,
}

#[derive(Debug, Clone)]
pub struct VoucherRiskInfo {
    pub voucher_id: String,
    pub amount_muon: u64,
    pub risk: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct TxInfo {
    pub id: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_muon: u64,
    pub fee_muon: u64,
    pub timestamp_ms: u64,
    pub memo: Option<String>,
    pub witness_count: u32,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum EnergyState {
    Active,
    Intermittent,
    Passive,
}

#[derive(Debug, Clone, Copy)]
pub enum ThermalState {
    Normal,
    Warm,
    Critical,
}

#[derive(Debug, Clone, Copy)]
pub enum MovementIndicator {
    Unknown,
    Stationary,
    InMotion,
}

#[derive(Debug, Clone)]
pub struct EnergyProfileData {
    pub battery: EnergyState,
    pub thermal: ThermalState,
    pub movement: MovementIndicator,
}

fn energy_state_to_ffi(s: RustEnergyState) -> EnergyState {
    match s {
        RustEnergyState::Active => EnergyState::Active,
        RustEnergyState::Intermittent => EnergyState::Intermittent,
        RustEnergyState::Passive => EnergyState::Passive,
    }
}

fn energy_state_from_ffi(s: EnergyState) -> RustEnergyState {
    match s {
        EnergyState::Active => RustEnergyState::Active,
        EnergyState::Intermittent => RustEnergyState::Intermittent,
        EnergyState::Passive => RustEnergyState::Passive,
    }
}

fn thermal_state_to_ffi(s: RustThermalState) -> ThermalState {
    match s {
        RustThermalState::Normal => ThermalState::Normal,
        RustThermalState::Warm => ThermalState::Warm,
        RustThermalState::Critical => ThermalState::Critical,
    }
}

fn thermal_state_from_ffi(s: ThermalState) -> RustThermalState {
    match s {
        ThermalState::Normal => RustThermalState::Normal,
        ThermalState::Warm => RustThermalState::Warm,
        ThermalState::Critical => RustThermalState::Critical,
    }
}

fn movement_to_ffi(m: RustMovementIndicator) -> MovementIndicator {
    match m {
        RustMovementIndicator::Unknown => MovementIndicator::Unknown,
        RustMovementIndicator::Stationary => MovementIndicator::Stationary,
        RustMovementIndicator::InMotion => MovementIndicator::InMotion,
    }
}

fn energy_profile_to_ffi(p: RustEnergyProfile) -> EnergyProfileData {
    EnergyProfileData {
        battery: energy_state_to_ffi(p.battery),
        thermal: thermal_state_to_ffi(p.thermal),
        movement: movement_to_ffi(p.movement),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConnectivityMode {
    Internet,
    MeshOnly,
}

impl From<NetConnectivityMode> for ConnectivityMode {
    fn from(m: NetConnectivityMode) -> Self {
        match m {
            NetConnectivityMode::Internet => Self::Internet,
            NetConnectivityMode::MeshOnly => Self::MeshOnly,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HotWalletConfig {
    pub max_cache_muon: u64,
    pub refill_threshold_muon: u64,
    pub refill_amount_muon: u64,
    pub cold_wallet_address: Option<String>,
}

impl From<RustHotWalletConfig> for HotWalletConfig {
    fn from(c: RustHotWalletConfig) -> Self {
        Self {
            max_cache_muon: c.max_cache_muon,
            refill_threshold_muon: c.refill_threshold_muon,
            refill_amount_muon: c.refill_amount_muon,
            cold_wallet_address: c.cold_wallet_address,
        }
    }
}

impl From<HotWalletConfig> for RustHotWalletConfig {
    fn from(c: HotWalletConfig) -> Self {
        Self {
            max_cache_muon: c.max_cache_muon,
            refill_threshold_muon: c.refill_threshold_muon,
            refill_amount_muon: c.refill_amount_muon,
            cold_wallet_address: c.cold_wallet_address,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HotWalletStatus {
    pub confirmed_muon: u64,
    pub pending_muon: u64,
    pub max_cache_muon: u64,
    pub auto_refill_enabled: bool,
    pub needs_refill: bool,
    pub connectivity: ConnectivityMode,
}

#[derive(Debug, Clone)]
pub struct PaymentRequestData {
    pub to_address: String,
    pub amount_muon: u64,
    pub memo: Option<String>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct MiniAppInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub developer: String,
    pub entry: String,
    pub accepts_payments: bool,
    pub payment_address: Option<String>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MiniAppPolicyData {
    pub app_id: String,
    pub reputation_score: u8,
    pub safe_mode_active: bool,
    pub safe_mode_forced: bool,
    pub safe_mode_suggested: bool,
    pub user_safe_mode: bool,
    pub revoked: bool,
    pub strict_csp_eligible: bool,
}

#[derive(Debug, Clone)]
pub struct MiniAppInstallPreview {
    pub manifest: MiniAppInfo,
    pub bundle_hash: String,
    pub trust_level: String,
    pub listing_signature_ok: bool,
    pub installed_version: Option<String>,
    pub is_downgrade: bool,
    pub has_inline_script: bool,
    pub strict_csp_eligible: bool,
    pub bundle_signature_ok: bool,
    pub reproducible_attested: bool,
}

#[derive(Debug, Clone)]
pub struct AppStoreListing {
    pub manifest: MiniAppInfo,
    pub bundle_hash: String,
    pub updated_at_ms: u64,
    pub signature_valid: bool,
}

pub trait NodeEventCallback: Send + Sync {
    fn on_peer_discovered(&self, _peer_id: String) {}
    fn on_peer_lost(&self, _peer_id: String) {}
    fn on_chat_received(&self, _message: ChatMessage) {}
    fn on_mail_received(&self, _message: MailMessage) {}
    fn on_bulletin_received(&self, _post: BulletinPost) {}
    fn on_sos_received(&self, _post: SosPost) {}
    fn on_app_store_listing(&self, _listing: AppStoreListing) {}
    fn on_connectivity_changed(&self, _mode: ConnectivityMode) {}
    fn on_contact_request(&self, _peer_id: String, _display_name: String) {}
}

struct FfiNotifier;

impl NotificationSink for FfiNotifier {
    fn on_chat_received(&self, _from: &str, _preview: &str) {}

    fn on_mail_received(&self, _from: &str, _subject: &str) {}

    fn on_bulletin_posted(&self, _scope: &str, _title: &str) {}

    fn on_sos_received(&self, payload: &mycena_core::sos::SosPayload) {
        // POISON-RECOVER: callback clone only; poison skips one SOS JNI dispatch.
        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
            cb.on_sos_received(to_sos_post(payload, query_sos_trust_sync(payload)));
        }
    }

    fn on_contact_request(&self, peer_id: &str, display_name: &str) {
        // POISON-RECOVER: callback clone only; poison skips one contact JNI dispatch.
        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
            cb.on_contact_request(peer_id.to_string(), display_name.to_string());
        }
    }

    fn on_contact_added(&self, peer_id: &str, display_name: &str) {
        // POISON-RECOVER: callback clone only; poison skips one contact JNI dispatch.
        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
            cb.on_contact_request(peer_id.to_string(), display_name.to_string());
        }
    }
}

static RUNTIME: OnceCell<Runtime> = OnceCell::new();
// POISON-RECOVER: global FFI node slot holds Option<Arc>; poison only blocks one init/shutdown path.
static NODE: LazyLock<Mutex<Option<Arc<RwLock<NodeState>>>>> = LazyLock::new(|| Mutex::new(None));
// POISON-RECOVER: callback registry; clone is cheap and poison skips one event dispatch.
static CALLBACK: LazyLock<Mutex<Option<Arc<dyn NodeEventCallback>>>> =
    LazyLock::new(|| Mutex::new(None));

struct FfiCoinTransport {
    handle: NodeHandle,
}

#[async_trait]
impl CoinTransport for FfiCoinTransport {
    async fn broadcast_coin_inner(&self, coin_inner: Vec<u8>) -> anyhow::Result<()> {
        let payload = AppMessage::encode_coin_payload(&coin_inner)?;
        self.handle
            .mesh_broadcast(
                "mycena/coin/v1".into(),
                "[coin]".into(),
                payload,
                OP_GOSSIP_BULLETIN_PUBLISH,
            )
            .await
    }

    async fn broadcast_coin_voucher_inner(&self, coin_inner: Vec<u8>) -> anyhow::Result<()> {
        let payload = AppMessage::encode_coin_payload(&coin_inner)?;
        self.handle
            .mesh_broadcast_coin_voucher(
                "mycena/coin/v1".into(),
                "[coin]".into(),
                payload,
                OP_GOSSIP_BULLETIN_PUBLISH,
            )
            .await
    }

    async fn peer_payment_voucher_v1_allowed(&self, peer: &str) -> bool {
        self.handle.peer_payment_voucher_v1_allowed(peer).await
    }

    async fn send_direct_coin_inner(
        &self,
        to_peer: String,
        coin_inner: Vec<u8>,
    ) -> anyhow::Result<()> {
        let payload = AppMessage::encode_coin_payload(&coin_inner)?;
        self.handle
            .mesh_send_direct(to_peer, "[coin]".into(), payload, OP_DIRECT_MESSAGE_SEND)
            .await
    }
}

struct NodeState {
    db_path: String,
    handle: NodeHandle,
    app_node: Arc<AppNode>,
    app_store: Arc<mycena_app::miniapp::AppStore>,
    coin_node: Arc<CoinNode>,
    coin_identity_path: String,
    cap_mac_key: [u8; 32],
    local_peer_id: String,
    crowd_mode: Arc<AtomicBool>,
    internet_enabled: Arc<AtomicBool>,
    display_name: Arc<RwLock<String>>,
    runner_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    peer_watch_task: tokio::task::JoinHandle<()>,
    connectivity_task: tokio::task::JoinHandle<()>,
    hot_wallet: Arc<HotWallet>,
    connectivity_mode: Arc<Mutex<NetConnectivityMode>>,
    user_trust: Arc<UserTrustStore>,
    user_trust_task: tokio::task::JoinHandle<()>,
}

pub fn init_node(config: NodeConfig) -> Result<(), MycenaException> {
    let runtime = RUNTIME.get_or_try_init(|| {
        Runtime::new().map_err(|e| host_err(format!("tokio runtime init failed: {e}")))
    })?;
    runtime.block_on(async move {
        #[cfg(target_os = "android")]
        {
            use tracing_subscriber::layer::SubscriberExt;
            use tracing_subscriber::util::SubscriberInitExt;
            if let Ok(android_layer) = tracing_android::layer("mycena") {
                let _ = tracing_subscriber::registry()
                    .with(android_layer)
                    .try_init();
            }
        }

        let (connectivity, internet_enabled) = ConnectivityMonitor::new(config.internet_enabled);
        ConnectivityMonitor::spawn_monitor(
            connectivity.mode_tx.clone(),
            internet_enabled.clone(),
        );
        let connectivity_rx_node = connectivity.mode_rx.clone();
        let mut connectivity_rx_bg = connectivity.mode_rx.clone();

        let storage_key = config
            .storage_key_hex
            .as_deref()
            .map(mycena_node::parse_storage_key_hex)
            .transpose()
            .map_err(host_err)?;

        let listen_addr = config
            .listen_addr
            .parse()
            .map_err(|e| host_err(format!("invalid listen address for node: {e}")))?;
        let rust_config = RustNodeConfig::for_host(HostNodeOptions {
            listen_addr,
            db_path: config.db_path.clone(),
            keypair_path: Some(format!("{}/identity", config.db_path)),
            bootstrap_peers: config.bootstrap_peers.clone(),
            connectivity_rx: Some(connectivity_rx_node),
            display_name: Some(config.display_name.clone()),
            storage_key,
            rendezvous_enabled: config.rendezvous_enabled,
            internet_enabled: internet_enabled.clone(),
            connectivity_mode_tx: Some(connectivity.mode_tx.clone()),
            bulletin_subscriptions: config.bulletin_subscriptions.clone(),
            crowd_mode: config.crowd_mode,
        });

        let (runner, handle) =
            NodeRunner::new(rust_config).map_err(|e| host_err(format!("failed to initialize node runner: {e}")))?;
        let local_peer_id = runner.local_peer_id().to_string();
        let runner_task = tokio::spawn(async move { runner.run().await });

        let app_storage = Arc::new(
            AppStorage::open_with_key(&format!("{}/app", config.db_path), storage_key)
                .map_err(|e| host_err(format!("failed to open app storage: {e}")))?,
        );
        let app_store = Arc::new(
            mycena_app::miniapp::AppStore::open(&format!("{}/miniapp", config.db_path))
                .map_err(|e| host_err(format!("failed to open miniapp store: {e}")))?,
        );
        let coin_identity_path = format!("{}/identity", config.db_path);
        let coin_addr = address_from_keypair(
            &mycena_node::secrets::load_or_create_keypair(&coin_identity_path, storage_key)
                .map_err(|e| host_err(format!("failed to load node identity for coin: {e}")))?,
        );
        let coin_ledger = Arc::new(
            LocalLedger::open(&format!("{}/coin", config.db_path))
                .map_err(|e| host_err(format!("failed to open coin ledger: {e}")))?,
        );
        let voucher_registry = Arc::new(
            VoucherSettlementRegistry::open(&format!("{}/coin_vouchers", config.db_path))
                .map_err(|e| host_err(format!("failed to open voucher registry: {e}")))?,
        );
        let coin_transport = Arc::new(FfiCoinTransport {
            handle: handle.clone(),
        });
        let coin_node = Arc::new(
            CoinNode::new(
                coin_ledger,
                coin_transport,
                coin_addr,
                local_peer_id.clone(),
                coin_identity_path.clone(),
            )
            .with_voucher_registry(voucher_registry),
        );
        let hot_wallet = Arc::new(HotWallet::new(
            RustHotWalletConfig::default(),
            coin_node.clone(),
            coin_identity_path.clone(),
        ));
        let user_trust = Arc::new(
            UserTrustStore::open(&config.db_path)
                .map_err(|e| host_err(format!("failed to open user trust store: {e}")))?,
        );
        let ut_handle = handle.clone();
        let ut_store = user_trust.clone();
        let user_trust_task = tokio::spawn(async move {
            let mut sync = UserTrustSyncState {
                messages_forwarded: 0,
                messages_delivered_local: 0,
            };
            push_endorsements_to_node(&ut_handle, &ut_store).await;
            let mut metrics_tick = tokio::time::interval(std::time::Duration::from_secs(30));
            let mut uptime_tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = metrics_tick.tick() => {
                        sync_user_trust_metrics(&ut_handle, &ut_store, &mut sync).await;
                    }
                    _ = uptime_tick.tick() => {
                        let _ = ut_store.tick_uptime(wall_now_ms(), 60);
                    }
                }
            }
        });
        let connectivity_mode = Arc::new(Mutex::new(*connectivity_rx_bg.borrow()));
        let cm_task = connectivity_mode.clone();
        let hw_task = hot_wallet.clone();
        let connectivity_task = tokio::spawn(async move {
            let mode0 = *connectivity_rx_bg.borrow();
            if let Ok(mut g) = cm_task.lock() {
                *g = mode0;
            }
            if mode0 == NetConnectivityMode::Internet {
                let _ = hw_task.maybe_refill().await;
                let _ = hw_task.enforce_cap().await;
            }
            loop {
                if connectivity_rx_bg.changed().await.is_err() {
                    break;
                }
                let mode = *connectivity_rx_bg.borrow();
                if let Ok(mut g) = cm_task.lock() {
                    *g = mode;
                }
                // POISON-RECOVER: callback clone only; poison skips one connectivity JNI event.
                if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
                    cb.on_connectivity_changed(mode.into());
                }
                if mode == NetConnectivityMode::Internet {
                    if let Err(e) = hw_task.maybe_refill().await {
                        tracing::warn!("hot wallet refill check failed: {e:#}");
                    }
                    if let Err(e) = hw_task.enforce_cap().await {
                        tracing::warn!("hot wallet cap enforcement failed: {e:#}");
                    }
                }
            }
        });

        let enc_keypair =
            mycena_node::secrets::load_or_create_enc_keypair(&config.db_path, storage_key)
                .map_err(|e| host_err(format!("failed to load enc keypair: {e}")))?;

        let (app_node, mut inbox) = AppNode::new(
            handle.clone(),
            local_peer_id.clone(),
            config.display_name.clone(),
            app_storage.clone(),
            Arc::new(FfiNotifier),
            Some(coin_node.clone()),
            Some(app_store.clone()),
            enc_keypair,
        );
        let app_node = Arc::new(app_node);
        app_node.clone().start_incoming_task();
        app_node.clone().restore_accepted_contact_dials();
        app_node.clone().start_peer_contact_sync();

        let peer_handle = handle.clone();
        let peer_watch_task = tokio::spawn(async move {
            let mut known = HashSet::<String>::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tick.tick().await;
                let peers = peer_handle.known_peers().await;
                let current: HashSet<String> = peers.into_iter().collect();
                // POISON-RECOVER: callback clone only; poison skips one peer-discovery JNI batch.
                if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
                    for peer in current.difference(&known) {
                        cb.on_peer_discovered(peer.clone());
                    }
                    for peer in known.difference(&current) {
                        cb.on_peer_lost(peer.clone());
                    }
                }
                known = current;
            }
        });

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok(chat) = inbox.chat_rx.recv() => {
                        // POISON-RECOVER: callback clone only; poison skips one chat JNI dispatch.
                        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
                            cb.on_chat_received(to_chat_message(chat.message, chat.from_peer));
                        }
                    }
                    Ok(mail) = inbox.mail_rx.recv() => {
                        // POISON-RECOVER: callback clone only; poison skips one mail JNI dispatch.
                        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
                            cb.on_mail_received(to_mail_message(mail, false));
                        }
                    }
                    Ok(post) = inbox.bulletin_rx.recv() => {
                        // POISON-RECOVER: callback clone only; poison skips one bulletin/SOS JNI dispatch.
                        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
                            if post.scope == mycena_core::sos::SOS_SCOPE {
                                if let Ok(payload) = mycena_core::sos::SosPayload::decode_json(&post.body) {
                                    cb.on_sos_received(to_sos_post(&payload, query_sos_trust_sync(&payload)));
                                }
                            } else {
                                cb.on_bulletin_received(to_bulletin(post));
                            }
                        }
                    }
                    Ok(listing) = inbox.appstore_rx.recv() => {
                        // POISON-RECOVER: callback clone only; poison skips one app-store JNI dispatch.
                        if let Some(cb) = lock_or_recover!(CALLBACK, "callback lock").clone() {
                            cb.on_app_store_listing(to_app_store_listing(listing));
                        }
                    }
                }
            }
        });

        let cap_mac_key = mycena_app::miniapp::mac_key_from_db_path(&config.db_path);
        let crowd_mode = Arc::new(AtomicBool::new(config.crowd_mode));
        let state = Arc::new(RwLock::new(NodeState {
            db_path: config.db_path.clone(),
            handle,
            app_node,
            app_store,
            coin_node,
            coin_identity_path,
            cap_mac_key,
            local_peer_id,
            crowd_mode,
            internet_enabled,
            display_name: Arc::new(RwLock::new(config.display_name)),
            runner_task,
            peer_watch_task,
            connectivity_task,
            hot_wallet,
            connectivity_mode,
            user_trust,
            user_trust_task,
        }));
        // POISON-RECOVER: assigns whole Option<Arc>; init is atomic from None to Some.
        *lock_or_recover!(NODE, "node lock") = Some(state);
        Ok(())
    })
}

pub fn stop_node() {
    let Some(runtime) = RUNTIME.get() else { return };
    // POISON-RECOVER: take() clears slot; shutdown is idempotent if already None.
    let state = lock_or_recover!(NODE, "node lock").take();
    runtime.block_on(async {
        if let Some(state) = state {
            let guard = state.write().await;
            guard.runner_task.abort();
            guard.peer_watch_task.abort();
            guard.connectivity_task.abort();
            guard.user_trust_task.abort();
        }
    });
}

pub fn local_peer_id() -> String {
    with_state(|s| s.local_peer_id.clone())
}

pub fn local_account_id() -> String {
    with_state(|s| s.handle.account_id().to_string())
}

pub fn relay_peer_id() -> String {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.handle.relay_peer_id()
    })
}

pub fn relay_keypair_age_secs() -> u64 {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.handle.relay_keypair_age_secs()
    })
}

pub fn relay_circuit_multiaddr(remote_peer_id: String) -> Option<String> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        mycena_core::bootstrap::relay_circuit_multiaddr_for_db(
            &state.db_path,
            &remote_peer_id,
            None,
        )
    })
}

pub fn known_peers() -> Vec<String> {
    let state = state_arc();
    runtime().block_on(async {
        let guard = state.read().await;
        guard.handle.known_peers().await
    })
}

pub fn connected_peers() -> Vec<String> {
    let state = state_arc();
    runtime().block_on(async {
        let guard = state.read().await;
        guard.handle.connected_peers().await
    })
}

pub fn list_pending_custody() -> Vec<CustodyPendingStatus> {
    let state = state_arc();
    runtime().block_on(async {
        let guard = state.read().await;
        guard
            .handle
            .pending_custody_statuses()
            .await
            .into_iter()
            .map(custody_pending_from_node)
            .collect()
    })
}

pub fn metrics() -> NodeMetrics {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let m = state.handle.metrics().await;
        NodeMetrics {
            messages_forwarded: m.messages_forwarded,
            messages_dropped_ttl: m.messages_dropped_ttl,
            messages_dropped_queue: m.messages_dropped_queue,
            messages_delivered_local: m.messages_delivered_local,
            pending_queue_size: m.pending_queue_size as u64,
            seen_cache_size: m.seen_cache_size as u64,
            connected_peers: m.connected_peers as u64,
            max_peers: m.max_peers as u64,
            peer_cap_rejections: m.peer_cap_rejections,
            crisis_active: m.crisis_active,
            crowd_density_active: m.crowd_density_active,
            crisis_dropped_background_gossip: m.crisis_dropped_background_gossip,
            crisis_dropped_background_forward: m.crisis_dropped_background_forward,
            crisis_dropped_background_sync: m.crisis_dropped_background_sync,
            webrtc_links_active: m.webrtc_links_active,
            webrtc_host_available: m.webrtc_host_available,
            wifi_aware_available: m.wifi_aware_available,
            discovery_events_wifi_aware: m.discovery_events_wifi_aware,
            discovery_events_ble: m.discovery_events_ble,
            prefer_wifi_aware_discovery: m.prefer_wifi_aware_discovery,
        }
    })
}

pub fn send_chat_direct(to_peer: String, body: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .send_chat(Some(to_peer), body)
            .await
            .map(|_| ())
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

/// Burn utility credits (anti-spam caution) and send chat with elevated mesh priority where supported.
pub fn send_chat_priority(
    to_peer: String,
    body: String,
    deposit_muon: u64,
) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        if deposit_muon > 0 {
            state
                .coin_node
                .burn_priority_deposit(deposit_muon)
                .map_err(|e| MycenaException::ChatError {
                    detail: e.to_string(),
                })?;
        }
        let priority = priority_for_deposit(deposit_muon);
        state
            .app_node
            .send_chat_with_priority(Some(to_peer), body, priority)
            .await
            .map(|_| ())
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn user_trust_summary() -> UserTrustSummary {
    with_state(|s| s.user_trust.snapshot().into())
}

pub fn endorse_peer(peer_id: String, strength: u8) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .user_trust
            .endorse_peer(&peer_id, strength, wall_now_ms())
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })?;
        push_endorsements_to_node(&state.handle, &state.user_trust).await;
        Ok(())
    })
}

pub fn send_chat_broadcast(body: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .send_chat(None, body)
            .await
            .map(|_| ())
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn local_enc_pubkey() -> String {
    with_state(|s| s.handle.local_enc_pubkey_hex())
}

pub fn send_chat_encrypted(to_peer: String, body: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .send_chat_encrypted(to_peer, body, Priority::Normal)
            .await
            .map(|_| ())
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn has_enc_key_for(peer_id: String) -> bool {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.handle.has_enc_key_for(&peer_id).await
    })
}

pub fn list_contacts() -> Vec<ContactInfo> {
    with_state(|state| {
        state
            .app_node
            .list_contacts()
            .unwrap_or_default()
            .into_iter()
            .map(contact_to_info)
            .collect()
    })
}

pub fn list_accepted_contacts() -> Vec<ContactInfo> {
    with_state(|state| {
        state
            .app_node
            .list_accepted_contacts()
            .unwrap_or_default()
            .into_iter()
            .map(contact_to_info)
            .collect()
    })
}

pub fn list_pending_contacts() -> Vec<ContactInfo> {
    with_state(|state| {
        state
            .app_node
            .list_pending_contacts()
            .unwrap_or_default()
            .into_iter()
            .map(contact_to_info)
            .collect()
    })
}

pub fn add_contact(
    peer_id: String,
    display_name: String,
    accepted: bool,
) -> Result<ContactInfo, MycenaException> {
    with_state(|state| {
        state
            .app_node
            .add_contact(&peer_id, &display_name, accepted)
            .map(contact_to_info)
            .map_err(host_err)
    })
}

pub fn accept_contact(peer_id: String) -> Result<ContactInfo, MycenaException> {
    with_state(|state| {
        state
            .app_node
            .accept_contact(&peer_id)
            .map(contact_to_info)
            .map_err(host_err)
    })
}

pub fn reject_contact(peer_id: String) -> Result<(), MycenaException> {
    with_state(|state| state.app_node.reject_contact(&peer_id).map_err(host_err))
}

pub fn block_contact(peer_id: String) -> Result<ContactInfo, MycenaException> {
    with_state(|state| {
        state
            .app_node
            .block_contact(&peer_id)
            .map(contact_to_info)
            .map_err(host_err)
    })
}

pub fn unblock_contact(peer_id: String) -> Result<ContactInfo, MycenaException> {
    with_state(|state| {
        state
            .app_node
            .unblock_contact(&peer_id)
            .map(contact_to_info)
            .map_err(host_err)
    })
}

pub fn peer_reputation(peer_id: String) -> Option<PeerReputationData> {
    runtime().block_on(async {
        let state = state_arc();
        let guard = state.read().await;
        let rep = guard
            .app_node
            .node_handle()
            .peer_reputation(&peer_id)
            .await?;
        Some(PeerReputationData {
            strikes: rep.strikes,
            last_strike_ms: rep.last_strike_ms,
            throttled_until_ms: rep.throttled_until_ms,
            reliability: rep.reliability,
            spam_score: rep.spam_score,
            relay_success_rate: rep.relay_success_rate,
            age_ms: rep.age_ms,
            mutual_contacts: rep.mutual_contacts,
        })
    })
}

pub fn remove_contact(peer_id: String) -> Result<(), MycenaException> {
    with_state(|state| state.app_node.remove_contact(&peer_id).map_err(host_err))
}

pub fn list_groups() -> Vec<GroupInfo> {
    with_state(|state| {
        state
            .app_node
            .storage()
            .all_groups()
            .unwrap_or_default()
            .into_iter()
            .map(|g| GroupInfo {
                id: g.id.clone(),
                name: g.name.clone(),
                member_count: g.members.len() as u32,
                created_at_ms: g.created_at_ms,
            })
            .collect()
    })
}

pub fn create_group(name: String) -> Result<GroupInfo, MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let g = Group::new(name);
        state.app_node.storage().save_group(&g).map_err(host_err)?;
        Ok(GroupInfo {
            id: g.id.clone(),
            name: g.name.clone(),
            member_count: g.members.len() as u32,
            created_at_ms: g.created_at_ms,
        })
    })
}

pub fn import_group_invite(json: String) -> Option<GroupInfo> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let g = Group::from_invite(&json).ok()?;
        state.app_node.storage().save_group(&g).ok()?;
        Some(GroupInfo {
            id: g.id.clone(),
            name: g.name.clone(),
            member_count: g.members.len() as u32,
            created_at_ms: g.created_at_ms,
        })
    })
}

pub fn export_group_invite(group_id: String) -> String {
    with_state(|state| {
        state
            .app_node
            .storage()
            .group_by_id(&group_id)
            .ok()
            .flatten()
            .map(|g| g.export_invite())
            .unwrap_or_default()
    })
}

pub fn send_group_message(group_id: String, body: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let Some(g) = state
            .app_node
            .storage()
            .group_by_id(&group_id)
            .ok()
            .flatten()
        else {
            return;
        };
        let _ = state.app_node.send_group_message(&g, body).await;
    });
}

pub fn delete_group(group_id: String) {
    with_state(|state| {
        let _ = state.app_node.storage().delete_group(&group_id);
    });
}

pub fn group_chat_history(group_id: String, limit: u32) -> Vec<ChatMessage> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let gid = format!("group:{group_id}");
        state
            .app_node
            .storage()
            .group_chat_history(&group_id, limit as usize)
            .unwrap_or_default()
            .into_iter()
            .map(|m| to_chat_message(m, gid.clone()))
            .collect()
    })
}

pub fn chat_history(peer_id: String, limit: u32) -> Vec<ChatMessage> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let my_name = state.app_node.display_name().await;
        let local_peer_id = state.local_peer_id.clone();
        state
            .app_node
            .chat_history(&peer_id, limit as usize)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                let from_peer = if m.from_display_name == my_name {
                    local_peer_id.clone()
                } else {
                    peer_id.clone()
                };
                to_chat_message(m, from_peer)
            })
            .collect()
    })
}

pub fn post_bulletin(
    scope: String,
    title: String,
    body: String,
    ttl_secs: u64,
    geohash: Option<String>,
) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state
            .app_node
            .post_bulletin(scope, title, body, ttl_secs, geohash.as_deref())
            .await;
    });
}

pub fn bulletins_for_scope(scope: String) -> Vec<BulletinPost> {
    with_state(|state| {
        state
            .app_node
            .bulletins_for_scope(&scope)
            .unwrap_or_default()
            .into_iter()
            .map(to_bulletin)
            .collect()
    })
}

pub fn get_bulletin_subscriptions() -> Vec<String> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.handle.bulletin_subscriptions().await
    })
}

pub fn subscribe_bulletin_scope(scope: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.handle.subscribe_bulletin_scope(scope).await;
    })
}

pub fn unsubscribe_bulletin_scope(scope: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.handle.unsubscribe_bulletin_scope(scope).await;
    })
}

pub fn is_bulletin_enabled() -> bool {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.handle.is_bulletin_enabled().await
    })
}

pub fn post_sos(geohash: String, message: String, ttl_secs: u64) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.app_node.post_sos(&geohash, &message, ttl_secs).await;
    });
}

pub fn list_sos_posts() -> Vec<SosPost> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .list_sos_posts_with_trust()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(p, trust)| to_sos_post(&p, trust))
            .collect()
    })
}

pub fn list_resource_map_entries() -> Vec<ResourceMapEntry> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .list_resource_map_entries_with_trust()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(e, trust)| to_resource_map_entry(e, trust))
            .collect()
    })
}

pub fn set_listen_sos(enabled: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.app_node.set_listen_sos(enabled).await;
    });
}

pub fn is_listen_sos_enabled() -> bool {
    with_state(|state| state.app_node.is_listen_sos())
}

/// Crisis manual mode: 0 = follow auto, 1 = force on, 2 = force off.
pub fn set_crisis_manual_mode(mode: u8) {
    use mycena_core::crisis_mode::CrisisManualMode;
    let manual = match mode {
        1 => CrisisManualMode::ForceOn,
        2 => CrisisManualMode::ForceOff,
        _ => CrisisManualMode::FollowAuto,
    };
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.app_node.set_crisis_manual(manual).await;
    });
}

pub fn is_crisis_mode_active() -> bool {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.app_node.is_crisis_active().await
    })
}

pub fn refresh_crisis_auto(internet_reachable: bool, local_geohash_prefix: Option<String>) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state
            .app_node
            .refresh_crisis_auto(internet_reachable, local_geohash_prefix.as_deref())
            .await;
    });
}

pub fn set_local_geohash_prefix(prefix: Option<String>) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state
            .app_node
            .set_local_geohash_prefix(prefix.as_deref())
            .await;
    });
}

pub fn post_resource_map(geohash: String, category: String, description: String, ttl_secs: u64) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state
            .app_node
            .post_resource_map(&geohash, &category, &description, ttl_secs)
            .await;
    });
}

pub fn post_volunteer_task(
    geohash: String,
    category: String,
    description: String,
    slots_needed: u32,
    ttl_secs: u64,
) -> Result<VolunteerTaskEntry, MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .post_volunteer_task(&geohash, &category, &description, slots_needed, ttl_secs)
            .await
            .map(to_volunteer_task_entry)
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn list_volunteer_tasks() -> Vec<VolunteerTaskEntry> {
    with_state(|state| {
        state
            .app_node
            .list_volunteer_tasks()
            .unwrap_or_default()
            .into_iter()
            .map(to_volunteer_task_entry)
            .collect()
    })
}

pub fn claim_volunteer_task(task_id: String) -> Result<VolunteerTaskEntry, MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .claim_volunteer_task(&task_id)
            .await
            .map(to_volunteer_task_entry)
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn list_scope_keys() -> Vec<ScopeKeyInfo> {
    with_state(|state| {
        state
            .app_node
            .list_scope_keys()
            .unwrap_or_default()
            .into_iter()
            .map(|sk| ScopeKeyInfo {
                scope: sk.scope,
                display_name: sk.display_name,
                added_at_ms: sk.added_at_ms,
            })
            .collect()
    })
}

pub fn add_scope_key(invite_json: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .add_scope_key_from_invite(&invite_json)
            .await
            .map(|_| ())
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn create_scope_key(scope: String, display_name: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.app_node.create_scope_key(scope, display_name).await;
    })
}

pub fn export_scope_key(scope: String) -> Result<String, MycenaException> {
    with_state(|state| {
        state
            .app_node
            .export_scope_key(&scope)
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn delete_scope_key(scope: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state.app_node.delete_scope_key(&scope).await;
    })
}

pub fn scope_is_encrypted(scope: String) -> bool {
    with_state(|state| state.app_node.scope_is_encrypted(&scope).unwrap_or(false))
}

pub fn start_proximity(profile: PresenceProfileData, ttl_secs: u32) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .start_proximity(profile_from_data(profile), ttl_secs)
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn stop_proximity() {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.app_node.stop_proximity().await;
    })
}

pub fn nearby_profiles() -> Vec<NearbyProfile> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .nearby_profiles()
            .await
            .into_iter()
            .map(entry_to_nearby)
            .collect()
    })
}

pub fn express_proximity_interest(enc_pubkey_hex: String) -> Result<bool, MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .express_proximity_interest(enc_pubkey_hex)
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn proximity_messages(since_ms: u64) -> Vec<ProximityMessageData> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .proximity_messages(since_ms)
            .await
            .into_iter()
            .map(|m| ProximityMessageData {
                from_enc_pubkey_hex: m.from_enc_pubkey_hex,
                body: m.body,
                received_at_ms: m.received_at_ms,
            })
            .collect()
    })
}

pub fn is_proximity_active() -> bool {
    with_state(|state| state.app_node.is_proximity_active())
}

pub fn send_proximity_message(
    enc_pubkey_hex: String,
    message: String,
) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .send_proximity_message(enc_pubkey_hex, message)
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn send_mail(to_peer: String, subject: String, body: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let app_node = {
            let guard = state.read().await;
            if guard.runner_task.is_finished() {
                return Err(MycenaException::ChatError {
                    detail: "mesh node stopped — restart the app".into(),
                });
            }
            guard.app_node.clone()
        };
        app_node
            .send_mail(to_peer, subject, body, vec![])
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn mail_inbox(limit: u32) -> Vec<MailMessage> {
    with_state(|state| {
        state
            .app_node
            .mail_inbox(limit as usize)
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                let is_read = state.app_node.is_mail_read(&m.id).unwrap_or(false);
                to_mail_message(m, is_read)
            })
            .collect()
    })
}

pub fn mail_sent(limit: u32) -> Vec<MailMessage> {
    with_state(|state| {
        state
            .app_node
            .mail_sent(limit as usize)
            .unwrap_or_default()
            .into_iter()
            .map(|m| to_mail_message(m, false))
            .collect()
    })
}

pub fn mark_mail_read(mail_id: String) {
    if let Ok(id) = uuid::Uuid::parse_str(&mail_id) {
        with_state(|state| {
            let _ = state.app_node.mark_mail_read(&id);
        });
    }
}

pub fn set_display_name(name: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state.app_node.set_display_name(name.clone()).await;
        *state.display_name.write().await = name;
    });
}

pub fn display_name() -> String {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let value = state.display_name.read().await.clone();
        value
    })
}

pub fn set_energy_state(state: EnergyState) {
    let cmd = NodeCommand::SetEnergyState(energy_state_from_ffi(state));
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st.handle.send(cmd).await;
    });
}

pub fn set_thermal_state(thermal: ThermalState) {
    let cmd = NodeCommand::SetThermalState(thermal_state_from_ffi(thermal));
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st.handle.send(cmd).await;
    });
}

pub fn energy_profile() -> EnergyProfileData {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        energy_profile_to_ffi(st.handle.energy_profile().await)
    })
}

pub fn set_rendezvous_enabled(enabled: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::SetRendezvousEnabled(enabled))
            .await;
    });
}

pub fn set_internet_enabled(enabled: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        st.internet_enabled.store(enabled, Ordering::Relaxed);
        let _ = st
            .handle
            .send(NodeCommand::SetInternetEnabled(enabled))
            .await;
    });
}

pub fn is_internet_enabled() -> bool {
    with_state(|s| s.internet_enabled.load(Ordering::Relaxed))
}

pub fn set_crowd_mode(enabled: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        st.crowd_mode.store(enabled, Ordering::Relaxed);
        let _ = st.handle.send(NodeCommand::SetCrowdMode(enabled)).await;
    });
}

pub fn is_crowd_mode_enabled() -> bool {
    with_state(|state| state.crowd_mode.load(Ordering::Relaxed))
}

pub fn set_privacy_shaping(config: PrivacyShapingConfig) {
    let rust = privacy_config_to_rust(config);
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st.handle.send(NodeCommand::SetPrivacyShaping(rust)).await;
    });
}

pub fn privacy_shaping_overhead_estimate(
    config: PrivacyShapingConfig,
) -> PrivacyShapingOverheadEstimate {
    overhead_from_rust(RustPrivacyShapingOverheadEstimate::from_config(
        &privacy_config_to_rust(config),
    ))
}

pub fn default_privacy_shaping_config(profile: PrivacyShapingProfile) -> PrivacyShapingConfig {
    privacy_config_from_rust(RustPrivacyShapingConfig::from_profile(ffi_profile_to_rust(
        profile,
    )))
}

pub fn get_shareable_multiaddrs() -> Vec<String> {
    runtime().block_on(async {
        let state = state_arc();
        let guard = state.read().await;
        let listen = guard.handle.listen_addrs().await;
        let enc = guard.handle.local_enc_pubkey_hex();
        let direct = guard
            .handle
            .stable_direct_multiaddr(&guard.local_peer_id)
            .await;
        let signed_invite = guard
            .handle
            .encode_signed_invite_v3(&guard.local_peer_id, direct.as_deref())
            .await
            .ok();
        mycena_core::bootstrap::shareable_dial_multiaddrs(
            &guard.local_peer_id,
            &enc,
            &listen,
            signed_invite.as_deref(),
        )
    })
}

pub fn build_offline_share_bootstrap_sidecar(
    install_url: String,
) -> Result<Vec<u8>, MycenaException> {
    runtime().block_on(async {
        let state = state_arc();
        let guard = state.read().await;
        let bootstrap = guard
            .app_node
            .build_offline_share_bootstrap(install_url)
            .await
            .map_err(|e| MycenaException::InstallError {
                detail: e.to_string(),
            })?;
        bootstrap
            .to_sidecar_bytes()
            .map_err(|e| MycenaException::InstallError {
                detail: e.to_string(),
            })
    })
}

pub fn propagate_installed_miniapps() -> u32 {
    runtime().block_on(async {
        let state = state_arc();
        let guard = state.read().await;
        guard
            .app_node
            .propagate_installed_miniapps()
            .await
            .unwrap_or(0)
    })
}

pub fn add_bootstrap_peer(multiaddr: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state
            .handle
            .send(NodeCommand::AddBootstrapPeer { multiaddr })
            .await;
    });
}

#[derive(Debug, Clone)]
pub struct BleOutboundFrame {
    pub to_peer: String,
    pub frame: Vec<u8>,
    /// 0 = BLE, 1 = Wi‑Fi Direct, 2 = Wi‑Fi Aware discovery, 3 = WebRTC (see `BearerMedium`).
    pub medium: u8,
}

#[derive(Debug, Clone)]
pub struct WebRtcSignalFrame {
    pub from_peer: String,
    pub session_id: String,
    pub kind: u8,
    pub payload: String,
    pub ice_mline_index: Option<u32>,
}

pub fn connect_invite(raw: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .clone()
            .connect_from_invite(&raw)
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn connect_transport_invite(raw: String) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .clone()
            .register_transport_from_invite(&raw)
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn set_wifi_aware_available(available: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::SetWifiAwareAvailable(available))
            .await;
    });
}

pub fn set_ble_discovery_active(active: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::SetBleDiscoveryActive(active))
            .await;
    });
}

pub fn prefer_wifi_aware_for_discovery() -> bool {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        st.handle.prefer_wifi_aware_for_discovery().await
    })
}

pub fn queue_discovery_announcement(invite_uri: String) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::QueueDiscoveryAnnouncement { invite_uri })
            .await;
    });
}

pub fn ingest_wifi_aware_discovery(peer_hint: String, invite_uri: String, latency_ms: u64) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::IngestWifiAwareDiscovery {
                peer_hint,
                invite_uri,
                latency_ms,
            })
            .await;
    });
}

pub fn record_ble_discovery_observation(latency_ms: u64, bytes: u32, success: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::RecordBleDiscoveryObservation {
                latency_ms,
                bytes,
                success,
            })
            .await;
    });
}

pub fn drain_discovery_announcements(max: u32) -> Vec<String> {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        if st
            .handle
            .send(NodeCommand::DrainDiscoveryAnnouncements {
                max: max as usize,
                reply: tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    })
}

pub fn ingest_ble_frame(from_peer: String, frame: Vec<u8>) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .handle
            .send(NodeCommand::IngestBleFrame { from_peer, frame })
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn peer_id_from_ble_frame(frame: Vec<u8>) -> Option<String> {
    mycena_core::ble_mesh::sender_peer_from_frame(&frame)
}

pub fn drain_ble_outbound(to_peer: Option<String>, max: u32) -> Vec<BleOutboundFrame> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = state
            .handle
            .send(NodeCommand::DrainBleOutbound {
                to_peer,
                max: max as usize,
                reply: Some(tx),
            })
            .await;
        rx.await
            .unwrap_or_default()
            .into_iter()
            .map(|(to_peer, frame)| BleOutboundFrame {
                to_peer,
                frame,
                medium: 0,
            })
            .collect()
    })
}

pub fn drain_bearer_outbound(to_peer: Option<String>, max: u32) -> Vec<BleOutboundFrame> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = state
            .handle
            .send(NodeCommand::DrainBearerOutbound {
                to_peer,
                max: max as usize,
                reply: Some(tx),
            })
            .await;
        rx.await
            .unwrap_or_default()
            .into_iter()
            .map(|(to_peer, frame, medium)| BleOutboundFrame {
                to_peer,
                frame,
                medium: medium as u8,
            })
            .collect()
    })
}

pub fn set_webrtc_available(available: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::SetWebRtcAvailable(available))
            .await;
    });
}

pub fn set_webrtc_link_ready(peer: String, ready: bool) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::SetWebRtcLinkReady { peer, ready })
            .await;
    });
}

pub fn ingest_webrtc_frame(from_peer: String, frame: Vec<u8>) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        st.handle
            .send(NodeCommand::IngestWebRtcFrame { from_peer, frame })
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn post_webrtc_signal(
    to_peer: String,
    session_id: String,
    kind: u8,
    payload: String,
    ice_mline_index: Option<u32>,
) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let _ = st
            .handle
            .send(NodeCommand::PostWebRtcSignal {
                to_peer,
                session_id,
                kind,
                payload,
                ice_mline_index,
            })
            .await;
    });
}

pub fn drain_webrtc_signaling_inbound(max: u32) -> Vec<WebRtcSignalFrame> {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = st
            .handle
            .send(NodeCommand::DrainWebRtcSignalingInbound {
                max: max as usize,
                reply: tx,
            })
            .await;
        rx.await
            .unwrap_or_default()
            .into_iter()
            .map(|s| WebRtcSignalFrame {
                from_peer: s.from_peer,
                session_id: s.session_id,
                kind: s.kind as u8,
                payload: s.payload,
                ice_mline_index: s.ice_mline_index,
            })
            .collect()
    })
}

pub fn drain_webrtc_negotiation_requests(max: u32) -> Vec<String> {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = st
            .handle
            .send(NodeCommand::DrainWebRtcNegotiationRequests {
                max: max as usize,
                reply: tx,
            })
            .await;
        rx.await.unwrap_or_default()
    })
}

pub fn webrtc_stun_servers() -> Vec<String> {
    mycena_core::webrtc_signaling::default_stun_servers()
}

pub fn queue_bearer_bulk(to_peer: String, payload: Vec<u8>) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .handle
            .send(NodeCommand::QueueBearerBulk { to_peer, payload })
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn register_ble_link(peer_id: String, device_id: String) {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        let _ = state
            .handle
            .send(NodeCommand::RegisterBleLink { peer_id, device_id })
            .await;
    });
}

pub fn ingest_ble_stream_frame(from_peer: String, frame: Vec<u8>) -> Result<(), MycenaException> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .handle
            .send(NodeCommand::BleStreamInbound { from_peer, frame })
            .await
            .map_err(|e| MycenaException::ChatError {
                detail: e.to_string(),
            })
    })
}

pub fn drain_ble_stream_outbound(to_peer: String, max: u32) -> Vec<Vec<u8>> {
    let all = drain_bearer_outbound(Some(to_peer.clone()), max);
    all.into_iter()
        .filter(|item| {
            mycena_core::bearer_transport::decode_bearer_frame(&item.frame)
                .map(|f| {
                    matches!(
                        f.kind,
                        mycena_core::bearer_transport::BearerPayloadKind::StreamDirect
                            | mycena_core::bearer_transport::BearerPayloadKind::LegacyStream
                    )
                })
                .unwrap_or(false)
        })
        .map(|item| item.frame)
        .collect()
}

pub fn ble_peer_addrs() -> Vec<String> {
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .handle
            .listen_addrs()
            .await
            .into_iter()
            .filter(|a| mycena_core::ble_multiaddr::is_ble_multiaddr(a))
            .collect()
    })
}

pub fn set_event_callback(callback: Box<dyn NodeEventCallback>) {
    // POISON-RECOVER: replaces callback Arc wholesale; old callback remains valid until dropped.
    *lock_or_recover!(CALLBACK, "callback lock") = Some(Arc::from(callback));
}

pub fn wallet_address() -> String {
    with_state(|s| s.coin_node.local_address().to_string())
}

pub fn wallet_balance() -> WalletBalance {
    with_state(|s| match s.coin_node.balance_eventual() {
        Ok(view) => {
            let settled = s
                .coin_node
                .balance_settled()
                .map(|v| v.spendable_muon())
                .unwrap_or(0);
            let acc = s
                .coin_node
                .voucher_exposure()
                .ok()
                .map(|e| e.unsettled_voucher_muon)
                .unwrap_or(0);
            let reserved = s
                .coin_node
                .balance_settled()
                .map(|v| v.voucher_reserved_muon())
                .unwrap_or(0);
            WalletBalance {
                confirmed_muon: view.confirmed_muon(),
                pending_muon: view.pending_muon(),
                settled_muon: settled,
                unsettled_voucher_muon: acc,
                voucher_reserved_muon: reserved,
            }
        }
        Err(_) => WalletBalance {
            confirmed_muon: 0,
            pending_muon: 0,
            settled_muon: 0,
            unsettled_voucher_muon: 0,
            voucher_reserved_muon: 0,
        },
    })
}

fn voucher_risk_label(risk: VoucherRiskLevel) -> &'static str {
    match risk {
        VoucherRiskLevel::Unsettled => "unsettled",
        VoucherRiskLevel::Settled => "settled",
        VoucherRiskLevel::DoubleSpendDetected => "double_spend_detected",
        VoucherRiskLevel::Expired => "expired",
        VoucherRiskLevel::Revoked => "revoked",
    }
}

pub fn wallet_voucher_risks() -> Vec<VoucherRiskInfo> {
    with_state(|s| {
        s.coin_node
            .voucher_exposure()
            .map(|view| {
                view.notices
                    .into_iter()
                    .map(|n| VoucherRiskInfo {
                        voucher_id: n.voucher_id,
                        amount_muon: n.amount_muon,
                        risk: voucher_risk_label(n.risk).to_string(),
                        message: n.message,
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

pub fn send_transaction(to_address: String, amount_muon: u64, memo: Option<String>) {
    let state = state_arc();
    let res = runtime().block_on(async {
        let st = state.read().await;
        if !mycena_coin::validate_address(&to_address) {
            anyhow::bail!("invalid MeshCoin address");
        }
        st.coin_node
            .submit_transfer_from_identity_path(
                &st.coin_identity_path,
                to_address,
                amount_muon,
                1000,
                memo,
            )
            .await
    });
    if let Err(e) = res {
        tracing::warn!("send_transaction failed: {e:#}");
    }
}

pub fn wallet_recent_transactions(limit: u32) -> Vec<TxInfo> {
    with_state(|s| {
        s.coin_node
            .recent_transactions(limit as usize)
            .unwrap_or_default()
            .into_iter()
            .map(tx_to_info)
            .collect()
    })
}

pub fn request_faucet() {
    with_state(|s| {
        if let Err(e) = s.coin_node.request_faucet_coins() {
            tracing::warn!("request_faucet failed: {e:#}");
        }
    });
}

pub fn get_hot_wallet_config() -> HotWalletConfig {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        st.hot_wallet.config().await.into()
    })
}

pub fn set_hot_wallet_config(config: HotWalletConfig) {
    let c: RustHotWalletConfig = config.into();
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        st.hot_wallet.update_config(c).await;
    });
}

pub fn set_cold_wallet_address(address: Option<String>) {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let mut c = st.hot_wallet.config().await;
        c.cold_wallet_address = address;
        st.hot_wallet.update_config(c).await;
    });
}

pub fn hot_wallet_status() -> HotWalletStatus {
    let state = state_arc();
    runtime().block_on(async {
        let st = state.read().await;
        let (conf, pend) = st.coin_node.balance().unwrap_or((0, 0));
        let cfg = st.hot_wallet.config().await;
        // POISON-RECOVER: reads enum copy; stale value only misreports wallet refill hint once.
        let net = *lock_or_recover!(st.connectivity_mode, "connectivity lock");
        let needs_refill = cfg.cold_wallet_address.is_some() && conf < cfg.refill_threshold_muon;
        HotWalletStatus {
            confirmed_muon: conf,
            pending_muon: pend,
            max_cache_muon: cfg.max_cache_muon,
            auto_refill_enabled: cfg.cold_wallet_address.is_some(),
            needs_refill,
            connectivity: net.into(),
        }
    })
}

pub fn current_connectivity_mode() -> ConnectivityMode {
    with_state(|s| {
        // POISON-RECOVER: reads enum copy; stale mode only delays one UI connectivity update.
        let net = *lock_or_recover!(s.connectivity_mode, "connectivity lock");
        net.into()
    })
}

pub fn build_payment_request_uri(
    to_address: String,
    amount_muon: u64,
    memo: Option<String>,
) -> String {
    PaymentRequest::new(to_address, amount_muon, memo).to_uri()
}

pub fn parse_payment_request_uri(uri: String) -> Option<PaymentRequestData> {
    let p = PaymentRequest::from_uri(&uri).ok()?;
    if p.is_expired() {
        return None;
    }
    Some(PaymentRequestData {
        to_address: p.to_address,
        amount_muon: p.amount_muon,
        memo: p.memo,
        expires_at_ms: p.expires_at_ms,
    })
}

pub fn miniapp_issue_bridge_session(app_id: String) -> String {
    mycena_app::miniapp::issue_session(&app_id)
}

pub fn miniapp_revoke_bridge_session(app_id: String) {
    mycena_app::miniapp::revoke_session(&app_id);
}

pub fn miniapp_issue_capability(
    app_id: String,
    permission: String,
    session_token: String,
) -> Result<String, MycenaException> {
    with_state(|s| {
        let perm = mycena_app::miniapp::parse_permission_name(&permission)
            .ok_or_else(|| host_err(format!("unknown permission: {permission}")))?;
        Ok(mycena_app::miniapp::issue_capability(
            &s.cap_mac_key,
            &app_id,
            &perm,
            &session_token,
            mycena_app::miniapp::DEFAULT_CAP_TTL_MS,
        ))
    })
}

pub fn miniapp_publish_revocation(app_id: String, reason: String) -> Result<(), MycenaException> {
    runtime().block_on(async {
        let st = state_arc();
        let guard = st.read().await;
        let keypair =
            mycena_node::load_or_create_keypair(&guard.coin_identity_path).map_err(host_err)?;
        let entry = guard
            .app_store
            .build_revocation_gossip(&app_id, &reason, &guard.local_peer_id, &keypair)
            .map_err(host_err)?;
        guard
            .app_node
            .publish_app_revocation(&entry)
            .await
            .map_err(host_err)
    })
}

pub fn miniapp_get_policy(app_id: String) -> Result<MiniAppPolicyData, MycenaException> {
    with_state(|s| {
        let p = s.app_store.policy_snapshot(&app_id).map_err(host_err)?;
        Ok(MiniAppPolicyData {
            app_id: p.app_id,
            reputation_score: p.reputation_score,
            safe_mode_active: p.safe_mode_active,
            safe_mode_forced: p.safe_mode_forced,
            safe_mode_suggested: p.safe_mode_suggested,
            user_safe_mode: p.user_safe_mode,
            revoked: p.revoked,
            strict_csp_eligible: p.strict_csp_eligible,
        })
    })
}

pub fn miniapp_set_safe_mode(app_id: String, enabled: bool) -> Result<(), MycenaException> {
    with_state(|s| {
        s.app_store
            .set_user_safe_mode(&app_id, enabled)
            .map_err(host_err)
    })
}

pub fn miniapp_report_app(app_id: String) -> Result<(), MycenaException> {
    with_state(|s| s.app_store.record_user_report(&app_id).map_err(host_err))
}

fn mini_app_manifest_to_info(m: mycena_app::miniapp::MiniAppManifest) -> MiniAppInfo {
    MiniAppInfo {
        id: m.id,
        name: m.name,
        description: m.description,
        version: m.version,
        developer: m.developer,
        entry: m.entry,
        accepts_payments: m.accepts_payments,
        payment_address: m.payment_address,
        permissions: m.permissions.iter().map(|p| format!("{p:?}")).collect(),
    }
}

pub fn list_installed_apps() -> Vec<MiniAppInfo> {
    with_state(|s| {
        s.app_store
            .installed_apps()
            .unwrap_or_default()
            .into_iter()
            .map(mini_app_manifest_to_info)
            .collect()
    })
}

fn trust_level_name(level: mycena_app::miniapp::InstallTrustLevel) -> String {
    use mycena_app::miniapp::InstallTrustLevel;
    match level {
        InstallTrustLevel::VerifiedListing => "verified_listing".to_string(),
        InstallTrustLevel::MatchingListingHash => "matching_listing_hash".to_string(),
        InstallTrustLevel::SideloadOnly => "sideload_only".to_string(),
        InstallTrustLevel::HashMismatch => "hash_mismatch".to_string(),
    }
}

pub fn preview_miniapp_install(
    bundle_data: Vec<u8>,
) -> Result<MiniAppInstallPreview, MycenaException> {
    with_state(|s| {
        let p = s.app_store.preview_install(&bundle_data).map_err(|e| {
            MycenaException::InstallError {
                detail: e.to_string(),
            }
        })?;
        Ok(MiniAppInstallPreview {
            manifest: mini_app_manifest_to_info(p.manifest),
            bundle_hash: p.bundle_hash,
            trust_level: trust_level_name(p.trust_level),
            listing_signature_ok: p.listing_signature_ok,
            installed_version: p.installed_version,
            is_downgrade: p.is_downgrade,
            has_inline_script: p.has_inline_script,
            strict_csp_eligible: p.strict_csp_eligible,
            bundle_signature_ok: p.bundle_signature_ok,
            reproducible_attested: p.reproducible_attested,
        })
    })
}

pub fn install_app(
    bundle_data: Vec<u8>,
    listing_app_id: Option<String>,
    allow_sideload: bool,
    allow_downgrade: bool,
) -> Result<MiniAppInfo, MycenaException> {
    with_state(|s| {
        let trust = if listing_app_id.is_some() {
            mycena_app::miniapp::InstallTrust::VerifiedListing
        } else if allow_sideload {
            mycena_app::miniapp::InstallTrust::SideloadAcknowledged
        } else {
            return Err(MycenaException::InstallError {
                detail: "listing_app_id or allow_sideload required".into(),
            });
        };
        if let Some(ref id) = listing_app_id {
            let preview = s.app_store.preview_install(&bundle_data).map_err(|e| {
                MycenaException::InstallError {
                    detail: e.to_string(),
                }
            })?;
            if preview.manifest.id != *id {
                return Err(MycenaException::InstallError {
                    detail: "bundle app id does not match listing_app_id".into(),
                });
            }
        }
        let m = s
            .app_store
            .install_verified(&bundle_data, trust, allow_downgrade)
            .map_err(|e| MycenaException::InstallError {
                detail: e.to_string(),
            })?;
        Ok(mini_app_manifest_to_info(m))
    })
}

pub fn install_official_bundle(app_id: String) -> Result<MiniAppInfo, MycenaException> {
    with_state(|s| {
        let m = s.app_store.install_official_bundle(&app_id).map_err(|e| {
            MycenaException::InstallError {
                detail: e.to_string(),
            }
        })?;
        Ok(mini_app_manifest_to_info(m))
    })
}

pub fn uninstall_app(app_id: String) -> Result<(), MycenaException> {
    mycena_app::miniapp::revoke_session(&app_id);
    with_state(|s| {
        s.app_store.uninstall(&app_id).map_err(host_err)?;
        let _ = s.app_node.storage().miniapp_clear_all_for_app(&app_id);
        Ok(())
    })
}

pub fn get_app_file(app_id: String, path: String) -> Option<Vec<u8>> {
    with_state(|s| s.app_store.get_file(&app_id, &path).ok().flatten())
}

pub fn browse_app_store() -> Vec<AppStoreListing> {
    with_state(|s| {
        s.app_store
            .browse_listings()
            .unwrap_or_default()
            .into_iter()
            .map(|row| {
                let signature_valid = row.verify_signature().unwrap_or(false);
                AppStoreListing {
                    manifest: mini_app_manifest_to_info(row.manifest),
                    bundle_hash: row.bundle_hash,
                    updated_at_ms: row.updated_at_ms,
                    signature_valid,
                }
            })
            .collect()
    })
}

pub fn miniapp_storage_get(app_id: String, key: String) -> Option<String> {
    with_state(|s| {
        let sk = format!("app:{app_id}:{key}");
        s.app_node.storage().miniapp_get(&sk).ok().flatten()
    })
}

pub fn miniapp_storage_set(
    app_id: String,
    key: String,
    value: String,
) -> Result<(), MycenaException> {
    use mycena_app::miniapp::storage_quota::{validate_key, validate_value};
    with_state(|s| {
        validate_key(&key).map_err(host_err)?;
        validate_value(&value).map_err(host_err)?;
        let sk = format!("app:{app_id}:{key}");
        s.app_node
            .storage()
            .miniapp_set(&sk, &value)
            .map_err(host_err)
    })
}

pub fn miniapp_storage_delete(app_id: String, key: String) -> Result<(), MycenaException> {
    with_state(|s| {
        let sk = format!("app:{app_id}:{key}");
        s.app_node.storage().miniapp_delete(&sk).map_err(host_err)
    })
}

pub fn miniapp_get_runtime(app_id: String) -> String {
    with_state(|s| {
        s.app_store
            .get_manifest(&app_id)
            .ok()
            .flatten()
            .map(|m| m.guest_runtime().as_str().to_string())
            .unwrap_or_else(|| "webview".to_string())
    })
}

pub fn miniapp_invoke_wasm_run(app_id: String) -> i32 {
    use mycena_app::miniapp::bridge_host::BridgeHost;
    use mycena_app::miniapp::wasm_bridge_host::run_wasm_guest_for_app;

    runtime().block_on(async {
        let st = state_arc();
        let guard = st.read().await;
        let manifest = guard.app_store.get_manifest(&app_id).ok().flatten();
        let Some(m) = manifest else {
            return -1;
        };
        let enc = guard.app_node.node_handle().local_enc_pubkey_hex();
        let safe_mode = guard
            .app_store
            .effective_safe_mode(&app_id)
            .unwrap_or(false);
        let host = BridgeHost::new(
            app_id.clone(),
            m.permissions,
            guard.app_node.clone(),
            guard.app_node.storage(),
            guard.app_store.clone(),
            Some(guard.coin_node.clone()),
            guard.local_peer_id.clone(),
            enc,
            safe_mode,
            guard.cap_mac_key,
        );
        match run_wasm_guest_for_app(&app_id, &guard.app_store, host).await {
            Ok(r) => r.status,
            Err(_) => -1,
        }
    })
}

pub fn miniapp_bridge_call(app_id: String, method: String, args_json: String) -> String {
    use serde_json::json;
    use serde_json::Value;
    runtime().block_on(async {
        let st = state_arc();
        let guard = st.read().await;
        let args: Value = serde_json::from_str(&args_json).unwrap_or(Value::Null);
        let manifest = guard.app_store.get_manifest(&app_id).ok().flatten();
        let permissions = manifest.map(|m| m.permissions).unwrap_or_default();
        let enc = guard.app_node.node_handle().local_enc_pubkey_hex();
        let safe_mode = guard
            .app_store
            .effective_safe_mode(&app_id)
            .unwrap_or(false);
        let host = mycena_app::miniapp::bridge_host::BridgeHost::new(
            app_id,
            permissions,
            guard.app_node.clone(),
            guard.app_node.storage(),
            guard.app_store.clone(),
            Some(guard.coin_node.clone()),
            guard.local_peer_id.clone(),
            enc,
            safe_mode,
            guard.cap_mac_key,
        );
        match host.handle(&method, &args).await {
            Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string()),
            Err(e) => {
                serde_json::to_string(&json!({ "error": e })).unwrap_or_else(|_| "{}".to_string())
            }
        }
    })
}

fn tx_to_info(t: mycena_coin::Transaction) -> TxInfo {
    let witness_count = t.witnesses.len() as u32;
    let confirmed = witness_count >= 3;
    TxInfo {
        id: t.id,
        from_address: t.from,
        to_address: t.to,
        amount_muon: t.amount_muon,
        fee_muon: t.fee_muon,
        timestamp_ms: t.timestamp_ms,
        memo: t.memo,
        witness_count,
        confirmed,
    }
}

fn with_state<T>(f: impl FnOnce(&NodeState) -> T) -> T {
    runtime().block_on(async {
        let state = state_arc();
        let guard = state.read().await;
        f(&guard)
    })
}

#[allow(clippy::expect_used)]
fn runtime() -> &'static Runtime {
    RUNTIME.get().expect("runtime not initialized")
}

#[allow(clippy::expect_used)]
fn state_arc() -> Arc<RwLock<NodeState>> {
    // POISON-RECOVER: clones existing Arc handle; poison before init still fails expect below.
    lock_or_recover!(NODE, "node lock")
        .as_ref()
        .cloned()
        .expect("node not initialized")
}

fn to_chat_message(m: AppChatMessage, from_peer: String) -> ChatMessage {
    ChatMessage {
        id: m.id.to_string(),
        from_peer,
        from_display_name: m.from_display_name,
        body: m.body,
        timestamp_ms: m.timestamp_ms,
    }
}

fn to_app_store_listing(l: mycena_app::miniapp::store::AppStoreListing) -> AppStoreListing {
    let signature_valid = l.verify_signature().unwrap_or(false);
    AppStoreListing {
        manifest: mini_app_manifest_to_info(l.manifest),
        bundle_hash: l.bundle_hash,
        updated_at_ms: l.updated_at_ms,
        signature_valid,
    }
}

fn to_bulletin(p: AppBulletinPost) -> BulletinPost {
    BulletinPost {
        id: p.id.to_string(),
        from_display_name: p.from_display_name,
        title: p.title,
        body: p.body,
        scope: p.scope,
        timestamp_ms: p.timestamp_ms,
        expires_at_ms: p.expires_at_ms,
    }
}

fn query_sos_trust_sync(
    payload: &mycena_core::sos::SosPayload,
) -> mycena_core::information_trust::InformationTrust {
    use mycena_core::information_trust::FactKind;
    use mycena_core::sos::SOS_DEFAULT_TTL_SECS;
    let state = state_arc();
    runtime().block_on(async {
        let state = state.read().await;
        state
            .app_node
            .node_handle()
            .query_fact_trust(
                FactKind::Sos,
                &payload.geohash,
                None,
                &payload.message,
                SOS_DEFAULT_TTL_SECS,
            )
            .await
    })
}

fn to_information_trust(
    trust: mycena_core::information_trust::InformationTrust,
) -> InformationTrustData {
    InformationTrustData {
        corroboration_count: trust.corroboration_count as u32,
        trusted_corroboration_count: trust.trusted_corroboration_count as u32,
        source_diversity: trust.source_diversity,
        geographic_consistency: trust.geographic_consistency,
        staleness_factor: trust.staleness_factor,
        trust_score: trust.trust_score,
        status: format!("{:?}", trust.status).to_ascii_lowercase(),
        display_label: trust.display_label(),
    }
}

fn to_sos_post(
    p: &mycena_core::sos::SosPayload,
    trust: mycena_core::information_trust::InformationTrust,
) -> SosPost {
    SosPost {
        geohash: p.geohash.clone(),
        message: p.message.clone(),
        created_at_ms: p.created_at_ms,
        trust: to_information_trust(trust),
    }
}

fn to_resource_map_entry(
    e: mycena_core::resource_map::ResourceMapEntry,
    trust: mycena_core::information_trust::InformationTrust,
) -> ResourceMapEntry {
    let expires_at_ms = e.expires_at_ms();
    ResourceMapEntry {
        geohash: e.geohash,
        category: e.category.as_str().to_string(),
        description: e.description,
        created_at_ms: e.created_at_ms,
        ttl_secs: e.ttl_secs,
        expires_at_ms,
        trust: to_information_trust(trust),
    }
}

fn to_volunteer_task_entry(task: mycena_core::volunteer::VolunteerTask) -> VolunteerTaskEntry {
    let slots_open = task.slots_open();
    let expires_at_ms = task.expires_at_ms();
    let claims_count = task.claims.len() as u32;
    VolunteerTaskEntry {
        task_id: task.task_id,
        geohash: task.geohash,
        category: task.category.as_str().to_string(),
        description: task.description,
        slots_needed: task.slots_needed,
        slots_open,
        claims_count,
        created_at_ms: task.created_at_ms,
        ttl_secs: task.ttl_secs,
        expires_at_ms,
    }
}

fn to_mail_message(m: AppMailMessage, is_read: bool) -> MailMessage {
    MailMessage {
        id: m.id.to_string(),
        from_peer: m.from_peer,
        from_display_name: m.from_display_name,
        to_peer: m.to_peer,
        subject: m.subject,
        body: m.body,
        timestamp_ms: m.timestamp_ms,
        is_read,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod poison_tests {
    use super::{CALLBACK, NODE};
    use mycena_core::lock_or_recover;

    #[test]
    fn ffi_global_slots_survive_mutex_poison() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = lock_or_recover!(CALLBACK, "callback lock");
            *guard = None;
            panic!("deliberate callback slot poison");
        }));
        *lock_or_recover!(CALLBACK, "callback lock") = None;

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = lock_or_recover!(NODE, "node lock");
            *guard = None;
            panic!("deliberate node slot poison");
        }));
        assert!(lock_or_recover!(NODE, "node lock").is_none());
    }
}
