// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! HTTP status + signed rendezvous surface for the bootstrap relay.

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use mycena_l0_facade::lock_recover::{read_rwlock_or_recover, write_rwlock_or_recover};
use mycena_l0_facade::protocol::IDENTIFY_PROTOCOL;
use mycena_l0_facade::rendezvous::{verify_rendezvous_proof, RendezvousProof};
use serde_json::json;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct StatusState {
    pub peer_id: String,
    pub connections: Arc<AtomicU64>,
    pub reservations: Arc<AtomicU64>,
    pub connected_peers: Arc<RwLock<HashSet<String>>>,
    pub rendezvous_opt_in: Arc<RwLock<HashSet<String>>>,
    pub started: Instant,
    pub version: &'static str,
    /// When set, `/status` requires `Authorization: Bearer`. Health stays public.
    pub status_token: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn bearer_ok(headers: &axum::http::HeaderMap, expected: &str) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t.trim() == expected)
}

pub fn status_router(state: StatusState) -> Router {
    Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/rendezvous", get(rendezvous))
        .route("/rendezvous/register", post(rendezvous_register))
        .route("/rendezvous/unregister", post(rendezvous_unregister))
        .with_state(state)
}

async fn health(State(s): State<StatusState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "version": s.version,
    }))
}

async fn status(
    State(s): State<StatusState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if let Some(token) = &s.status_token {
        if !bearer_ok(&headers, token) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    } else {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(json!({
        "status": "ok",
        "peer_id": s.peer_id,
        "connections": s.connections.load(Ordering::Relaxed),
        "reservations": s.reservations.load(Ordering::Relaxed),
        "uptime_secs": s.started.elapsed().as_secs(),
        "version": s.version,
        "wire_protocol": IDENTIFY_PROTOCOL,
    })))
}

async fn rendezvous(State(s): State<StatusState>) -> Json<serde_json::Value> {
    let connected = read_rwlock_or_recover(&s.connected_peers, "relay connected_peers");
    let opt_in = read_rwlock_or_recover(&s.rendezvous_opt_in, "relay rendezvous_opt_in");
    let peers: Vec<String> = connected
        .iter()
        .filter(|p| opt_in.contains(*p))
        .cloned()
        .collect();
    Json(json!({ "peers": peers }))
}

async fn rendezvous_register(
    State(s): State<StatusState>,
    Json(body): Json<RendezvousProof>,
) -> StatusCode {
    match verify_rendezvous_proof(&body, true, now_ms()) {
        Ok(peer_id) => {
            write_rwlock_or_recover(&s.rendezvous_opt_in, "relay rendezvous_opt_in")
                .insert(peer_id);
            StatusCode::NO_CONTENT
        }
        Err(_) => StatusCode::UNAUTHORIZED,
    }
}

async fn rendezvous_unregister(
    State(s): State<StatusState>,
    Json(body): Json<RendezvousProof>,
) -> StatusCode {
    match verify_rendezvous_proof(&body, false, now_ms()) {
        Ok(peer_id) => {
            write_rwlock_or_recover(&s.rendezvous_opt_in, "relay rendezvous_opt_in")
                .remove(&peer_id);
            StatusCode::NO_CONTENT
        }
        Err(_) => StatusCode::UNAUTHORIZED,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use libp2p::identity::Keypair;
    use mycena_l0_facade::rendezvous::sign_rendezvous_proof;

    fn test_state() -> StatusState {
        StatusState {
            peer_id: "relay".into(),
            connections: Arc::new(AtomicU64::new(0)),
            reservations: Arc::new(AtomicU64::new(0)),
            connected_peers: Arc::new(RwLock::new(HashSet::new())),
            rendezvous_opt_in: Arc::new(RwLock::new(HashSet::new())),
            started: Instant::now(),
            version: "test",
            status_token: Some("test-token-not-for-prod".into()),
        }
    }

    #[test]
    fn unsigned_register_is_rejected() {
        let fake = RendezvousProof {
            peer_id: "12D3KooWinject".into(),
            public_key_hex: String::new(),
            ts_ms: now_ms(),
            signature_hex: String::new(),
        };
        assert!(verify_rendezvous_proof(&fake, true, now_ms()).is_err());
    }

    #[test]
    fn signed_register_is_accepted() {
        let s = test_state();
        let kp = Keypair::generate_ed25519();
        let proof = sign_rendezvous_proof(&kp, true, now_ms()).unwrap();
        let peer = verify_rendezvous_proof(&proof, true, now_ms()).unwrap();
        write_rwlock_or_recover(&s.rendezvous_opt_in, "t").insert(peer.clone());
        assert!(read_rwlock_or_recover(&s.rendezvous_opt_in, "t").contains(&peer));
        assert_eq!(peer, kp.public().to_peer_id().to_string());
    }
}
