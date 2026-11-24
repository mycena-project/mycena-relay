// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! BLE / bearer discovery signals for the Connect tab (Android `MeshDiscoveryState` parity).

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const NEARBY_RECENT_MS: u64 = 90_000;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshDiscoverySnapshot {
    pub ble_bootstrap_enabled: bool,
    pub ble_permissions_granted: bool,
    pub scan_active: bool,
    pub scan_epoch_started_at_ms: u64,
    pub last_scan_restart_at_ms: u64,
    pub last_nearby_seen_at_ms: Option<u64>,
    pub nearby_device_count: u32,
    pub connected_peer_count: u32,
    pub connecting_from_invite: bool,
    pub now_ms: u64,
}

struct Inner {
    snapshot: MeshDiscoverySnapshot,
    nearby_ids: HashMap<String, u64>,
}

pub struct MeshDiscoveryState {
    inner: RwLock<Inner>,
}

impl MeshDiscoveryState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(Inner {
                snapshot: default_snapshot(),
                nearby_ids: HashMap::new(),
            }),
        })
    }

    pub async fn snapshot(&self) -> MeshDiscoverySnapshot {
        let mut guard = self.inner.write().await;
        let now = wall_now_ms();
        guard.snapshot.now_ms = now;
        prune_nearby(&mut guard, now);
        guard.snapshot.clone()
    }

    pub async fn on_ble_bootstrap_started(&self) {
        let now = wall_now_ms();
        let mut guard = self.inner.write().await;
        guard.snapshot.ble_bootstrap_enabled = true;
        guard.snapshot.ble_permissions_granted = true;
        guard.snapshot.scan_active = true;
        guard.snapshot.scan_epoch_started_at_ms = now;
        guard.snapshot.last_scan_restart_at_ms = now;
        guard.snapshot.now_ms = now;
    }

    pub async fn on_ble_bootstrap_stopped(&self) {
        let now = wall_now_ms();
        let mut guard = self.inner.write().await;
        guard.snapshot.ble_bootstrap_enabled = false;
        guard.snapshot.scan_active = false;
        guard.snapshot.nearby_device_count = 0;
        guard.snapshot.last_nearby_seen_at_ms = None;
        guard.nearby_ids.clear();
        guard.snapshot.now_ms = now;
    }

    pub async fn set_ble_bootstrap_enabled(&self, enabled: bool) {
        let mut guard = self.inner.write().await;
        guard.snapshot.ble_bootstrap_enabled = enabled;
        guard.snapshot.now_ms = wall_now_ms();
    }

    pub async fn on_nearby_sighting(&self, device_id: &str) {
        let now = wall_now_ms();
        let mut guard = self.inner.write().await;
        guard.nearby_ids.insert(device_id.to_string(), now);
        guard.snapshot.last_nearby_seen_at_ms = Some(now);
        prune_nearby(&mut guard, now);
        guard.snapshot.now_ms = now;
    }

    pub async fn set_connected_peer_count(&self, count: u32) {
        let mut guard = self.inner.write().await;
        guard.snapshot.connected_peer_count = count;
        guard.snapshot.now_ms = wall_now_ms();
        if count > 0 {
            guard.snapshot.connecting_from_invite = false;
        }
    }

    pub async fn set_connecting_from_invite(&self, connecting: bool) {
        let mut guard = self.inner.write().await;
        guard.snapshot.connecting_from_invite = connecting;
        guard.snapshot.now_ms = wall_now_ms();
    }
}

fn prune_nearby(inner: &mut Inner, now_ms: u64) {
    inner
        .nearby_ids
        .retain(|_, seen| now_ms.saturating_sub(*seen) <= NEARBY_RECENT_MS);
    inner.snapshot.nearby_device_count = inner.nearby_ids.len() as u32;
}

fn default_snapshot() -> MeshDiscoverySnapshot {
    MeshDiscoverySnapshot {
        ble_bootstrap_enabled: false,
        ble_permissions_granted: true,
        scan_active: false,
        scan_epoch_started_at_ms: 0,
        last_scan_restart_at_ms: 0,
        last_nearby_seen_at_ms: None,
        nearby_device_count: 0,
        connected_peer_count: 0,
        connecting_from_invite: false,
        now_ms: wall_now_ms(),
    }
}

fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
