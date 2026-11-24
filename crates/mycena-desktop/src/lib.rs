// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
#![deny(clippy::unwrap_used, clippy::expect_used)]
mod ble;
mod discovery;
mod share_server;

use async_trait::async_trait;
use mycena_app::contacts::{Contact, ContactStatus};
use mycena_app::envelope::AppMessage;
use mycena_app::groups::Group;
use mycena_app::node::{AppInbox, AppNode};
use mycena_app::notify::NotificationSink;
use mycena_app::storage::AppStorage;
use mycena_coin::{address_from_keypair, CoinNode, CoinTransport, LocalLedger, UserTrustStore};
use mycena_l0_facade::bootstrap::{load_custom_bootstrap_peers, save_custom_bootstrap_peers};
use mycena_l0_facade::energy::NodeState;
use mycena_l1_facade::consistency::{OP_DIRECT_MESSAGE_SEND, OP_GOSSIP_BULLETIN_PUBLISH};
use mycena_l1_facade::ConnectivityMode;
use mycena_l1_facade::{HostNodeOptions, NodeCommand, NodeConfig, NodeHandle, NodeRunner};
use serde_json::json;
use share_server::ShareServer;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::RwLock;

pub struct AppState {
    handle: NodeHandle,
    app_node: Arc<AppNode>,
    app_storage: Arc<AppStorage>,
    app_store: Arc<mycena_app::miniapp::AppStore>,
    coin_node: Arc<CoinNode>,
    user_trust: Arc<UserTrustStore>,
    #[allow(dead_code)]
    user_trust_task: tauri::async_runtime::JoinHandle<()>,
    local_peer_id: String,
    coin_identity_path: String,
    db_path: String,
    cap_mac_key: [u8; 32],
    energy_state: NodeState,
    rendezvous_enabled: bool,
    internet_enabled: bool,
    connectivity_mode: Arc<RwLock<ConnectivityMode>>,
    runner_task: tauri::async_runtime::JoinHandle<()>,
    ble_host: Option<ble::DesktopBleHost>,
    discovery: Arc<discovery::MeshDiscoveryState>,
}

fn connectivity_label(mode: ConnectivityMode) -> &'static str {
    match mode {
        ConnectivityMode::Internet => "internet",
        ConnectivityMode::MeshOnly => "mesh_only",
    }
}

fn connectivity_detail(
    internet_enabled: bool,
    rendezvous_enabled: bool,
    mode: ConnectivityMode,
) -> String {
    if !internet_enabled {
        return "Internet disabled in Settings — desktop uses LAN (mDNS) only; no relay or rendezvous.".into();
    }
    if mode == ConnectivityMode::MeshOnly {
        return "Internet enabled but public relay unreachable — local mesh (same Wi‑Fi) only."
            .into();
    }
    if rendezvous_enabled {
        return "Internet + relay online — LAN, bootstrap relay, and opt-in rendezvous active."
            .into();
    }
    "Internet + relay online — LAN and bootstrap relay; rendezvous discovery off.".into()
}

async fn connectivity_snapshot(s: &AppState) -> serde_json::Value {
    let mode = *s.connectivity_mode.read().await;
    json!({
        "internet_enabled": s.internet_enabled,
        "rendezvous_enabled": s.rendezvous_enabled,
        "connectivity_mode": connectivity_label(mode),
        "connectivity_detail": connectivity_detail(s.internet_enabled, s.rendezvous_enabled, mode),
    })
}

fn emit_settings_updated(app: &AppHandle, snapshot: serde_json::Value) {
    let _ = app.emit("settings-updated", snapshot);
}

struct DesktopUserTrustSync {
    messages_forwarded: u64,
    messages_delivered_local: u64,
}

fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn sync_desktop_user_trust(
    handle: &NodeHandle,
    store: &UserTrustStore,
    sync: &mut DesktopUserTrustSync,
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

async fn push_desktop_endorsements(handle: &NodeHandle, store: &UserTrustStore) {
    let map = store.endorsement_boost_map();
    let _ = handle.send(NodeCommand::SetLocalEndorsements(map)).await;
}

type SharedState = Arc<RwLock<Option<AppState>>>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UiSettings {
    #[serde(default = "default_rendezvous_enabled")]
    rendezvous_enabled: bool,
    #[serde(default = "default_internet_enabled")]
    internet_enabled: bool,
    #[serde(default)]
    max_connectivity: bool,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    listen_sos: bool,
    #[serde(default)]
    crowd_mode: bool,
    /// Geohash prefix (4+ chars) for SOS/bulletin gossip shards — not GPS; user-entered area hint.
    #[serde(default)]
    local_geohash_prefix: Option<String>,
    /// BLE mesh handoff (macOS/Linux — blew GATT host, ADR 003 Phase 1b).
    #[serde(default)]
    ble_bootstrap: bool,
}

fn default_rendezvous_enabled() -> bool {
    true
}

fn default_internet_enabled() -> bool {
    true
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            rendezvous_enabled: true,
            internet_enabled: true,
            max_connectivity: false,
            display_name: None,
            listen_sos: false,
            crowd_mode: false,
            local_geohash_prefix: None,
            ble_bootstrap: false,
        }
    }
}

fn ui_settings_path(db_path: &str) -> PathBuf {
    PathBuf::from(db_path).join("ui_settings.json")
}

fn load_ui_settings(db_path: &str) -> UiSettings {
    let path = ui_settings_path(db_path);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn ensure_db_dir(db_path: &str) -> Result<(), String> {
    std::fs::create_dir_all(db_path).map_err(|e| e.to_string())
}

fn save_ui_settings(db_path: &str, settings: &UiSettings) -> Result<(), String> {
    ensure_db_dir(db_path)?;
    let path = ui_settings_path(db_path);
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

struct DesktopNotifier {
    app: AppHandle,
}

impl NotificationSink for DesktopNotifier {
    fn on_chat_received(&self, from: &str, preview: &str) {
        let _ = self.app.emit(
            "chat-updated",
            json!({ "from_peer": from, "body": preview }),
        );
    }

    fn on_mail_received(&self, _from: &str, _subject: &str) {}

    fn on_bulletin_posted(&self, _scope: &str, _title: &str) {}

    fn on_sos_received(&self, payload: &mycena_l0_facade::sos::SosPayload) {
        let _ = self.app.emit(
            "sos-updated",
            json!({
                "geohash": payload.geohash,
                "message": payload.message,
            }),
        );
    }

    fn on_contact_request(&self, peer_id: &str, display_name: &str) {
        let _ = self.app.emit(
            "contacts-updated",
            json!({ "peer_id": peer_id, "display_name": display_name }),
        );
    }

    fn on_contact_added(&self, peer_id: &str, display_name: &str) {
        let _ = self.app.emit(
            "contacts-updated",
            json!({ "peer_id": peer_id, "display_name": display_name }),
        );
    }
}

#[derive(Clone)]
struct DesktopCoinTransport {
    handle: NodeHandle,
}

#[async_trait]
impl CoinTransport for DesktopCoinTransport {
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

    async fn broadcast_coin_voucher_inner(&self, coin_inner: Vec<u8>) -> anyhow::Result<()> {
        let payload = AppMessage::encode_coin_payload(&coin_inner)?;
        self.handle
            .mesh_broadcast_coin_voucher(
                "mycena/coin".to_string(),
                "coin:broadcast".to_string(),
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
            .mesh_send_direct(
                to_peer,
                "coin:direct".to_string(),
                payload,
                OP_DIRECT_MESSAGE_SEND,
            )
            .await
    }
}

#[tauri::command]
fn get_saved_profile() -> serde_json::Value {
    let path = default_db_path();
    let ui = load_ui_settings(&path);
    let has_data = identity_data_present(&path);
    let can_auto_start = has_data
        && ui
            .display_name
            .as_ref()
            .is_some_and(|n| !n.trim().is_empty());
    json!({
        "display_name": ui.display_name,
        "has_local_data": has_data,
        "can_auto_start": can_auto_start,
        "db_path": path,
    })
}

/// True when prior mesh identity or app database exists (not just an empty folder).
fn identity_data_present(path: &str) -> bool {
    let base = std::path::Path::new(path);
    if base.join(".secrets").join("account_ed25519.enc").exists() {
        return true;
    }
    if base.join("app").exists() {
        return true;
    }
    if base.join("identity").exists() {
        return true;
    }
    false
}

#[tauri::command]
fn get_default_db_path() -> String {
    default_db_path()
}

fn default_db_path() -> String {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("mycena")
        .to_string_lossy()
        .to_string()
}

fn share_dir() -> PathBuf {
    PathBuf::from(default_db_path()).join("share")
}

fn default_share_apk_path() -> PathBuf {
    share_dir().join("mycena.apk")
}

fn ensure_share_dir() -> Result<PathBuf, String> {
    let dir = share_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

type ShareServerState = Arc<ShareServer>;

/// Delete local Mycena data (identity, messages, keys). Safe before first successful start.
fn wipe_local_data_at(path: &str) -> Result<(), String> {
    mycena_l0_facade::clear_keyring_master(path);
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Ok(());
    }
    let mut last_err: Option<String> = None;
    for attempt in 0..5 {
        match std::fs::remove_dir_all(p) {
            Ok(()) => {
                tracing::info!("destroyed local data at {path}");
                mycena_l0_facade::clear_keyring_master(path);
                return Ok(());
            }
            Err(e) => {
                last_err = Some(format!("{e}"));
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                }
            }
        }
    }
    Err(format!(
        "failed to destroy local data at {path}: {}",
        last_err.unwrap_or_else(|| "unknown error".into())
    ))
}

/// Wipe the active database path and the default data dir (when they differ).
fn wipe_all_local_data(active_path: Option<&str>) -> Result<(), String> {
    let default = default_db_path();
    if let Some(path) = active_path {
        wipe_local_data_at(path)?;
        if path != default {
            wipe_local_data_at(&default)?;
        }
    } else {
        wipe_local_data_at(&default)?;
    }
    Ok(())
}

async fn shutdown_active_node(state: &SharedState) {
    let mut guard = state.write().await;
    if let Some(s) = guard.take() {
        s.runner_task.abort();
        drop(s);
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    }
}

#[tauri::command]
async fn reset_local_data(
    state: State<'_, SharedState>,
    db_path: Option<String>,
) -> Result<(), String> {
    let active_path = {
        let guard = state.read().await;
        guard.as_ref().map(|s| s.db_path.clone())
    };
    shutdown_active_node(&state).await;
    let path = db_path.unwrap_or_else(default_db_path);
    wipe_local_data_at(&path)?;
    if path != default_db_path() {
        wipe_local_data_at(&default_db_path())?;
    }
    if let Some(active) = active_path {
        if active != path && active != default_db_path() {
            wipe_local_data_at(&active)?;
        }
    }
    for check in [path.as_str(), default_db_path().as_str()] {
        if std::path::Path::new(check).exists() {
            return Err(format!(
                "local data still present at {check} after wipe — close Mycena completely and retry"
            ));
        }
    }
    Ok(())
}

/// Stop the node, wipe local data, and restart into onboarding.
#[tauri::command]
async fn return_to_onboarding(app: AppHandle, state: State<'_, SharedState>) -> Result<(), String> {
    let active_path = {
        let guard = state.read().await;
        guard.as_ref().map(|s| s.db_path.clone())
    };
    shutdown_active_node(&state).await;
    wipe_all_local_data(active_path.as_deref())?;
    let default = default_db_path();
    if std::path::Path::new(&default).exists() {
        return Err(format!(
            "local data still present at {default} after wipe — close Mycena completely and retry"
        ));
    }
    app.restart();
}

#[tauri::command]
async fn start_node(
    app: AppHandle,
    state: State<'_, SharedState>,
    db_path: String,
    display_name: String,
    bootstrap_peers: Vec<String>,
) -> Result<String, String> {
    {
        let guard = state.read().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.local_peer_id.clone());
        }
    }

    ensure_db_dir(&db_path)?;

    let ui_settings = load_ui_settings(&db_path);
    let mut ui_to_save = ui_settings.clone();
    ui_to_save.display_name = Some(display_name.clone());
    save_ui_settings(&db_path, &ui_to_save)?;
    let (connectivity, internet_enabled) =
        mycena_l1_facade::ConnectivityMonitor::new(ui_settings.internet_enabled);
    let mut mode_rx = connectivity.mode_rx.clone();
    let connectivity_mode = Arc::new(RwLock::new(*mode_rx.borrow()));
    let connectivity_watch = connectivity_mode.clone();
    tokio::spawn(async move {
        loop {
            if mode_rx.changed().await.is_err() {
                break;
            }
            let mode = *mode_rx.borrow();
            *connectivity_watch.write().await = mode;
        }
    });
    mycena_l1_facade::ConnectivityMonitor::spawn_monitor(
        connectivity.mode_tx.clone(),
        internet_enabled.clone(),
    );
    let listen_addr = "/ip4/0.0.0.0/tcp/0"
        .parse()
        .map_err(|e| format!("invalid listen addr: {e}"))?;
    let config = NodeConfig::for_host(HostNodeOptions {
        listen_addr,
        db_path: db_path.clone(),
        keypair_path: Some(format!("{db_path}/identity")),
        bootstrap_peers,
        connectivity_rx: Some(connectivity.mode_rx.clone()),
        connectivity_mode_tx: Some(connectivity.mode_tx.clone()),
        display_name: Some(display_name.clone()),
        storage_key: None,
        rendezvous_enabled: ui_settings.rendezvous_enabled && ui_settings.internet_enabled,
        internet_enabled: internet_enabled.clone(),
        bulletin_subscriptions: Vec::new(),
        crowd_mode: ui_settings.crowd_mode,
    });
    let (runner, handle) = NodeRunner::new(config).map_err(|e| e.to_string())?;
    let local_peer_id = runner.local_peer_id().to_string();
    let runner_task = tauri::async_runtime::spawn(async move {
        let _ = runner.run().await;
    });

    let app_storage =
        Arc::new(AppStorage::open(&format!("{db_path}/app")).map_err(|e| e.to_string())?);
    let app_store = Arc::new(
        mycena_app::miniapp::AppStore::open(&format!("{db_path}/miniapp"))
            .map_err(|e| e.to_string())?,
    );
    let coin_ledger =
        Arc::new(LocalLedger::open(&format!("{db_path}/coin")).map_err(|e| e.to_string())?);
    let identity_path = format!("{db_path}/identity");
    let coin_keypair =
        mycena_l1_facade::load_or_create_keypair(&identity_path).map_err(|e| e.to_string())?;
    let coin_addr = address_from_keypair(&coin_keypair);
    let coin_transport = Arc::new(DesktopCoinTransport {
        handle: handle.clone(),
    });
    let coin_node = Arc::new(CoinNode::new(
        coin_ledger,
        coin_transport,
        coin_addr,
        local_peer_id.clone(),
        identity_path,
    ));

    let enc_keypair = mycena_l0_facade::secrets::load_or_create_enc_keypair(&db_path, None)
        .map_err(|e| e.to_string())?;

    let (app_node, inbox) = AppNode::new(
        handle.clone(),
        local_peer_id.clone(),
        display_name,
        app_storage.clone(),
        Arc::new(DesktopNotifier { app: app.clone() }),
        Some(coin_node.clone()),
        Some(app_store.clone()),
        enc_keypair,
    );
    let app_node = Arc::new(app_node);
    app_node.clone().start_incoming_task();
    app_node.clone().start_peer_contact_sync();
    if ui_settings.listen_sos {
        let _ = app_node.set_listen_sos(true).await;
    }
    if let Some(ref gh) = ui_settings.local_geohash_prefix {
        if !gh.trim().is_empty() {
            let _ = app_node.set_local_geohash_prefix(Some(gh.trim())).await;
        }
    }
    setup_inbox_emitter(app, inbox);

    let user_trust = Arc::new(UserTrustStore::open(&db_path).map_err(|e| e.to_string())?);
    let ut_handle = handle.clone();
    let ut_store = user_trust.clone();
    let user_trust_task = tauri::async_runtime::spawn(async move {
        let mut sync = DesktopUserTrustSync {
            messages_forwarded: 0,
            messages_delivered_local: 0,
        };
        push_desktop_endorsements(&ut_handle, &ut_store).await;
        let mut metrics_tick = tokio::time::interval(std::time::Duration::from_secs(30));
        let mut uptime_tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = metrics_tick.tick() => {
                    sync_desktop_user_trust(&ut_handle, &ut_store, &mut sync).await;
                }
                _ = uptime_tick.tick() => {
                    let _ = ut_store.tick_uptime(wall_now_ms(), 60);
                }
            }
        }
    });

    let discovery = discovery::MeshDiscoveryState::new();
    discovery
        .set_ble_bootstrap_enabled(ui_settings.ble_bootstrap)
        .await;

    let ble_host = ble::DesktopBleHost::start(
        handle.clone(),
        app_node.clone(),
        local_peer_id.clone(),
        ui_settings.ble_bootstrap,
        discovery.clone(),
    );

    let cap_mac_key = mycena_app::miniapp::mac_key_from_db_path(&db_path);
    *state.write().await = Some(AppState {
        handle,
        app_node,
        app_storage,
        app_store,
        coin_node,
        user_trust,
        user_trust_task,
        local_peer_id: local_peer_id.clone(),
        coin_identity_path: format!("{db_path}/identity"),
        db_path: db_path.clone(),
        cap_mac_key,
        energy_state: NodeState::Active,
        rendezvous_enabled: ui_settings.rendezvous_enabled,
        internet_enabled: ui_settings.internet_enabled,
        connectivity_mode: connectivity_mode.clone(),
        runner_task,
        ble_host,
        discovery,
    });

    Ok(local_peer_id)
}

#[tauri::command]
async fn get_peers(state: State<'_, SharedState>) -> Result<Vec<String>, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(vec![]);
    };
    Ok(s.handle.connected_peers().await)
}

#[tauri::command]
async fn get_relay_status() -> Result<serde_json::Value, String> {
    let status =
        tokio::task::spawn_blocking(|| mycena_l0_facade::relay_status::fetch_relay_status(None))
            .await
            .map_err(|e| e.to_string())?;
    serde_json::to_value(status).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_shareable_multiaddrs(state: State<'_, SharedState>) -> Result<Vec<String>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let listen = s.handle.listen_addrs().await;
    let direct = s.handle.stable_direct_multiaddr(&s.local_peer_id).await;
    let signed_invite = s
        .handle
        .encode_signed_invite_v3(&s.local_peer_id, direct.as_deref())
        .await
        .ok();
    Ok(mycena_l0_facade::bootstrap::shareable_dial_multiaddrs(
        &s.local_peer_id,
        &s.handle.local_enc_pubkey_hex(),
        &listen,
        signed_invite.as_deref(),
    ))
}

#[tauri::command]
async fn connect_peer_id(
    app: AppHandle,
    state: State<'_, SharedState>,
    peer_id: String,
) -> Result<(), String> {
    let discovery = {
        let guard = state.read().await;
        let s = guard
            .as_ref()
            .ok_or_else(|| "node not started".to_string())?;
        s.discovery.set_connecting_from_invite(true).await;
        s.discovery.clone()
    };
    let discovery_timeout = discovery.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(45)).await;
        discovery_timeout.set_connecting_from_invite(false).await;
    });

    let result = {
        let guard = state.read().await;
        let s = guard
            .as_ref()
            .ok_or_else(|| "node not started".to_string())?;
        s.app_node
            .clone()
            .connect_from_invite(&peer_id)
            .await
            .map_err(|e| e.to_string())
    };

    discovery.set_connecting_from_invite(false).await;

    result?;
    let _ = app.emit("contacts-updated", ());
    Ok(())
}

#[tauri::command]
async fn get_mesh_discovery_snapshot(
    state: State<'_, SharedState>,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let connected = s.handle.connected_peers().await.len() as u32;
    s.discovery.set_connected_peer_count(connected).await;
    let snap = s.discovery.snapshot().await;
    serde_json::to_value(snap).map_err(|e| e.to_string())
}

fn contact_json(c: Contact) -> serde_json::Value {
    let status = match c.status {
        ContactStatus::Pending => "pending",
        ContactStatus::Accepted => "accepted",
        ContactStatus::Blocked => "blocked",
    };
    json!({
        "peer_id": c.peer_id,
        "display_name": c.display_name,
        "added_at_ms": c.added_at_ms,
        "status": status,
        "account_id": c.account_id,
    })
}

/// Chat sidebar: accepted contacts only (never connected peers — use Connect tab for those).
#[tauri::command]
async fn get_chat_contacts(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    list_accepted_contacts(state).await
}

#[tauri::command]
async fn list_contacts(state: State<'_, SharedState>) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node
        .list_contacts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(contact_json)
        .collect())
}

#[tauri::command]
async fn list_accepted_contacts(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let contacts = s
        .app_node
        .list_accepted_contacts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|c| mycena_l0_facade::bootstrap::is_mesh_peer_id(&c.peer_id))
        .collect::<Vec<_>>();
    Ok(contacts.into_iter().map(contact_json).collect())
}

#[tauri::command]
async fn list_pending_contacts(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node
        .list_pending_contacts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(contact_json)
        .collect())
}

#[tauri::command]
async fn add_contact(
    app: AppHandle,
    state: State<'_, SharedState>,
    peer_id: String,
    display_name: String,
    accepted: bool,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let c = s
        .app_node
        .add_contact(&peer_id, &display_name, accepted)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("contacts-updated", ());
    Ok(contact_json(c))
}

#[tauri::command]
async fn accept_contact(
    app: AppHandle,
    state: State<'_, SharedState>,
    peer_id: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let c = s
        .app_node
        .accept_contact(&peer_id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("contacts-updated", ());
    Ok(contact_json(c))
}

#[tauri::command]
async fn reject_contact(
    app: AppHandle,
    state: State<'_, SharedState>,
    peer_id: String,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .reject_contact(&peer_id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("contacts-updated", ());
    Ok(())
}

#[tauri::command]
async fn block_contact(
    app: AppHandle,
    state: State<'_, SharedState>,
    peer_id: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let c = s
        .app_node
        .block_contact(&peer_id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("contacts-updated", ());
    Ok(contact_json(c))
}

#[tauri::command]
async fn unblock_contact(
    app: AppHandle,
    state: State<'_, SharedState>,
    peer_id: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let c = s
        .app_node
        .unblock_contact(&peer_id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("contacts-updated", ());
    Ok(contact_json(c))
}

#[tauri::command]
async fn get_peer_reputation(
    state: State<'_, SharedState>,
    peer_id: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let rep = s.handle.peer_reputation(&peer_id).await;
    Ok(match rep {
        Some(r) => json!({
            "strikes": r.strikes,
            "last_strike_ms": r.last_strike_ms,
            "throttled_until_ms": r.throttled_until_ms,
            "reliability": r.reliability,
            "spam_score": r.spam_score,
            "relay_success_rate": r.relay_success_rate,
            "age_ms": r.age_ms,
            "mutual_contacts": r.mutual_contacts,
        }),
        None => json!({
            "strikes": 0,
            "last_strike_ms": 0,
            "throttled_until_ms": 0,
            "reliability": 0.25,
            "spam_score": 0.0,
            "relay_success_rate": 0.25,
            "age_ms": 0,
            "mutual_contacts": 0,
        }),
    })
}

#[tauri::command]
async fn post_sos(
    state: State<'_, SharedState>,
    geohash: String,
    message: String,
    ttl_secs: u64,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .post_sos(&geohash, &message, ttl_secs)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_sos_posts(state: State<'_, SharedState>) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node
        .list_sos_posts_with_trust()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(p, trust)| {
            json!({
                "geohash": p.geohash,
                "message": p.message,
                "created_at_ms": p.created_at_ms,
                "trust": trust_json(&trust),
            })
        })
        .collect())
}

#[tauri::command]
async fn set_listen_sos(state: State<'_, SharedState>, enabled: bool) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .set_listen_sos(enabled)
        .await
        .map_err(|e| e.to_string())?;
    let mut ui = load_ui_settings(&s.db_path);
    ui.listen_sos = enabled;
    save_ui_settings(&s.db_path, &ui)?;
    Ok(())
}

#[tauri::command]
async fn set_crisis_manual_mode(state: State<'_, SharedState>, mode: u8) -> Result<(), String> {
    use mycena_l0_facade::crisis_mode::CrisisManualMode;
    let manual = match mode {
        1 => CrisisManualMode::ForceOn,
        2 => CrisisManualMode::ForceOff,
        _ => CrisisManualMode::FollowAuto,
    };
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .set_crisis_manual(manual)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn is_crisis_mode_active(state: State<'_, SharedState>) -> Result<bool, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node.is_crisis_active().await)
}

#[tauri::command]
async fn refresh_crisis_auto(
    state: State<'_, SharedState>,
    internet_reachable: bool,
    local_geohash_prefix: Option<String>,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .refresh_crisis_auto(internet_reachable, local_geohash_prefix.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_local_geohash_prefix(
    state: State<'_, SharedState>,
    prefix: Option<String>,
) -> Result<(), String> {
    let mut guard = state.write().await;
    let s = guard
        .as_mut()
        .ok_or_else(|| "node not started".to_string())?;
    let normalized = prefix.filter(|p| !p.trim().is_empty());
    s.app_node
        .set_local_geohash_prefix(normalized.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let mut ui = load_ui_settings(&s.db_path);
    ui.local_geohash_prefix = normalized;
    save_ui_settings(&s.db_path, &ui)?;
    Ok(())
}

#[tauri::command]
async fn post_resource_map(
    state: State<'_, SharedState>,
    geohash: String,
    category: String,
    description: String,
    ttl_secs: u64,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .post_resource_map(&geohash, &category, &description, ttl_secs)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_resource_map_entries(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node
        .list_resource_map_entries_with_trust()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(e, trust)| {
            json!({
                "geohash": e.geohash,
                "category": e.category.as_str(),
                "description": e.description,
                "created_at_ms": e.created_at_ms,
                "ttl_secs": e.ttl_secs,
                "expires_at_ms": e.expires_at_ms(),
                "trust": trust_json(&trust),
            })
        })
        .collect())
}

#[tauri::command]
async fn post_volunteer_task(
    state: State<'_, SharedState>,
    geohash: String,
    category: String,
    description: String,
    slots_needed: u32,
    ttl_secs: u64,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let task = s
        .app_node
        .post_volunteer_task(&geohash, &category, &description, slots_needed, ttl_secs)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "task_id": task.task_id,
        "geohash": task.geohash,
        "category": task.category.as_str(),
        "description": task.description,
        "slots_needed": task.slots_needed,
        "slots_open": task.slots_open(),
        "claims_count": task.claims.len(),
    }))
}

#[tauri::command]
async fn list_volunteer_tasks(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node
        .list_volunteer_tasks()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|task| {
            json!({
                "task_id": task.task_id,
                "geohash": task.geohash,
                "category": task.category.as_str(),
                "description": task.description,
                "slots_needed": task.slots_needed,
                "slots_open": task.slots_open(),
                "claims_count": task.claims.len(),
                "created_at_ms": task.created_at_ms,
                "expires_at_ms": task.expires_at_ms(),
            })
        })
        .collect())
}

#[tauri::command]
async fn claim_volunteer_task(
    state: State<'_, SharedState>,
    task_id: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let task = s
        .app_node
        .claim_volunteer_task(&task_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "task_id": task.task_id,
        "slots_open": task.slots_open(),
        "claims_count": task.claims.len(),
    }))
}

fn trust_json(trust: &mycena_l0_facade::information_trust::InformationTrust) -> serde_json::Value {
    json!({
        "corroboration_count": trust.corroboration_count,
        "trusted_corroboration_count": trust.trusted_corroboration_count,
        "source_diversity": trust.source_diversity,
        "geographic_consistency": trust.geographic_consistency,
        "staleness_factor": trust.staleness_factor,
        "trust_score": trust.trust_score,
        "status": format!("{:?}", trust.status).to_ascii_lowercase(),
        "display_label": trust.display_label(),
    })
}

#[tauri::command]
async fn user_trust_summary(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let summary = s.user_trust.snapshot();
    Ok(json!({
        "relay_contribution": summary.relay_contribution,
        "uptime_secs": summary.uptime_secs,
        "successful_deliveries": summary.successful_deliveries,
        "local_endorsements": summary.local_endorsements,
        "contribution_score": summary.contribution_score,
        "contribution_tier": summary.contribution_tier,
    }))
}

#[tauri::command]
async fn get_metrics(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(json!({}));
    };
    let m = s.handle.metrics().await;
    Ok(json!({
        "messages_forwarded": m.messages_forwarded,
        "messages_dropped_ttl": m.messages_dropped_ttl,
        "messages_dropped_hops": m.messages_dropped_hops,
        "messages_dropped_queue": m.messages_dropped_queue,
        "messages_dropped_invalid_sig": m.messages_dropped_invalid_sig,
        "messages_dropped_no_sig": m.messages_dropped_no_sig,
        "messages_delivered_local": m.messages_delivered_local,
        "pending_queue_size": m.pending_queue_size,
        "seen_cache_size": m.seen_cache_size,
        "connected_peers": m.connected_peers,
        "max_peers": m.max_peers,
        "crisis_active": m.crisis_active,
        "crowd_density_active": m.crowd_density_active,
        "crisis_dropped_background_gossip": m.crisis_dropped_background_gossip,
        "crisis_dropped_background_forward": m.crisis_dropped_background_forward,
        "crisis_dropped_background_sync": m.crisis_dropped_background_sync,
        "webrtc_links_active": m.webrtc_links_active,
        "webrtc_host_available": m.webrtc_host_available,
        "wifi_aware_available": m.wifi_aware_available,
        "discovery_events_wifi_aware": m.discovery_events_wifi_aware,
        "discovery_events_ble": m.discovery_events_ble,
        "prefer_wifi_aware_discovery": m.prefer_wifi_aware_discovery,
    }))
}

#[tauri::command]
async fn send_chat(
    state: State<'_, SharedState>,
    to_peer: String,
    body: String,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .send_chat(Some(to_peer), body)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn peer_has_enc_key(state: State<'_, SharedState>, peer_id: String) -> Result<bool, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.handle.has_enc_key_for(&peer_id).await)
}

#[tauri::command]
async fn send_chat_encrypted(
    state: State<'_, SharedState>,
    to_peer: String,
    body: String,
) -> Result<String, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .send_chat_encrypted(to_peer, body, mycena_l0_facade::data::Priority::Normal)
        .await
        .map(|_| "encrypted".to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_pending_custody(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(vec![]);
    };
    Ok(s.handle
        .pending_custody_statuses()
        .await
        .into_iter()
        .filter_map(|p| serde_json::to_value(p).ok())
        .collect())
}

#[tauri::command]
async fn chat_history(
    state: State<'_, SharedState>,
    peer_id: String,
    limit: u32,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(vec![]);
    };
    let my_name = s.app_node.display_name().await;
    Ok(s.app_storage
        .chat_history(&peer_id, limit as usize)
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id.to_string(),
                "from_display_name": m.from_display_name,
                "body": m.body,
                "timestamp_ms": m.timestamp_ms,
                "is_mine": m.from_display_name == my_name,
            })
        })
        .collect())
}

#[tauri::command]
async fn send_mail(
    state: State<'_, SharedState>,
    to_peer: String,
    subject: String,
    body: String,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .send_mail(to_peer, subject, body, vec![])
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn mail_inbox(
    state: State<'_, SharedState>,
    limit: u32,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(vec![]);
    };
    Ok(s.app_storage
        .inbox(limit as usize)
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id.to_string(),
                "from_peer": m.from_peer,
                "from_display_name": m.from_display_name,
                "subject": m.subject,
                "body": m.body,
                "timestamp_ms": m.timestamp_ms,
                "is_read": s.app_storage.is_read(&m.id).unwrap_or(false),
            })
        })
        .collect())
}

#[tauri::command]
async fn wallet_balance(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(json!({"confirmed_muon": 0u64, "pending_muon": 0u64}));
    };
    let (confirmed, pending) = s
        .coin_node
        .balance_eventual()
        .map(|v| (v.confirmed_muon(), v.pending_muon()))
        .unwrap_or((0, 0));
    let settled = s
        .coin_node
        .balance_settled()
        .map(|v| v.spendable_muon())
        .unwrap_or(0);
    Ok(json!({
        "confirmed_muon": confirmed,
        "pending_muon": pending,
        "settled_muon": settled,
    }))
}

#[tauri::command]
async fn wallet_address(state: State<'_, SharedState>) -> Result<String, String> {
    let guard = state.read().await;
    Ok(guard
        .as_ref()
        .map(|s| s.coin_node.local_address().to_string())
        .unwrap_or_default())
}

#[tauri::command]
async fn subscribe_bulletin_scope(
    state: State<'_, SharedState>,
    scope: String,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .subscribe_bulletin_scope(scope)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn post_bulletin(
    state: State<'_, SharedState>,
    scope: String,
    title: String,
    body: String,
    ttl_secs: u64,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .post_bulletin(scope, title, body, ttl_secs, None)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn bulletins_for_scope(
    state: State<'_, SharedState>,
    scope: String,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(vec![]);
    };
    Ok(s.app_storage
        .bulletins_for_scope(&scope)
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            json!({
                "id": p.id.to_string(),
                "from_display_name": p.from_display_name,
                "title": p.title,
                "body": p.body,
                "scope": p.scope,
                "timestamp_ms": p.timestamp_ms,
                "expires_at_ms": p.expires_at_ms,
            })
        })
        .collect())
}

#[tauri::command]
async fn add_peer(state: State<'_, SharedState>, multiaddr: String) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .send(NodeCommand::AddBootstrapPeer { multiaddr })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_groups(state: State<'_, SharedState>) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let groups = s
        .app_node
        .storage()
        .all_groups()
        .map_err(|e| e.to_string())?;
    Ok(groups
        .into_iter()
        .map(|g| {
            json!({
                "id": g.id,
                "name": g.name,
                "member_count": g.members.len(),
                "created_at_ms": g.created_at_ms,
            })
        })
        .collect())
}

#[tauri::command]
async fn create_group(
    state: State<'_, SharedState>,
    name: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let g = Group::new(name);
    s.app_node
        .storage()
        .save_group(&g)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "id": g.id,
        "name": g.name,
        "member_count": g.members.len(),
        "created_at_ms": g.created_at_ms,
    }))
}

#[tauri::command]
async fn import_group_invite(
    state: State<'_, SharedState>,
    json_str: String,
) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let g = Group::from_invite(&json_str).map_err(|e| e.to_string())?;
    s.app_node
        .storage()
        .save_group(&g)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "id": g.id,
        "name": g.name,
        "member_count": g.members.len(),
        "created_at_ms": g.created_at_ms,
    }))
}

#[tauri::command]
async fn export_group_invite(
    state: State<'_, SharedState>,
    group_id: String,
) -> Result<String, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    Ok(s.app_node
        .storage()
        .group_by_id(&group_id)
        .map_err(|e| e.to_string())?
        .map(|g| g.export_invite())
        .unwrap_or_default())
}

#[tauri::command]
async fn send_group_message(
    state: State<'_, SharedState>,
    group_id: String,
    body: String,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let g = s
        .app_node
        .storage()
        .group_by_id(&group_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "unknown group".to_string())?;
    s.app_node
        .send_group_message(&g, body)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_group(state: State<'_, SharedState>, group_id: String) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node
        .storage()
        .delete_group(&group_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_chat_history(
    state: State<'_, SharedState>,
    group_id: String,
    limit: u32,
) -> Result<Vec<serde_json::Value>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let gid = format!("group:{group_id}");
    let messages = s
        .app_node
        .storage()
        .group_chat_history(&group_id, limit as usize)
        .map_err(|e| e.to_string())?;
    Ok(messages
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id.to_string(),
                "from_peer": &gid,
                "from_display_name": m.from_display_name,
                "body": m.body,
                "timestamp_ms": m.timestamp_ms,
            })
        })
        .collect())
}

#[tauri::command]
async fn wallet_send(
    state: State<'_, SharedState>,
    to_address: String,
    amount_muon: u64,
    fee_muon: u64,
    memo: Option<String>,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.coin_node
        .submit_transfer_from_identity_path(
            &s.coin_identity_path,
            to_address,
            amount_muon,
            fee_muon,
            memo,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_list_installed(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let apps = s.app_store.installed_apps().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for m in apps {
        out.push(serde_json::to_value(&m).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn miniapp_trust_level_name(level: mycena_app::miniapp::InstallTrustLevel) -> &'static str {
    use mycena_app::miniapp::InstallTrustLevel;
    match level {
        InstallTrustLevel::VerifiedListing => "verified_listing",
        InstallTrustLevel::MatchingListingHash => "matching_listing_hash",
        InstallTrustLevel::SideloadOnly => "sideload_only",
        InstallTrustLevel::HashMismatch => "hash_mismatch",
    }
}

#[tauri::command]
async fn miniapp_preview_install(
    state: State<'_, SharedState>,
    bundle_base64: String,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(bundle_base64.trim())
        .map_err(|e| e.to_string())?;
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let p = s
        .app_store
        .preview_install(&bytes)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "manifest": p.manifest,
        "bundle_hash": p.bundle_hash,
        "trust_level": miniapp_trust_level_name(p.trust_level),
        "listing_signature_ok": p.listing_signature_ok,
        "installed_version": p.installed_version,
        "is_downgrade": p.is_downgrade,
        "has_inline_script": p.has_inline_script,
        "strict_csp_eligible": p.strict_csp_eligible,
        "reproducible_attested": p.reproducible_attested,
        "bundle_signature_ok": p.bundle_signature_ok,
    }))
}

#[tauri::command]
async fn miniapp_install(
    state: State<'_, SharedState>,
    bundle_base64: String,
    listing_app_id: Option<String>,
    allow_sideload: bool,
    allow_downgrade: bool,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    use mycena_app::miniapp::InstallTrust;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(bundle_base64.trim())
        .map_err(|e| e.to_string())?;
    let trust = if listing_app_id.is_some() {
        InstallTrust::VerifiedListing
    } else if allow_sideload {
        InstallTrust::SideloadAcknowledged
    } else {
        return Err("listing_app_id or allow_sideload required".into());
    };
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    if let Some(ref id) = listing_app_id {
        let preview = s
            .app_store
            .preview_install(&bytes)
            .map_err(|e| e.to_string())?;
        if preview.manifest.id != *id {
            return Err("bundle app id does not match listing_app_id".into());
        }
    }
    let m = s
        .app_store
        .install_verified(&bytes, trust, allow_downgrade)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(&m).map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_install_official(
    state: State<'_, SharedState>,
    app_id: String,
) -> Result<serde_json::Value, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let m = s
        .app_store
        .install_official_bundle(&app_id)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(&m).map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_uninstall(state: State<'_, SharedState>, app_id: String) -> Result<(), String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    s.app_store.uninstall(&app_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_browse_store(
    state: State<'_, SharedState>,
) -> Result<Vec<serde_json::Value>, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let rows = s.app_store.browse_listings().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for l in rows {
        let signature_valid = l.verify_signature().unwrap_or(false);
        out.push(serde_json::json!({
            "manifest": l.manifest,
            "bundle_hash": l.bundle_hash,
            "sources": l.sources,
            "published_at_ms": l.published_at_ms,
            "updated_at_ms": l.updated_at_ms,
            "signature": l.signature,
            "signature_valid": signature_valid,
        }));
    }
    Ok(out)
}

#[tauri::command]
async fn miniapp_get_runtime(
    state: State<'_, SharedState>,
    app_id: String,
) -> Result<String, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let m = s
        .app_store
        .get_manifest(&app_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "unknown app".to_string())?;
    Ok(m.guest_runtime().as_str().to_string())
}

#[tauri::command]
async fn miniapp_invoke_wasm_run(
    state: State<'_, SharedState>,
    app_id: String,
) -> Result<i32, String> {
    use mycena_app::miniapp::bridge_host::BridgeHost;
    use mycena_app::miniapp::wasm_bridge_host::run_wasm_guest_for_app;

    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let m = s
        .app_store
        .get_manifest(&app_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "unknown app".to_string())?;
    let enc = s.app_node.node_handle().local_enc_pubkey_hex();
    let safe_mode = s.app_store.effective_safe_mode(&app_id).unwrap_or(false);
    let host = BridgeHost::new(
        app_id.clone(),
        m.permissions,
        s.app_node.clone(),
        s.app_node.storage(),
        s.app_store.clone(),
        Some(s.coin_node.clone()),
        s.local_peer_id.clone(),
        enc,
        safe_mode,
        s.cap_mac_key,
    );
    run_wasm_guest_for_app(&app_id, &s.app_store, host)
        .await
        .map(|r| r.status)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_get_entry_html(
    state: State<'_, SharedState>,
    app_id: String,
    script_nonce: String,
) -> Result<String, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let m = s
        .app_store
        .get_manifest(&app_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "unknown app".to_string())?;
    let entry_path = if m.runtime == "wasm" {
        m.hybrid_ui_entry()
            .filter(|ui| s.app_store.get_file(&app_id, ui).ok().flatten().is_some())
            .unwrap_or(&m.entry)
    } else {
        &m.entry
    };
    if m.runtime == "wasm" && entry_path.ends_with(".wasm") {
        return Ok(format!(
            r#"<!DOCTYPE html><html><head>{csp}</head><body><p>WASM mini-app (logic-only). Guest module loaded in sandbox.</p></body></html>"#,
            csp = mycena_app::miniapp::csp::meta_tag(&script_nonce)
        ));
    }
    let bytes = s
        .app_store
        .get_file(&app_id, entry_path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "entry missing".to_string())?;
    let body = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let csp_meta = mycena_app::miniapp::csp::meta_tag(&script_nonce);
    let session = mycena_app::miniapp::issue_session(&app_id);
    let bridge = mycena_app::miniapp::bridge_api_js_source();
    let network_block = format!(
        r#"<script nonce="{script_nonce}">
window.fetch = function() {{ return Promise.reject(new Error('Network blocked')); }};
window.XMLHttpRequest = function() {{ throw new Error('Network blocked'); }};
window.WebSocket = function() {{ throw new Error('WebSocket blocked'); }};
if (navigator.sendBeacon) {{ navigator.sendBeacon = function() {{ return false; }}; }}
</script>"#
    );
    let shim = format!(
        r#"<script nonce="{script_nonce}">
window.__mycena_session = {session_json};
window.addEventListener("message", function (ev) {{
  if (ev.data && ev.data.__mycena_resolve && window.__mycena_resolve) {{
    var r = ev.data.__mycena_resolve;
    window.__mycena_resolve(r.id, r.result, r.error);
  }}
}});
window.__mycena_native_call = function (json) {{
  window.parent.postMessage({{ __mycena_call: json }}, window.location.origin);
}};
</script><script nonce="{script_nonce}">
"#,
        session_json = serde_json::to_string(&session).map_err(|e| e.to_string())?
    );
    Ok(format!(
        "{csp_meta}\n{network_block}\n{shim}{bridge}</script>\n{body}"
    ))
}

#[tauri::command]
fn miniapp_revoke_bridge_session(app_id: String) {
    mycena_app::miniapp::revoke_session(&app_id);
}

#[tauri::command]
async fn miniapp_issue_capability(
    state: State<'_, SharedState>,
    app_id: String,
    permission: String,
    session_token: String,
) -> Result<String, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let perm = mycena_app::miniapp::parse_permission_name(&permission)
        .ok_or_else(|| format!("unknown permission: {permission}"))?;
    Ok(mycena_app::miniapp::issue_capability(
        &s.cap_mac_key,
        &app_id,
        &perm,
        &session_token,
        mycena_app::miniapp::DEFAULT_CAP_TTL_MS,
    ))
}

#[tauri::command]
async fn miniapp_publish_revocation(
    state: State<'_, SharedState>,
    app_id: String,
    reason: String,
) -> Result<(), String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let keypair = mycena_l1_facade::load_or_create_keypair(&s.coin_identity_path)
        .map_err(|e| e.to_string())?;
    let entry = s
        .app_store
        .build_revocation_gossip(&app_id, &reason, &s.local_peer_id, &keypair)
        .map_err(|e| e.to_string())?;
    s.app_node
        .publish_app_revocation(&entry)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_get_policy(
    state: State<'_, SharedState>,
    app_id: String,
) -> Result<serde_json::Value, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let p = s
        .app_store
        .policy_snapshot(&app_id)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "app_id": p.app_id,
        "reputation_score": p.reputation_score,
        "safe_mode_active": p.safe_mode_active,
        "safe_mode_forced": p.safe_mode_forced,
        "safe_mode_suggested": p.safe_mode_suggested,
        "user_safe_mode": p.user_safe_mode,
        "revoked": p.revoked,
        "strict_csp_eligible": p.strict_csp_eligible,
    }))
}

#[tauri::command]
async fn miniapp_set_safe_mode(
    state: State<'_, SharedState>,
    app_id: String,
    enabled: bool,
) -> Result<(), String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    s.app_store
        .set_user_safe_mode(&app_id, enabled)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_report_app(state: State<'_, SharedState>, app_id: String) -> Result<(), String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    s.app_store
        .record_user_report(&app_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn miniapp_bridge_call(
    state: State<'_, SharedState>,
    app_id: String,
    method: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let g = state.read().await;
    let s = g.as_ref().ok_or_else(|| "node not started".to_string())?;
    let manifest = s
        .app_store
        .get_manifest(&app_id)
        .map_err(|e| e.to_string())?;
    let permissions = manifest.map(|m| m.permissions).unwrap_or_default();
    let enc = s.handle.local_enc_pubkey_hex();
    let safe_mode = s.app_store.effective_safe_mode(&app_id).unwrap_or(false);
    let host = mycena_app::miniapp::bridge_host::BridgeHost::new(
        app_id,
        permissions,
        s.app_node.clone(),
        s.app_storage.clone(),
        s.app_store.clone(),
        Some(s.coin_node.clone()),
        s.local_peer_id.clone(),
        enc,
        safe_mode,
        s.cap_mac_key,
    );
    host.handle(&method, &args).await.map_err(|e| e.to_string())
}

fn node_state_label(state: NodeState) -> &'static str {
    match state {
        NodeState::Active => "Active",
        NodeState::Intermittent => "Intermittent",
        NodeState::Passive => "Passive",
    }
}

fn parse_energy_state(energy_state: &str) -> Result<NodeState, String> {
    match energy_state {
        "Active" => Ok(NodeState::Active),
        "Intermittent" => Ok(NodeState::Intermittent),
        "Passive" => Ok(NodeState::Passive),
        other => Err(format!("unknown energy state: {other}")),
    }
}

#[tauri::command]
async fn get_connectivity_mode(state: State<'_, SharedState>) -> Result<String, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok("unknown".into());
    };
    let mode = *s.connectivity_mode.read().await;
    Ok(connectivity_label(mode).to_string())
}

#[tauri::command]
async fn get_settings(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(json!({}));
    };
    let display_name = s.app_node.display_name().await;
    let connectivity = *s.connectivity_mode.read().await;
    let ui = load_ui_settings(&s.db_path);
    Ok(json!({
        "display_name": display_name,
        "energy_state": node_state_label(s.energy_state),
        "local_peer_id": s.local_peer_id,
        "account_id": s.handle.account_id(),
        "transport_epoch": s.handle.transport_epoch(),
        "rendezvous_enabled": s.rendezvous_enabled,
        "internet_enabled": s.internet_enabled,
        "max_connectivity": ui.max_connectivity,
        "listen_sos": ui.listen_sos,
        "crowd_mode": ui.crowd_mode,
        "local_geohash_prefix": ui.local_geohash_prefix,
        "ble_bootstrap": ui.ble_bootstrap,
        "connectivity_mode": connectivity_label(connectivity),
        "connectivity_detail": connectivity_detail(s.internet_enabled, s.rendezvous_enabled, connectivity),
    }))
}

#[tauri::command]
async fn set_rendezvous_enabled(
    app: AppHandle,
    state: State<'_, SharedState>,
    enabled: bool,
) -> Result<(), String> {
    let snapshot = {
        let mut guard = state.write().await;
        let s = guard
            .as_mut()
            .ok_or_else(|| "node not started".to_string())?;
        if !s.internet_enabled && enabled {
            return Err("Enable internet connectivity before relay discovery".into());
        }
        s.handle
            .send(NodeCommand::SetRendezvousEnabled(enabled))
            .await
            .map_err(|e| e.to_string())?;
        s.rendezvous_enabled = enabled;
        let mut ui = load_ui_settings(&s.db_path);
        ui.rendezvous_enabled = enabled;
        save_ui_settings(&s.db_path, &ui)?;
        connectivity_snapshot(s).await
    };
    emit_settings_updated(&app, snapshot);
    Ok(())
}

#[tauri::command]
async fn set_internet_enabled(
    app: AppHandle,
    state: State<'_, SharedState>,
    enabled: bool,
) -> Result<(), String> {
    let snapshot = {
        let mut guard = state.write().await;
        let s = guard
            .as_mut()
            .ok_or_else(|| "node not started".to_string())?;
        s.handle
            .send(NodeCommand::SetInternetEnabled(enabled))
            .await
            .map_err(|e| e.to_string())?;
        s.internet_enabled = enabled;
        if !enabled {
            s.rendezvous_enabled = false;
        }
        let mut ui = load_ui_settings(&s.db_path);
        ui.internet_enabled = enabled;
        if !enabled {
            ui.rendezvous_enabled = false;
            ui.max_connectivity = false;
        }
        save_ui_settings(&s.db_path, &ui)?;
        connectivity_snapshot(s).await
    };
    emit_settings_updated(&app, snapshot);
    Ok(())
}

#[tauri::command]
async fn set_max_connectivity(
    app: AppHandle,
    state: State<'_, SharedState>,
    enabled: bool,
) -> Result<(), String> {
    let snapshot = {
        let mut guard = state.write().await;
        let s = guard
            .as_mut()
            .ok_or_else(|| "node not started".to_string())?;
        let mut ui = load_ui_settings(&s.db_path);
        ui.max_connectivity = enabled;
        if enabled {
            s.handle
                .send(NodeCommand::SetInternetEnabled(true))
                .await
                .map_err(|e| e.to_string())?;
            s.handle
                .send(NodeCommand::SetRendezvousEnabled(true))
                .await
                .map_err(|e| e.to_string())?;
            s.internet_enabled = true;
            s.rendezvous_enabled = true;
            ui.internet_enabled = true;
            ui.rendezvous_enabled = true;
        }
        save_ui_settings(&s.db_path, &ui)?;
        connectivity_snapshot(s).await
    };
    emit_settings_updated(&app, snapshot);
    Ok(())
}

#[tauri::command]
async fn set_crowd_mode(
    app: AppHandle,
    state: State<'_, SharedState>,
    enabled: bool,
) -> Result<(), String> {
    let snapshot = {
        let mut guard = state.write().await;
        let s = guard
            .as_mut()
            .ok_or_else(|| "node not started".to_string())?;
        s.handle
            .send(NodeCommand::SetCrowdMode(enabled))
            .await
            .map_err(|e| e.to_string())?;
        let mut ui = load_ui_settings(&s.db_path);
        ui.crowd_mode = enabled;
        save_ui_settings(&s.db_path, &ui)?;
        connectivity_snapshot(s).await
    };
    emit_settings_updated(&app, snapshot);
    Ok(())
}

#[tauri::command]
async fn set_ble_bootstrap(
    app: AppHandle,
    state: State<'_, SharedState>,
    enabled: bool,
) -> Result<(), String> {
    let snapshot = {
        let mut guard = state.write().await;
        let s = guard
            .as_mut()
            .ok_or_else(|| "node not started".to_string())?;
        let mut ui = load_ui_settings(&s.db_path);
        ui.ble_bootstrap = enabled;
        save_ui_settings(&s.db_path, &ui)?;
        s.discovery.set_ble_bootstrap_enabled(enabled).await;
        if let Some(host) = s.ble_host.take() {
            host.stop_and_wait().await;
        }
        s.ble_host = ble::DesktopBleHost::start(
            s.handle.clone(),
            s.app_node.clone(),
            s.local_peer_id.clone(),
            enabled,
            s.discovery.clone(),
        );
        connectivity_snapshot(s).await
    };
    emit_settings_updated(&app, snapshot);
    Ok(())
}

#[tauri::command]
async fn webrtc_set_available(
    state: State<'_, SharedState>,
    available: bool,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .send(NodeCommand::SetWebRtcAvailable(available))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn webrtc_set_link_ready(
    state: State<'_, SharedState>,
    peer: String,
    ready: bool,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .send(NodeCommand::SetWebRtcLinkReady { peer, ready })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn webrtc_ingest_frame(
    state: State<'_, SharedState>,
    from_peer: String,
    frame: Vec<u8>,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .send(NodeCommand::IngestWebRtcFrame { from_peer, frame })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn webrtc_post_signal(
    state: State<'_, SharedState>,
    to_peer: String,
    session_id: String,
    kind: u8,
    payload: String,
    ice_mline_index: Option<u32>,
) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .send(NodeCommand::PostWebRtcSignal {
            to_peer,
            session_id,
            kind,
            payload,
            ice_mline_index,
        })
        .await
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct WebRtcSignalJson {
    from_peer: String,
    session_id: String,
    kind: u8,
    payload: String,
    ice_mline_index: Option<u32>,
}

#[tauri::command]
async fn webrtc_drain_signaling_inbound(
    state: State<'_, SharedState>,
    max: u32,
) -> Result<Vec<WebRtcSignalJson>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    s.handle
        .send(NodeCommand::DrainWebRtcSignalingInbound {
            max: max as usize,
            reply: tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(rx
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|sig| WebRtcSignalJson {
            from_peer: sig.from_peer,
            session_id: sig.session_id,
            kind: sig.kind as u8,
            payload: sig.payload,
            ice_mline_index: sig.ice_mline_index,
        })
        .collect())
}

#[tauri::command]
async fn webrtc_drain_negotiation_requests(
    state: State<'_, SharedState>,
    max: u32,
) -> Result<Vec<String>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    s.handle
        .send(NodeCommand::DrainWebRtcNegotiationRequests {
            max: max as usize,
            reply: tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(rx.await.unwrap_or_default())
}

#[tauri::command]
fn webrtc_stun_servers() -> Vec<String> {
    mycena_l0_facade::webrtc_signaling::default_stun_servers()
}

#[derive(serde::Serialize)]
struct BearerOutboundJson {
    to_peer: String,
    frame: Vec<u8>,
    medium: u8,
}

#[tauri::command]
async fn webrtc_drain_bearer_outbound(
    state: State<'_, SharedState>,
    max: u32,
) -> Result<Vec<BearerOutboundJson>, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    s.handle
        .send(NodeCommand::DrainBearerOutbound {
            to_peer: None,
            max: max as usize,
            reply: Some(tx),
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(rx
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(to_peer, frame, medium)| BearerOutboundJson {
            to_peer,
            frame,
            medium: medium as u8,
        })
        .collect())
}

#[tauri::command]
async fn set_display_name(state: State<'_, SharedState>, name: String) -> Result<(), String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    s.app_node.set_display_name(name.clone()).await;
    let mut ui = load_ui_settings(&s.db_path);
    ui.display_name = Some(name);
    save_ui_settings(&s.db_path, &ui)?;
    Ok(())
}

#[tauri::command]
async fn set_energy_state(
    state: State<'_, SharedState>,
    energy_state: String,
) -> Result<(), String> {
    let ns = parse_energy_state(&energy_state)?;
    let mut guard = state.write().await;
    let s = guard
        .as_mut()
        .ok_or_else(|| "node not started".to_string())?;
    s.handle
        .send(NodeCommand::SetEnergyState(ns))
        .await
        .map_err(|e| e.to_string())?;
    s.energy_state = ns;
    Ok(())
}

#[tauri::command]
async fn get_store_stats(state: State<'_, SharedState>) -> Result<serde_json::Value, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(json!({"count": 0, "oldest_ms": 0}));
    };
    let stats = s.handle.store_stats().await.map_err(|e| e.to_string())?;
    Ok(json!({
        "count": stats.count,
        "oldest_ms": stats.oldest_ms,
    }))
}

#[tauri::command]
async fn run_gc(state: State<'_, SharedState>) -> Result<u64, String> {
    let guard = state.read().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| "node not started".to_string())?;
    let deleted = s.handle.gc_now().await.map_err(|e| e.to_string())?;
    Ok(deleted as u64)
}

#[tauri::command]
async fn get_custom_bootstrap_peers(state: State<'_, SharedState>) -> Result<Vec<String>, String> {
    let guard = state.read().await;
    let Some(s) = guard.as_ref() else {
        return Ok(vec![]);
    };
    Ok(load_custom_bootstrap_peers(&s.db_path))
}

#[tauri::command]
async fn set_custom_bootstrap_peers(
    state: State<'_, SharedState>,
    peers: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.write().await;
    let s = guard
        .as_mut()
        .ok_or_else(|| "node not started".to_string())?;
    save_custom_bootstrap_peers(&s.db_path, &peers).map_err(|e| e.to_string())?;
    for multiaddr in &peers {
        if let Err(e) = s
            .handle
            .send(NodeCommand::AddBootstrapPeer {
                multiaddr: multiaddr.clone(),
            })
            .await
        {
            tracing::warn!("failed to dial bootstrap peer {multiaddr}: {e}");
        }
    }
    Ok(())
}

#[tauri::command]
async fn get_share_status(
    share: State<'_, ShareServerState>,
) -> Result<share_server::ShareServerStatus, String> {
    Ok(share.status().await)
}

#[tauri::command]
async fn start_share_server(
    share: State<'_, ShareServerState>,
) -> Result<share_server::ShareServerStatus, String> {
    share.start().await?;
    Ok(share.status().await)
}

#[tauri::command]
async fn stop_share_server(
    share: State<'_, ShareServerState>,
) -> Result<share_server::ShareServerStatus, String> {
    share.stop().await;
    Ok(share.status().await)
}

#[tauri::command]
async fn choose_share_apk(
    app: AppHandle,
    share: State<'_, ShareServerState>,
) -> Result<share_server::ShareServerStatus, String> {
    use tauri_plugin_dialog::DialogExt;

    let picked = app
        .dialog()
        .file()
        .add_filter("Android APK", &["apk"])
        .blocking_pick_file();

    let Some(file_path) = picked else {
        return Ok(share.status().await);
    };

    let source = file_path.into_path().map_err(|e| e.to_string())?;
    ensure_share_dir()?;
    let dest = default_share_apk_path();
    std::fs::copy(&source, &dest).map_err(|e| format!("Could not copy APK: {e}"))?;
    share.set_apk_path(dest).await;
    Ok(share.status().await)
}

/// Immediately deletes all local data and restarts the app.
#[tauri::command]
async fn panic_wipe(app: AppHandle, state: State<'_, SharedState>) -> Result<(), String> {
    let active_path = {
        let guard = state.read().await;
        guard.as_ref().map(|s| s.db_path.clone())
    };

    shutdown_active_node(&state).await;
    wipe_all_local_data(active_path.as_deref())?;
    let default = default_db_path();
    if std::path::Path::new(&default).exists() {
        return Err(format!(
            "local data still present at {default} after wipe — close Mycena completely and retry"
        ));
    }
    app.restart();
}

fn setup_inbox_emitter(app: AppHandle, mut inbox: AppInbox) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                result = inbox.chat_rx.recv() => {
                    match result {
                        Ok(chat) => {
                            let _ = app.emit(
                                "chat-updated",
                                serde_json::json!({
                                    "from_peer": chat.from_peer,
                                    "body": chat.message.body,
                                }),
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = app.emit("chat-updated", ());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = inbox.mail_rx.recv() => {
                    match result {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = app.emit("mail-updated", ());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = inbox.bulletin_rx.recv() => {
                    match result {
                        Ok(post) => {
                            if post.scope == mycena_l0_facade::sos::SOS_SCOPE {
                                let _ = app.emit("sos-updated", ());
                            } else {
                                let _ = app.emit("bulletin-updated", ());
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = app.emit("bulletin-updated", ());
                            let _ = app.emit("sos-updated", ());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = inbox.appstore_rx.recv() => {
                    match result {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = app.emit("appstore-updated", ());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });
}

fn setup_event_emitter(app_handle: AppHandle, state: SharedState) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let guard = state.read().await;
            let Some(s) = guard.as_ref() else {
                continue;
            };
            let metrics = s.handle.metrics().await;
            let live_peers: Vec<String> = s.handle.connected_peers().await;
            let peers = live_peers.len();
            let connectivity = *s.connectivity_mode.read().await;
            let _ = app_handle.emit(
                "metrics-updated",
                json!({
                    "forwarded": metrics.messages_forwarded,
                    "queue": metrics.pending_queue_size,
                    "peers": peers,
                    "connectivity": connectivity_label(connectivity),
                    "internet_enabled": s.internet_enabled,
                    "rendezvous_enabled": s.rendezvous_enabled,
                    "connectivity_detail": connectivity_detail(
                        s.internet_enabled,
                        s.rendezvous_enabled,
                        connectivity,
                    ),
                }),
            );
        }
    });
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("mycena_desktop=info,mycena_node=info,mycena_app=info,libp2p_mdns=info")
        }))
        .with_target(true)
        .try_init();
}

pub fn run() {
    init_tracing();
    let shared_state: SharedState = Arc::new(RwLock::new(None));
    let _ = ensure_share_dir();
    let share_server: ShareServerState = Arc::new(ShareServer::new(default_share_apk_path()));

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(shared_state.clone())
        .manage(share_server)
        .setup(move |app| {
            setup_event_emitter(app.handle().clone(), shared_state.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_default_db_path,
            get_saved_profile,
            reset_local_data,
            return_to_onboarding,
            start_node,
            get_peers,
            get_relay_status,
            get_shareable_multiaddrs,
            get_mesh_discovery_snapshot,
            connect_peer_id,
            get_chat_contacts,
            list_contacts,
            list_accepted_contacts,
            list_pending_contacts,
            add_contact,
            accept_contact,
            reject_contact,
            block_contact,
            unblock_contact,
            get_peer_reputation,
            post_sos,
            list_sos_posts,
            set_listen_sos,
            set_crisis_manual_mode,
            is_crisis_mode_active,
            refresh_crisis_auto,
            set_local_geohash_prefix,
            post_resource_map,
            list_resource_map_entries,
            post_volunteer_task,
            list_volunteer_tasks,
            claim_volunteer_task,
            user_trust_summary,
            get_metrics,
            send_chat,
            peer_has_enc_key,
            send_chat_encrypted,
            list_pending_custody,
            chat_history,
            send_mail,
            mail_inbox,
            wallet_balance,
            wallet_address,
            wallet_send,
            post_bulletin,
            subscribe_bulletin_scope,
            bulletins_for_scope,
            add_peer,
            list_groups,
            create_group,
            import_group_invite,
            export_group_invite,
            send_group_message,
            delete_group,
            group_chat_history,
            miniapp_list_installed,
            miniapp_preview_install,
            miniapp_install,
            miniapp_install_official,
            miniapp_uninstall,
            miniapp_browse_store,
            miniapp_get_runtime,
            miniapp_invoke_wasm_run,
            miniapp_get_entry_html,
            miniapp_bridge_call,
            miniapp_revoke_bridge_session,
            miniapp_issue_capability,
            miniapp_publish_revocation,
            miniapp_get_policy,
            miniapp_set_safe_mode,
            miniapp_report_app,
            get_settings,
            get_connectivity_mode,
            set_display_name,
            set_energy_state,
            set_rendezvous_enabled,
            set_internet_enabled,
            set_max_connectivity,
            set_ble_bootstrap,
            webrtc_set_available,
            webrtc_set_link_ready,
            webrtc_ingest_frame,
            webrtc_post_signal,
            webrtc_drain_signaling_inbound,
            webrtc_drain_negotiation_requests,
            webrtc_stun_servers,
            webrtc_drain_bearer_outbound,
            set_crowd_mode,
            get_store_stats,
            run_gc,
            get_custom_bootstrap_peers,
            set_custom_bootstrap_peers,
            get_share_status,
            start_share_server,
            stop_share_server,
            choose_share_apk,
            panic_wipe,
        ])
        .run(tauri::generate_context!())
        // INVARIANT: process-fatal if the native window cannot start.
        .unwrap_or_else(|e| panic!("error running tauri application: {e}"));
}
