// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! BLE libp2p multiaddrs (ADR 002 F3): `/ble/<device>/gatt/<uuid>/p2p/<peer-id>`.

pub const DEFAULT_GATT_SERVICE: &str = "6e400001-b5a3-f393-e0a9-e50e24dcca9e";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleMultiaddr {
    pub device_id: String,
    pub gatt_service: String,
    pub peer_id: String,
}

/// Normalize MAC / device id (strip colons, lowercase).
pub fn normalize_device_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| *c != ':')
        .collect::<String>()
        .to_lowercase()
}

/// Build a canonical BLE dial multiaddr.
pub fn build_ble_multiaddr(device_id: &str, peer_id: &str) -> String {
    build_ble_multiaddr_with_service(device_id, DEFAULT_GATT_SERVICE, peer_id)
}

pub fn build_ble_multiaddr_with_service(
    device_id: &str,
    gatt_service: &str,
    peer_id: &str,
) -> String {
    format!(
        "/ble/{}/gatt/{}/p2p/{}",
        normalize_device_id(device_id),
        gatt_service,
        peer_id
    )
}

pub fn is_ble_multiaddr(addr: &str) -> bool {
    addr.starts_with("/ble/")
}

/// Parse `/ble/<device>/gatt/<uuid>/p2p/<peer-id>`.
pub fn parse_ble_multiaddr(addr: &str) -> Option<BleMultiaddr> {
    let mut parts = addr.trim().split('/');
    if !parts.next()?.is_empty() {
        return None;
    }
    if parts.next()? != "ble" {
        return None;
    }
    let device_id = normalize_device_id(parts.next()?);
    if parts.next()? != "gatt" {
        return None;
    }
    let gatt_service = parts.next()?.to_string();
    if parts.next()? != "p2p" {
        return None;
    }
    let peer_id = parts.next()?.to_string();
    if peer_id.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(BleMultiaddr {
        device_id,
        gatt_service,
        peer_id,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn roundtrip_ble_multiaddr() {
        let addr = build_ble_multiaddr("AA:BB:CC:DD:EE:FF", "12D3KooWExample");
        assert!(is_ble_multiaddr(&addr));
        let parsed = parse_ble_multiaddr(&addr).expect("parse");
        assert_eq!(parsed.device_id, "aabbccddeeff");
        assert_eq!(parsed.gatt_service, DEFAULT_GATT_SERVICE);
        assert_eq!(parsed.peer_id, "12D3KooWExample");
    }

    #[test]
    fn rejects_non_ble() {
        assert!(!is_ble_multiaddr("/ip4/127.0.0.1/tcp/7761/p2p/abc"));
        assert!(parse_ble_multiaddr("/ip4/1.2.3.4/tcp/80").is_none());
    }
}
