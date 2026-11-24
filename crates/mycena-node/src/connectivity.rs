// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Periodic reachability probe for hybrid Internet / mesh operation.

use mycena_core::transport::ConnectivityMode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::info;

const PROBE_HOST: &str = "mycena-relay.fly.dev";
const PROBE_PORT: u16 = 4001;

pub struct ConnectivityMonitor {
    pub mode_tx: watch::Sender<ConnectivityMode>,
    pub mode_rx: watch::Receiver<ConnectivityMode>,
}

/// Effective transport mode: user opt-out wins over relay reachability.
pub fn effective_connectivity_mode(
    internet_enabled: bool,
    probe_reachable: bool,
) -> ConnectivityMode {
    if !internet_enabled {
        return ConnectivityMode::MeshOnly;
    }
    if probe_reachable {
        ConnectivityMode::Internet
    } else {
        ConnectivityMode::MeshOnly
    }
}

impl ConnectivityMonitor {
    pub fn new(internet_enabled: bool) -> (Self, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(internet_enabled));
        let initial = effective_connectivity_mode(internet_enabled, true);
        let (mode_tx, mode_rx) = watch::channel(initial);
        (Self { mode_tx, mode_rx }, flag)
    }

    /// Background task: probe TCP reachability every 15s and update [`ConnectivityMode`].
    pub fn spawn_monitor(tx: watch::Sender<ConnectivityMode>, internet_enabled: Arc<AtomicBool>) {
        tokio::spawn(async move {
            let mut current = *tx.borrow();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let reachable = check_internet_connectivity().await;
                let enabled = internet_enabled.load(Ordering::Relaxed);
                let new_mode = effective_connectivity_mode(enabled, reachable);
                if new_mode != current {
                    info!("connectivity changed: {:?} → {:?}", current, new_mode);
                    current = new_mode;
                    let _ = tx.send(new_mode);
                }
            }
        });
    }
}

impl Default for ConnectivityMonitor {
    fn default() -> Self {
        Self::new(true).0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn opt_out_forces_mesh_only() {
        assert_eq!(
            effective_connectivity_mode(false, true),
            ConnectivityMode::MeshOnly
        );
        assert_eq!(
            effective_connectivity_mode(false, false),
            ConnectivityMode::MeshOnly
        );
    }

    #[test]
    fn probe_decides_when_internet_enabled() {
        assert_eq!(
            effective_connectivity_mode(true, true),
            ConnectivityMode::Internet
        );
        assert_eq!(
            effective_connectivity_mode(true, false),
            ConnectivityMode::MeshOnly
        );
    }
}

async fn check_internet_connectivity() -> bool {
    let addr = format!("{PROBE_HOST}:{PROBE_PORT}");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await;
    matches!(result, Ok(Ok(_)))
}

/// Kademlia policy when connectivity mode changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KadConnectivityAction {
    /// Run bootstrap and keep client-mode DHT queries enabled.
    Activate,
    /// Stay in client mode but skip bootstrap (mesh-only / offline).
    Pause,
}

pub fn kad_action_for_mode(mode: ConnectivityMode) -> KadConnectivityAction {
    match mode {
        ConnectivityMode::Internet => KadConnectivityAction::Activate,
        ConnectivityMode::MeshOnly => KadConnectivityAction::Pause,
    }
}
