// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

/// Placeholder until [ContactHello](crate::contact_hello) supplies a human display name.
pub const PENDING_DISPLAY_NAME: &str = "";

pub fn looks_like_peer_id_stub(name: &str, peer: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return true;
    }
    if name == peer {
        return true;
    }
    if peer.len() >= 12 && name == peer.chars().take(12).collect::<String>() {
        return true;
    }
    if peer.len() >= 16 && name == peer.chars().take(16).collect::<String>() {
        return true;
    }
    name.starts_with("12D3") && name.len() <= 20
}
