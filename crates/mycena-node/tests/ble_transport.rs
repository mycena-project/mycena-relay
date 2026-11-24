// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! BLE libp2p overlay multiaddr format (ADR 002 F3).

use mycena_core::ble_multiaddr::{build_ble_multiaddr, is_ble_multiaddr, parse_ble_multiaddr};

#[test]
fn ble_multiaddr_parse_and_build() {
    let addr = build_ble_multiaddr("AA:BB:CC:DD:EE:FF", "12D3KooWExample");
    assert!(is_ble_multiaddr(&addr));
    let parsed = parse_ble_multiaddr(&addr).expect("parse");
    assert_eq!(parsed.device_id, "aabbccddeeff");
    assert_eq!(parsed.peer_id, "12D3KooWExample");
}
