// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Default Internet bootstrap relays — equal peers, anti-censorship distribution.
//!
//! Every slot ships **both** `/ip4/…` and `/dns4/…` multiaddrs (plus QUIC) so blocking
//! DNS alone is insufficient. There is no primary relay; clients dial the full set and
//! [`crate::relay_pool::RelayPool`] hash-selects for circuits.

use crate::relay_registry::key_from_seed;
use libp2p::identity::Keypair;

/// One default bootstrap relay slot (independent operator / jurisdiction).
#[derive(Debug, Clone, Copy)]
pub struct StandardBootstrapRelay {
    /// Stable slot id (registry + operator docs).
    pub slot: &'static str,
    /// libp2p PeerId when the relay identity is fixed (production deploy).
    pub fixed_peer_id: Option<&'static str>,
    /// Derive PeerId from this seed when [`fixed_peer_id`] is `None`.
    pub peer_seed: Option<&'static [u8]>,
    pub ipv4: &'static str,
    pub dns4: &'static str,
    pub operator_label: &'static str,
    pub region: &'static str,
    pub hosting_provider: &'static str,
    pub operator_seed: &'static [u8],
    pub curator_id: &'static str,
    /// BGP ASN (public, embedded for client diversity rules).
    pub asn: u32,
    /// Public operator contact (email or HTTPS URL).
    pub operator_contact: &'static str,
    /// `true` when the relay is reachable at the listed addresses today.
    pub live: bool,
}

/// Hostname of the operated bootstrap relay (Fly.io machine).
pub const RELAY_HOSTNAME: &str = "mycena-relay.fly.dev";

/// Public `/health` and `/rendezvous` origin.
///
/// Fly terminates TLS on 443 and forwards to the relay status server.
/// (internal port 8080, see deploy/relay/fly.toml in mycena-relay).
pub const RELAY_HTTP_ORIGIN: &str = "https://mycena-relay.fly.dev";

/// Resolve the HTTP origin for relay health and rendezvous.
///
/// `MYCENA_RELAY_HTTP_ORIGIN` wins when set (full origin, or `host` / `host:port`).
/// `MYCENA_RELAY_HOST` is the same shape. A bare hostname uses port 8080.
pub fn relay_http_origin(override_host: Option<&str>) -> String {
    if let Some(raw) = override_host.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_relay_origin(raw);
    }
    if let Ok(raw) = std::env::var("MYCENA_RELAY_HTTP_ORIGIN") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return normalize_relay_origin(raw);
        }
    }
    if let Ok(raw) = std::env::var("MYCENA_RELAY_HOST") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return normalize_relay_origin(raw);
        }
    }
    RELAY_HTTP_ORIGIN.to_string()
}

fn normalize_relay_origin(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('/');
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_string();
    }
    if raw.contains(':') {
        return format!("http://{raw}");
    }
    format!("http://{raw}:8080")
}

/// Seven geographically distributed bootstrap relays (no preferred entry).
///
/// Slot `eu-fly` is the operated relay. Other slots are curator-endorsed
/// reservations — operators claim a slot by deploying to the listed DNS + IPv4 and
/// opening a registry update PR (see `docs/RELAY_OPERATOR_GUIDE.md`).
pub const STANDARD_BOOTSTRAP_RELAYS: &[StandardBootstrapRelay] = &[
    StandardBootstrapRelay {
        slot: "eu-fly",
        fixed_peer_id: Some("12D3KooWCVC3ZgY5T4tvBz6kujKzbifbCGnU3kDbvtPnRPfHTogn"),
        peer_seed: None,
        ipv4: "192.0.2.57",
        dns4: RELAY_HOSTNAME,
        operator_label: "mycena-core-fly",
        region: "eu-central",
        hosting_provider: "Fly.io (FRA)",
        operator_seed: b"operator:mycena-fly:v1",
        curator_id: "mycena-release",
        asn: 51_167,
        operator_contact: "https://github.com/mycena-project/Mycena/security/advisories/new",
        live: true,
    },
    StandardBootstrapRelay {
        slot: "eu-fra",
        fixed_peer_id: None,
        peer_seed: Some(b"peer:bootstrap:eu-fra:v1"),
        ipv4: "243.185.187.10",
        dns4: "bootstrap-eu-fra.mycena.network",
        operator_label: "community-mesh-eu-fra",
        region: "eu-central",
        hosting_provider: "Hetzner (DE)",
        operator_seed: b"operator:community-mesh-eu-fra:v1",
        curator_id: "community-mesh-eu",
        asn: 24_940,
        operator_contact: "ops-eu-fra@community-mesh.example",
        live: false,
    },
    StandardBootstrapRelay {
        slot: "eu-ams",
        fixed_peer_id: None,
        peer_seed: Some(b"peer:bootstrap:eu-ams:v1"),
        ipv4: "243.186.187.10",
        dns4: "bootstrap-eu-ams.mycena.network",
        operator_label: "community-mesh-eu-ams",
        region: "eu-west",
        hosting_provider: "OVH (NL)",
        operator_seed: b"operator:community-mesh-eu-ams:v1",
        curator_id: "community-mesh-eu",
        asn: 16_276,
        operator_contact: "ops-eu-ams@community-mesh.example",
        live: false,
    },
    StandardBootstrapRelay {
        slot: "na-east",
        fixed_peer_id: None,
        peer_seed: Some(b"peer:bootstrap:na-east:v1"),
        ipv4: "243.187.187.10",
        dns4: "bootstrap-na-east.mycena.network",
        operator_label: "community-mesh-na-east",
        region: "na-east",
        hosting_provider: "Vultr (US-East)",
        operator_seed: b"operator:community-mesh-na-east:v1",
        curator_id: "community-mesh-na",
        asn: 20_473,
        operator_contact: "ops-na-east@community-mesh.example",
        live: false,
    },
    StandardBootstrapRelay {
        slot: "na-west",
        fixed_peer_id: None,
        peer_seed: Some(b"peer:bootstrap:na-west:v1"),
        ipv4: "243.188.187.10",
        dns4: "bootstrap-na-west.mycena.network",
        operator_label: "community-mesh-na-west",
        region: "na-west",
        hosting_provider: "Akamai/Linode (US-West)",
        operator_seed: b"operator:community-mesh-na-west:v1",
        curator_id: "community-mesh-na",
        asn: 63_949,
        operator_contact: "ops-na-west@community-mesh.example",
        live: false,
    },
    StandardBootstrapRelay {
        slot: "ap-syd",
        fixed_peer_id: None,
        peer_seed: Some(b"peer:bootstrap:ap-syd:v1"),
        ipv4: "243.189.187.10",
        dns4: "bootstrap-ap-syd.mycena.network",
        operator_label: "community-mesh-ap-syd",
        region: "ap-southeast",
        hosting_provider: "DigitalOcean (AU)",
        operator_seed: b"operator:community-mesh-ap-syd:v1",
        curator_id: "community-mesh-eu",
        asn: 14_061,
        operator_contact: "ops-ap-syd@community-mesh.example",
        live: false,
    },
    StandardBootstrapRelay {
        slot: "sa-sao",
        fixed_peer_id: None,
        peer_seed: Some(b"peer:bootstrap:sa-sao:v1"),
        ipv4: "243.190.187.10",
        dns4: "bootstrap-sa-sao.mycena.network",
        operator_label: "community-mesh-sa-sao",
        region: "sa-east",
        hosting_provider: "Hostinger (BR)",
        operator_seed: b"operator:community-mesh-sa-sao:v1",
        curator_id: "community-mesh-na",
        asn: 47_583,
        operator_contact: "ops-sa-sao@community-mesh.example",
        live: false,
    },
];

/// Legacy alias — operated slot (Fly.io).
pub const RELAY_IPV4: &str = STANDARD_BOOTSTRAP_RELAYS[0].ipv4;
pub const RELAY_PEER_ID: &str = "12D3KooWCVC3ZgY5T4tvBz6kujKzbifbCGnU3kDbvtPnRPfHTogn";

/// Resolve the libp2p PeerId for a bootstrap slot.
pub fn relay_peer_id(relay: &StandardBootstrapRelay) -> anyhow::Result<String> {
    if let Some(id) = relay.fixed_peer_id {
        return Ok(id.to_string());
    }
    let seed = relay
        .peer_seed
        .ok_or_else(|| anyhow::anyhow!("slot {} missing peer identity", relay.slot))?;
    let kp = key_from_seed(seed)?;
    Ok(kp.public().to_peer_id().to_string())
}

/// All multiaddrs for one slot: IP + DNS, TCP + QUIC (unordered, equal weight).
pub fn relay_multiaddrs(relay: &StandardBootstrapRelay) -> anyhow::Result<Vec<String>> {
    let peer_id = relay_peer_id(relay)?;
    Ok(vec![
        format!("/ip4/{}/tcp/4001/p2p/{peer_id}", relay.ipv4),
        format!("/ip4/{}/udp/4001/quic-v1/p2p/{peer_id}", relay.ipv4),
        format!("/dns4/{}/tcp/4001/p2p/{peer_id}", relay.dns4),
        format!("/dns4/{}/udp/4001/quic-v1/p2p/{peer_id}", relay.dns4),
    ])
}

/// Flat list of all default bootstrap multiaddrs (no preferred ordering).
pub fn standard_bootstrap_multiaddrs() -> Vec<String> {
    let mut out = Vec::new();
    for relay in STANDARD_BOOTSTRAP_RELAYS {
        match relay_multiaddrs(relay) {
            Ok(addrs) => {
                for addr in addrs {
                    if !out.iter().any(|x| x == &addr) {
                        out.push(addr);
                    }
                }
            }
            Err(e) => tracing::warn!("standard bootstrap slot {} skipped: {e}", relay.slot),
        }
    }
    out
}

/// TCP `/ip4/` bases only (relay pool primary dial paths).
pub fn standard_bootstrap_tcp_ip4_multiaddrs() -> Vec<String> {
    let mut out = Vec::new();
    for relay in STANDARD_BOOTSTRAP_RELAYS {
        if let Ok(peer_id) = relay_peer_id(relay) {
            let addr = format!("/ip4/{}/tcp/4001/p2p/{peer_id}", relay.ipv4);
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
    }
    out
}

/// All standard bootstrap relay PeerIds.
pub fn standard_bootstrap_peer_ids() -> Vec<String> {
    STANDARD_BOOTSTRAP_RELAYS
        .iter()
        .filter_map(|relay| relay_peer_id(relay).ok())
        .collect()
}

pub fn is_standard_bootstrap_peer(peer_id: &str) -> bool {
    let p = peer_id.trim();
    standard_bootstrap_peer_ids().iter().any(|id| id == p)
}

pub fn operator_keypair(relay: &StandardBootstrapRelay) -> anyhow::Result<Keypair> {
    key_from_seed(relay.operator_seed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn at_least_six_independent_bootstrap_slots() {
        assert!(STANDARD_BOOTSTRAP_RELAYS.len() >= 6);
        let providers: std::collections::HashSet<_> = STANDARD_BOOTSTRAP_RELAYS
            .iter()
            .map(|r| r.hosting_provider)
            .collect();
        assert!(
            providers.len() >= 6,
            "each slot must use a distinct hosting provider"
        );
    }

    #[test]
    fn each_slot_has_ip4_and_dns4_multiaddrs() {
        for relay in STANDARD_BOOTSTRAP_RELAYS {
            let addrs = relay_multiaddrs(relay).expect("multiaddrs");
            assert!(
                addrs
                    .iter()
                    .any(|a| a.starts_with("/ip4/") && a.contains("/tcp/")),
                "{} missing ip4/tcp",
                relay.slot
            );
            assert!(
                addrs
                    .iter()
                    .any(|a| a.starts_with("/dns4/") && a.contains("/tcp/")),
                "{} missing dns4/tcp",
                relay.slot
            );
        }
    }

    #[test]
    fn ipv4_prefixes_are_unique_per_slot() {
        use crate::relay_registry::ipv4_prefix_from_multiaddr;
        let mut seen = std::collections::HashSet::new();
        for relay in STANDARD_BOOTSTRAP_RELAYS {
            let addr = format!("/ip4/{}/tcp/4001/p2p/placeholder", relay.ipv4);
            let prefix = ipv4_prefix_from_multiaddr(&addr).expect("prefix");
            assert!(seen.insert(prefix), "duplicate /24 for slot {}", relay.slot);
        }
    }

    #[test]
    fn operated_http_origin_uses_slot_hostname() {
        let host = STANDARD_BOOTSTRAP_RELAYS[0].dns4;
        assert_eq!(RELAY_HTTP_ORIGIN, format!("https://{host}"));
        assert_eq!(
            relay_http_origin(Some("relay.example")),
            "http://relay.example:8080"
        );
        assert_eq!(
            relay_http_origin(Some("http://relay.example:8080")),
            "http://relay.example:8080"
        );
    }

    #[test]
    fn bootstrap_slot_peer_ids_are_stable() {
        for relay in STANDARD_BOOTSTRAP_RELAYS {
            let a = relay_peer_id(relay).unwrap();
            let b = relay_peer_id(relay).unwrap();
            assert_eq!(a, b, "slot {}", relay.slot);
        }
    }
}
