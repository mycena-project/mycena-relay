// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! HTTP status probe for the public mycena-relay (used by desktop UI).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RelayStatus {
    pub online: bool,
    pub status: String,
    /// Raw libp2p socket count (TCP+QUIC per client; not unique devices).
    pub connections: u64,
    /// Unique peer IDs currently connected to the relay (when reported by server).
    pub unique_peers: u64,
    /// Peers opted into rendezvous and reachable via relay circuit (best user-facing count).
    pub reachable_peers: u64,
    pub peer_id: Option<String>,
}

/// Fetch relay liveness from `{origin}/health` (blocking).
/// Full telemetry on `/status` is operator-token-gated and is not queried here.
pub fn fetch_relay_status(host: Option<&str>) -> RelayStatus {
    let origin = crate::standard_bootstrap::relay_http_origin(host);
    let url = format!("{origin}/health");
    let reachable_peers = fetch_relay_rendezvous_count(&origin);
    let resp = match ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(r) => r,
        Err(_) => {
            return RelayStatus {
                online: false,
                status: "offline".into(),
                connections: 0,
                unique_peers: 0,
                reachable_peers,
                peer_id: None,
            };
        }
    };
    let body: serde_json::Value = match resp.into_json() {
        Ok(v) => v,
        Err(_) => {
            return RelayStatus {
                online: false,
                status: "offline".into(),
                connections: 0,
                unique_peers: 0,
                reachable_peers,
                peer_id: None,
            };
        }
    };
    let online = body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    RelayStatus {
        online,
        status: if online {
            "ok".into()
        } else {
            "offline".into()
        },
        connections: 0,
        unique_peers: 0,
        reachable_peers,
        peer_id: None,
    }
}

fn fetch_relay_rendezvous_count(origin: &str) -> u64 {
    let url = format!("{origin}/rendezvous");
    let resp = match ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let body: serde_json::Value = match resp.into_json() {
        Ok(v) => v,
        Err(_) => return 0,
    };
    body.get("peers")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0)
}
