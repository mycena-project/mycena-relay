// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycelium Project
//! Rendezvous HTTP API against the public relay (opt-in discovery).
//!
//! Register/unregister bodies are signed with the libp2p identity so a third
//! party cannot opt another peer in or knock them off the list.

use libp2p::PeerId;
use serde::{Deserialize, Serialize};

const DEFAULT_RELAY_HOST: &str = "mycelium-relay.fly.dev";
const SIGN_DOMAIN: &[u8] = b"mycelium-rendezvous-v1";
const MAX_SKEW_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Deserialize)]
struct RendezvousResponse {
    peers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RendezvousAuthBody {
    pub peer_id: String,
    pub ts_ms: u64,
    pub signature_hex: String,
}

fn relay_host(host: Option<&str>) -> String {
    let env_host = std::env::var("MYCELIUM_RELAY_HOST").ok();
    host.or(env_host.as_deref())
        .unwrap_or(DEFAULT_RELAY_HOST)
        .to_string()
}

pub fn signing_bytes(peer_id: &str, register: bool, ts_ms: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SIGN_DOMAIN.len() + peer_id.len() + 16);
    bytes.extend_from_slice(SIGN_DOMAIN);
    bytes.push(0);
    bytes.push(u8::from(register));
    bytes.extend_from_slice(peer_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&ts_ms.to_le_bytes());
    bytes
}

pub fn sign_registration(
    keypair: &libp2p::identity::Keypair,
    register: bool,
) -> anyhow::Result<RendezvousAuthBody> {
    let peer_id = keypair.public().to_peer_id().to_string();
    let ts_ms = crate::data::now_ms();
    let sig = keypair
        .sign(&signing_bytes(&peer_id, register, ts_ms))
        .map_err(|e| anyhow::anyhow!("rendezvous sign: {e}"))?;
    Ok(RendezvousAuthBody {
        peer_id,
        ts_ms,
        signature_hex: hex::encode(sig),
    })
}

fn verify_peer_signature(peer_id: &PeerId, bytes: &[u8], sig: &[u8]) -> bool {
    let multihash = peer_id.as_ref();
    if multihash.code() != 0 {
        return false;
    }
    let Ok(pk) = libp2p::identity::PublicKey::try_decode_protobuf(multihash.digest()) else {
        return false;
    };
    pk.verify(bytes, sig)
}

/// Parse and verify a register/unregister body. Returns the authenticated PeerId.
pub fn verify_registration(
    body: &RendezvousAuthBody,
    register: bool,
    now_ms: u64,
) -> Result<PeerId, &'static str> {
    let peer_id: PeerId = body.peer_id.parse().map_err(|_| "invalid peer id")?;
    let skew = now_ms.abs_diff(body.ts_ms);
    if skew > MAX_SKEW_MS {
        return Err("timestamp skew");
    }
    let sig = hex::decode(body.signature_hex.trim()).map_err(|_| "invalid signature hex")?;
    let bytes = signing_bytes(&body.peer_id, register, body.ts_ms);
    if !verify_peer_signature(&peer_id, &bytes, &sig) {
        return Err("bad signature");
    }
    Ok(peer_id)
}

/// Register or unregister this peer for relay rendezvous (opt-in visibility).
pub fn set_relay_rendezvous_registration(
    keypair: &libp2p::identity::Keypair,
    register: bool,
    host: Option<&str>,
) {
    let host = relay_host(host);
    let path = if register {
        "rendezvous/register"
    } else {
        "rendezvous/unregister"
    };
    let url = format!("https://{host}/{path}");
    let body = match sign_registration(keypair, register) {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!("rendezvous sign failed: {e}");
            return;
        }
    };
    let result = ureq::post(&url)
        .timeout(std::time::Duration::from_secs(8))
        .send_json(body);
    if let Err(e) = result {
        tracing::debug!("rendezvous registration ({register}) failed: {e}");
    }
}

/// Returns peer IDs opted in and currently reserved on the public relay.
pub fn fetch_relay_rendezvous(host: Option<&str>) -> Vec<String> {
    let host = relay_host(host);
    let url = format!("https://{host}/rendezvous");
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
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let kp = libp2p::identity::Keypair::generate_ed25519();
        let body = sign_registration(&kp, true).unwrap();
        let now = crate::data::now_ms();
        let peer = verify_registration(&body, true, now).unwrap();
        assert_eq!(peer, kp.public().to_peer_id());
        assert!(verify_registration(&body, false, now).is_err());
    }

    #[test]
    fn rejects_wrong_key() {
        let kp = libp2p::identity::Keypair::generate_ed25519();
        let other = libp2p::identity::Keypair::generate_ed25519();
        let mut body = sign_registration(&kp, true).unwrap();
        body.peer_id = other.public().to_peer_id().to_string();
        let now = crate::data::now_ms();
        assert!(verify_registration(&body, true, now).is_err());
    }
}
