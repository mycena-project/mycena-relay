// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! macOS / Linux BLE host bridge (Phase 1b — ADR 003).
//!
//! Uses [`blew`] for central (F1 scan/read, F2/F3 relay) and peripheral (GATT invite + mesh data).
//! Android parity is in Kotlin; this module calls the same `NodeCommand` surface as FFI.

mod bridge;
mod gatt;
mod link_registry;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod host;

use crate::discovery::MeshDiscoveryState;
use mycena_app::node::AppNode;
use mycena_l1_facade::NodeHandle;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Background BLE bootstrap host for desktop.
pub struct DesktopBleHost {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl DesktopBleHost {
    /// Spawn BLE central + peripheral loops, or log when unsupported/disabled.
    pub fn start(
        handle: NodeHandle,
        app_node: Arc<AppNode>,
        local_peer_id: String,
        enabled: bool,
        discovery: Arc<MeshDiscoveryState>,
    ) -> Option<Self> {
        if !enabled {
            return None;
        }

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let links = Arc::new(link_registry::LinkRegistry::default());
            let ctx = host::BleHostContext {
                handle,
                app_node,
                local_peer_id,
                links,
                discovery,
            };
            let task = tauri::async_runtime::spawn(async move {
                host::run(ctx, shutdown_rx).await;
            });
            info!("desktop BLE host started (blew central + peripheral)");
            Some(Self { shutdown_tx, task })
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (handle, app_node, local_peer_id);
            info!("desktop BLE bootstrap unavailable on this platform");
            None
        }
    }

    /// Signal shutdown and wait for CoreBluetooth cleanup (avoid orphaned scan/advertise).
    pub async fn stop_and_wait(mut self) {
        let _ = self.shutdown_tx.send(true);
        let task = std::mem::replace(&mut self.task, tauri::async_runtime::spawn(async {}));
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
}

impl Drop for DesktopBleHost {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}
