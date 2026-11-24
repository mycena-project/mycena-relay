// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Rendezvous HTTP API against the public relay (opt-in discovery).
//!
//! Registration is **signed** with the node's libp2p key. Unsigned `{peer_id}`
//! POSTs are rejected so an attacker cannot inject arbitrary IDs onto the list.

use libp2p::identity::{Keypair, PublicKey};
use serde::{Deserialize, Serialize};

const MAX_SKEW_MS: u64 = 5 * 60 * 1000;
const BINDING: &str = "mycena-rendezvous-v1";

#[derive(Debug, Deserialize)]
struct RendezvousResponse {
    peers: Vec<String>,
}

/// Signed proof that the caller controls `peer_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RendezvousProof {
    pub peer_id: String,
    pub public_key_hex: String,
    pub ts_ms: u64,
    pub signature_hex: String,
}

fn relay_origin(host: Option<&str>) -> String {
    crate::standard_bootstrap::relay_http_origin(host)
}

fn canonical_message(action: &str, peer_id: &str, ts_ms: u64) -> String {
    format!("{BINDING}|{action}|{peer_id}|{ts_ms}")
}

/// Build a signed register/unregister body from the local libp2p key.
pub fn sign_rendezvous_proof(
    keypair: &Keypair,
    register: bool,
    ts_ms: u64,
) -> anyhow::Result<RendezvousProof> {
    let peer_id = keypair.public().to_peer_id().to_string();
    let action = if register { "register" } else { "unregister" };
    let msg = canonical_message(action, &peer_id, ts_ms);
    let signature = keypair.sign(msg.as_bytes())?;
    Ok(RendezvousProof {
        peer_id,
        public_key_hex: hex::encode(keypair.public().encode_protobuf()),
        ts_ms,
        signature_hex: hex::encode(signature),
    })
}

/// Verify a proof. Returns the authenticated peer ID.
pub fn verify_rendezvous_proof(
    proof: &RendezvousProof,
    register: bool,
    now_ms: u64,
) -> Result<String, &'static str> {
    if proof.peer_id.is_empty() {
        return Err("empty peer_id");
    }
    let skew = now_ms.abs_diff(proof.ts_ms);
    if skew > MAX_SKEW_MS {
        return Err("timestamp outside replay window");
    }
    let pk_bytes =
        hex::decode(proof.public_key_hex.trim()).map_err(|_| "invalid public_key_hex")?;
    let pk = PublicKey::try_decode_protobuf(&pk_bytes).map_err(|_| "invalid public key")?;
    if pk.to_peer_id().to_string() != proof.peer_id {
        return Err("public key does not match peer_id");
    }
    let sig = hex::decode(proof.signature_hex.trim()).map_err(|_| "invalid signature_hex")?;
    let action = if register { "register" } else { "unregister" };
    let msg = canonical_message(action, &proof.peer_id, proof.ts_ms);
    if !pk.verify(msg.as_bytes(), &sig) {
        return Err("bad signature");
    }
    Ok(proof.peer_id.clone())
}

/// Register or unregister this peer for relay rendezvous (opt-in visibility).
pub fn set_relay_rendezvous_registration(keypair: &Keypair, register: bool, host: Option<&str>) {
    let origin = relay_origin(host);
    let path = if register {
        "rendezvous/register"
    } else {
        "rendezvous/unregister"
    };
    let url = format!("{origin}/{path}");
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let Ok(body) = sign_rendezvous_proof(keypair, register, ts_ms) else {
        tracing::debug!("rendezvous proof sign failed");
        return;
    };
    let result = ureq::post(&url)
        .timeout(std::time::Duration::from_secs(8))
        .send_json(body);
    if let Err(e) = result {
        tracing::debug!("rendezvous registration ({register}) failed: {e}");
    }
}

/// Returns peer IDs opted in and currently connected to the public relay.
pub fn fetch_relay_rendezvous(host: Option<&str>) -> Vec<String> {
    let origin = relay_origin(host);
    let url = format!("{origin}/rendezvous");
    let resp = match ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("rendezvous fetch failed: {e}");
            return Vec::new();
        }
    };
    let body: RendezvousResponse = match resp.into_json() {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("rendezvous json failed: {e}");
            return Vec::new();
        }
    };
    body.peers
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn signed_proof_roundtrip() {
        let kp = Keypair::generate_ed25519();
        let now = 1_700_000_000_000;
        let proof = sign_rendezvous_proof(&kp, true, now).unwrap();
        let peer = verify_rendezvous_proof(&proof, true, now).unwrap();
        assert_eq!(peer, kp.public().to_peer_id().to_string());
    }

    #[test]
    fn unsigned_legacy_body_fails() {
        let proof = RendezvousProof {
            peer_id: "12D3KooWfake".into(),
            public_key_hex: String::new(),
            ts_ms: 1,
            signature_hex: String::new(),
        };
        assert!(verify_rendezvous_proof(&proof, true, 1).is_err());
    }

    #[test]
    fn wrong_action_fails() {
        let kp = Keypair::generate_ed25519();
        let now = 1_700_000_000_000;
        let proof = sign_rendezvous_proof(&kp, true, now).unwrap();
        assert!(verify_rendezvous_proof(&proof, false, now).is_err());
    }

    #[test]
    fn stale_timestamp_fails() {
        let kp = Keypair::generate_ed25519();
        let now = 1_700_000_000_000;
        let proof = sign_rendezvous_proof(&kp, true, now).unwrap();
        assert!(verify_rendezvous_proof(&proof, true, now + MAX_SKEW_MS + 1).is_err());
    }
}
