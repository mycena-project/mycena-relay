// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Öffentliche Bootstrap-Peers für das Mycena-Netzwerk.
//!
//! Default Internet bootstrap is a **flat list** of independently operated relays
//! ([`standard_bootstrap::STANDARD_BOOTSTRAP_RELAYS`]) — no primary relay, no fallback
//! hierarchy. Each slot ships `/ip4/` and `/dns4/` multiaddrs (anti-DNS-block).
//!
//! Federated relay registry (signed operator entries) merges with the embedded list —
//! see [`crate::relay_registry`] and [`RELAY_OPERATOR_GUIDE.md`](../../docs/RELAY_OPERATOR_GUIDE.md).

pub use crate::standard_bootstrap::{
    standard_bootstrap_tcp_ip4_multiaddrs, RELAY_IPV4, RELAY_PEER_ID, STANDARD_BOOTSTRAP_RELAYS,
};

/// All embedded default bootstrap multiaddrs (equal weight, no preferred entry).
pub fn standard_bootstrap_multiaddrs() -> Vec<String> {
    crate::standard_bootstrap::standard_bootstrap_multiaddrs()
}

/// Public relay / bootstrap infra — not a user device in Connect UI.
pub fn is_relay_peer(peer_id: &str) -> bool {
    let p = peer_id.trim();
    if crate::standard_bootstrap::is_standard_bootstrap_peer(p) {
        return true;
    }
    let manifest = crate::relay_registry::load_relay_manifest("");
    crate::relay_pool::RelayPool::from_registry_manifest(&manifest, false).is_relay_peer(p)
}

/// Verified federated relay multiaddrs (embedded manifest + optional `relay_registry.json`).
pub fn federated_bootstrap_multiaddrs(db_path: &str) -> Vec<String> {
    crate::relay_registry::federated_bootstrap_peers(db_path)
}

/// True for a libp2p PeerId string (not a multiaddr or truncated garbage).
pub fn is_mesh_peer_id(peer_id: &str) -> bool {
    let p = peer_id.trim();
    p.starts_with("12D3KooW") && p.len() >= 52 && !p.contains('/')
}
/// Extract the `/p2p/<PeerId>` component from a libp2p multiaddr.
pub fn peer_id_from_multiaddr(addr: &str) -> Option<String> {
    let parsed: libp2p::Multiaddr = addr.parse().ok()?;
    peer_id_from_parsed_multiaddr(&parsed).map(|p| p.to_string())
}

pub fn peer_id_from_parsed_multiaddr(addr: &libp2p::Multiaddr) -> Option<libp2p::PeerId> {
    use libp2p::multiaddr::Protocol;
    addr.iter().find_map(|proto| {
        if let Protocol::P2p(peer_id) = proto {
            Some(peer_id)
        } else {
            None
        }
    })
}

/// libp2p circuit address to reach `remote_peer_id` via the public relay (NAT traversal).
pub fn relay_circuit_multiaddr(remote_peer_id: &str) -> Option<String> {
    relay_circuit_multiaddr_with_relay(remote_peer_id, None)
}

/// Circuit address via a specific relay pool entry (`relay_index` rotates on failover).
pub fn relay_circuit_multiaddr_with_relay(
    remote_peer_id: &str,
    relay_index: Option<usize>,
) -> Option<String> {
    relay_circuit_multiaddr_for_db("", remote_peer_id, relay_index)
}

/// Federated relay circuit multiaddr using embedded + `<db_path>/relay_registry.json`.
pub fn relay_circuit_multiaddr_for_db(
    db_path: &str,
    remote_peer_id: &str,
    relay_index: Option<usize>,
) -> Option<String> {
    use crate::relay_pool::RelayPool;
    use crate::relay_registry::{include_catalog_relays, load_relay_manifest};

    let remote = remote_peer_id.trim();
    if remote.is_empty() {
        return None;
    }
    let manifest = load_relay_manifest(db_path);
    let pool = RelayPool::from_registry_manifest(&manifest, include_catalog_relays());
    let pool = if pool.is_empty() {
        RelayPool::from_bootstrap_peers(&federated_bootstrap_multiaddrs(db_path))
    } else {
        pool
    };
    if pool.is_empty() {
        return None;
    }
    let idx = relay_index.unwrap_or_else(|| pool.select_diverse_index_for(remote));
    pool.circuit_multiaddr(remote, idx)
}

/// Best LAN multiaddr for embedding in a signed v3 invite (stable, not 0.0.0.0 / loopback).
pub fn pick_stable_lan_multiaddr(listen_addrs: &[String], local_peer_id: &str) -> Option<String> {
    listen_addrs.iter().find_map(|a| {
        let trimmed = a.trim();
        if trimmed.is_empty()
            || !trimmed.contains("/ip4/")
            || trimmed.contains("127.0.0.1")
            || trimmed.contains("0.0.0.0")
            || trimmed.contains("/p2p-circuit")
        {
            return None;
        }
        Some(if trimmed.contains("/p2p/") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/p2p/{local_peer_id}")
        })
    })
}

/// Shareable dial info: signed v3 invite when provided, else v1, then LAN + relay.
pub fn shareable_dial_multiaddrs(
    local_peer_id: &str,
    _enc_pubkey_hex: &str,
    listen_addrs: &[String],
    signed_invite: Option<&str>,
) -> Vec<String> {
    let best_direct = listen_addrs.iter().find_map(|a| {
        let trimmed = a.trim();
        if trimmed.is_empty()
            || !trimmed.contains("/ip4/")
            || trimmed.contains("127.0.0.1")
            || trimmed.contains("/p2p-circuit")
        {
            return None;
        }
        Some(if trimmed.contains("/p2p/") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/p2p/{local_peer_id}")
        })
    });
    let mut out = Vec::new();
    if let Some(v3) = signed_invite.filter(|s| !s.is_empty()) {
        out.push(v3.to_string());
    } else {
        out.push(crate::invite::encode_invite(local_peer_id));
    }
    if let Some(ref direct) = best_direct {
        out.push(direct.clone());
    }
    if let Some(circuit) = relay_circuit_multiaddr(local_peer_id) {
        out.push(circuit);
    }
    for addr in listen_addrs {
        let trimmed = addr.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains("/p2p/") {
            out.push(trimmed.to_string());
        } else {
            out.push(format!("{trimmed}/p2p/{local_peer_id}"));
        }
    }
    out
}

/// Lädt Bootstrap-Peers mit folgender Priorität:
/// 1. Umgebungsvariable `MYCENA_BOOTSTRAP_PEERS` (kommasepariert)
/// 2. Datei `<db_path>/bootstrap.txt` (eine Adresse pro Zeile, # = Kommentar)
/// 3. Verified federated relay registry (embedded + `<db_path>/relay_registry.json`)
/// 4. Embedded [`standard_bootstrap_multiaddrs()`]
pub fn load_bootstrap_peers(db_path: &str) -> Vec<String> {
    if let Ok(env_peers) = std::env::var("MYCENA_BOOTSTRAP_PEERS") {
        let peers: Vec<String> = env_peers
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if !peers.is_empty() {
            tracing::info!(
                "bootstrap: using {} peer(s) from MYCENA_BOOTSTRAP_PEERS",
                peers.len()
            );
            return peers;
        }
    }

    let path = std::path::Path::new(db_path).join("bootstrap.txt");
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let peers: Vec<String> = content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect();
            if !peers.is_empty() {
                tracing::info!(
                    "bootstrap: using {} peer(s) from {}",
                    peers.len(),
                    path.display()
                );
                return peers;
            }
        }
    }

    tracing::info!(
        "bootstrap: using {} built-in peer(s) from standard bootstrap list",
        standard_bootstrap_multiaddrs().len()
    );
    let mut peers: Vec<String> = federated_bootstrap_multiaddrs(db_path);
    if peers.is_empty() {
        peers = standard_bootstrap_multiaddrs();
    }
    peers
}

/// Reads custom bootstrap peers from `<db_path>/bootstrap.txt` only (no env / built-in fallback).
pub fn load_custom_bootstrap_peers(db_path: &str) -> Vec<String> {
    let path = std::path::Path::new(db_path).join("bootstrap.txt");
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&path)
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Peers / multiaddrs the user added via QR or Connect (retried until connected).
const DIAL_PEERS_FILE: &str = "dial_peers.txt";

/// Load persisted dial targets (one multiaddr per line).
pub fn load_persisted_dial_peers(db_path: &str) -> Vec<String> {
    let path = std::path::Path::new(db_path).join(DIAL_PEERS_FILE);
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&path)
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Append a dial target if not already stored (QR scan, Connect tab, etc.).
/// Circuit targets replace any previously stored circuit entries (one active QR target).
pub fn persist_dial_peer(db_path: &str, multiaddr: &str) -> std::io::Result<()> {
    let trimmed = multiaddr.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut peers = load_persisted_dial_peers(db_path);
    if trimmed.contains("/p2p-circuit/") {
        peers.retain(|p| !p.contains("/p2p-circuit/"));
    }
    if peers.iter().any(|p| p == trimmed) {
        return Ok(());
    }
    peers.push(trimmed.to_string());
    write_dial_peers_file(db_path, &peers)
}

/// Keep only the most recent circuit dial target; drop duplicate/stale QR scans.
pub fn sanitize_persisted_dial_peers(db_path: &str) -> std::io::Result<()> {
    let peers = load_persisted_dial_peers(db_path);
    let mut cleaned: Vec<String> = peers
        .iter()
        .filter(|p| !p.contains("/p2p-circuit/"))
        .cloned()
        .collect();
    if let Some(last_circuit) = peers.iter().rfind(|p| p.contains("/p2p-circuit/")) {
        cleaned.push(last_circuit.clone());
    }
    if cleaned == peers {
        return Ok(());
    }
    if cleaned.is_empty() {
        let path = std::path::Path::new(db_path).join(DIAL_PEERS_FILE);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    write_dial_peers_file(db_path, &cleaned)
}

fn write_dial_peers_file(db_path: &str, peers: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(db_path)?;
    let path = std::path::Path::new(db_path).join(DIAL_PEERS_FILE);
    std::fs::write(path, peers.join("\n"))
}

/// Persists custom bootstrap peers to `<db_path>/bootstrap.txt`.
pub fn save_custom_bootstrap_peers(db_path: &str, peers: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(db_path)?;
    let path = std::path::Path::new(db_path).join("bootstrap.txt");
    if peers.is_empty() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    std::fs::write(path, peers.join("\n"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn peer_id_from_multiaddr_extracts_embedded_id() {
        let addrs = standard_bootstrap_multiaddrs();
        let addr = addrs
            .iter()
            .find(|a| a.contains(RELAY_PEER_ID))
            .expect("operated relay in standard bootstrap");
        let peer_id = peer_id_from_multiaddr(addr).expect("peer id");
        assert_eq!(peer_id, RELAY_PEER_ID);
    }
    #[test]
    fn standard_bootstrap_has_seven_equal_relays() {
        let addrs = standard_bootstrap_multiaddrs();
        assert!(addrs.len() >= 28, "7 relays × (ip4+dns4) × (tcp+quic)");
        assert!(addrs.iter().any(|a| a.contains("mycena-relay.fly.dev")));
        assert!(addrs.iter().any(|a| a.contains("192.0.2.57")));
    }

    #[test]
    fn relay_circuit_addr_uses_embedded_relay() {
        let addr =
            relay_circuit_multiaddr("12D3KooWExamplePeerIdForTestOnly").expect("circuit addr");
        assert!(addr.contains("/p2p-circuit/p2p/12D3KooWExamplePeerIdForTestOnly"));
        assert!(
            crate::standard_bootstrap::standard_bootstrap_peer_ids()
                .iter()
                .any(|id| addr.contains(id.as_str())),
            "circuit must route via a standard bootstrap relay: {addr}"
        );
    }

    #[test]
    fn persist_dial_peer_replaces_old_circuit_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().to_str().expect("path");
        let old = relay_circuit_multiaddr("12D3KooWOldPeerIdForTestOnly").expect("old");
        let new = relay_circuit_multiaddr("12D3KooWNewPeerIdForTestOnly").expect("new");
        persist_dial_peer(db, &old).expect("persist old");
        persist_dial_peer(db, &new).expect("persist new");
        let peers = load_persisted_dial_peers(db);
        assert_eq!(peers.len(), 1);
        assert!(peers[0].contains("12D3KooWNewPeerIdForTestOnly"));
    }

    #[test]
    fn is_mesh_peer_id_rejects_multiaddr_and_short_ids() {
        assert!(is_mesh_peer_id(RELAY_PEER_ID));
        assert!(is_mesh_peer_id(
            "12D3KooWLpQgW1svymPTqprdewzviJjoSYe4fVzUzgjP6P9e7Q3A"
        ));
        assert!(!is_mesh_peer_id("12D3KooW"));
        assert!(!is_mesh_peer_id(
            "/dns4/mycena-relay.fly.dev/tcp/4001/p2p/12D3KooW"
        ));
    }

    #[test]
    fn sanitize_keeps_latest_circuit_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().to_str().expect("path");
        write_dial_peers_file(
            db,
            &[
                relay_circuit_multiaddr("12D3KooWFirstPeerIdForTestOnly").expect("a"),
                relay_circuit_multiaddr("12D3KooWSecondPeerIdForTestOnly").expect("b"),
            ],
        )
        .expect("write");
        sanitize_persisted_dial_peers(db).expect("sanitize");
        let peers = load_persisted_dial_peers(db);
        assert_eq!(peers.len(), 1);
        assert!(peers[0].contains("12D3KooWSecondPeerIdForTestOnly"));
    }
}
