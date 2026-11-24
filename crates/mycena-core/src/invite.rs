// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Compact invite payloads for QR codes and deep links (all Mycena clients).

use crate::crypto::ratchet::{self, PrekeyBundle};

const PREFIX_V1: &str = "mycena://invite/v1#";
const PREFIX_V2: &str = "mycena://invite/v2#";
const PREFIX_V3: &str = "mycena://invite/v3#";

/// Peer identity plus optional E2E key and direct dial hint (invite v2/v3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInviteInfo {
    pub peer_id: String,
    /// Stable account id (v3) — maps across transport PeerId rotation.
    pub account_id: Option<String>,
    /// Transport epoch from signed invite (v3).
    pub transport_epoch: u64,
    /// True when a v3 signature was verified at parse time.
    pub signature_valid: bool,
    /// X25519 public key hex from invite v2/v3 — seeds E2E without waiting for libp2p PeerInfo.
    pub enc_pubkey_hex: Option<String>,
    /// Optional LAN/direct multiaddr (same Wi‑Fi); tried before relay circuit.
    pub direct_multiaddr: Option<String>,
    /// Optional Double-Ratchet prekey bundle (v3 tail) — enables v1 E2E on first message after QR.
    pub ratchet_bundle: Option<PrekeyBundle>,
}

impl PeerInviteInfo {
    pub fn dial_multiaddrs(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(ref direct) = self.direct_multiaddr {
            if !direct.trim().is_empty() {
                out.push(direct.clone());
            }
        }
        if let Some(circuit) = crate::bootstrap::relay_circuit_multiaddr(&self.peer_id) {
            if !out.iter().any(|a| a == &circuit) {
                out.push(circuit);
            }
        }
        out
    }
}

/// QR-friendly invite (peer id only; dial via default relay circuit).
pub fn encode_invite(peer_id: &str) -> String {
    format!("{PREFIX_V1}{}", peer_id.trim())
}

/// Preferred QR invite: peer id + E2E public key (+ optional direct multiaddr on same Wi‑Fi).
pub fn encode_invite_v2(
    peer_id: &str,
    enc_pubkey_hex: &str,
    direct_multiaddr: Option<&str>,
) -> String {
    let peer_id = peer_id.trim();
    let enc = enc_pubkey_hex.trim();
    match direct_multiaddr.map(str::trim).filter(|s| !s.is_empty()) {
        Some(addr) => format!("{PREFIX_V2}{peer_id}|{enc}|{addr}"),
        None => format!("{PREFIX_V2}{peer_id}|{enc}"),
    }
}

/// Preferred signed QR invite (Phase C2): account + transport peer + enc key + epoch.
pub fn encode_signed_invite_v3(
    account_id: &str,
    peer_id: &str,
    enc_pubkey_hex: &str,
    epoch: u64,
    account_keypair: &libp2p::identity::Keypair,
    direct_multiaddr: Option<&str>,
    ratchet_bundle: Option<&PrekeyBundle>,
) -> anyhow::Result<String> {
    let account_id = account_id.trim();
    let peer_id = peer_id.trim();
    let enc = enc_pubkey_hex.trim();
    if !valid_enc_pubkey_hex(enc) {
        anyhow::bail!("invalid enc_pubkey_hex");
    }
    if let Some(bundle) = ratchet_bundle {
        let enc_bytes = hex::decode(enc)?;
        if enc_bytes.len() != 32 {
            anyhow::bail!("invalid enc_pubkey_hex");
        }
        let mut enc_arr = [0u8; 32];
        enc_arr.copy_from_slice(&enc_bytes);
        if bundle.identity_key != enc_arr {
            anyhow::bail!("ratchet identity_key must match enc_pubkey_hex");
        }
        if !ratchet::verify_prekey_bundle(bundle) {
            anyhow::bail!("invalid ratchet prekey bundle");
        }
    }
    let bytes = crate::account_identity::invite_v3_signable_bytes(
        account_id,
        peer_id,
        enc,
        epoch,
        ratchet_bundle,
    );
    let sig = crate::account_identity::sign_account_message(account_keypair, &bytes)?;
    let sig_hex = hex::encode(sig);
    let direct = direct_multiaddr.map(str::trim).filter(|s| !s.is_empty());
    let mut body = match direct {
        Some(addr) => format!("{PREFIX_V3}{account_id}|{peer_id}|{enc}|{epoch}|{sig_hex}|{addr}"),
        None => format!("{PREFIX_V3}{account_id}|{peer_id}|{enc}|{epoch}|{sig_hex}"),
    };
    if let Some(bundle) = ratchet_bundle {
        body.push('|');
        body.push_str(&format_bundle_tail(bundle));
    }
    Ok(body)
}

fn format_bundle_tail(bundle: &PrekeyBundle) -> String {
    let mut out = format!(
        "{}|{}|{}|{}",
        bundle.registration_id,
        bundle.signed_prekey_id,
        hex::encode(bundle.signed_prekey),
        hex::encode(bundle.signed_prekey_sig),
    );
    if let (Some(id), Some(pk)) = (bundle.one_time_prekey_id, bundle.one_time_prekey) {
        out.push('|');
        out.push_str(&id.to_string());
        out.push('|');
        out.push_str(&hex::encode(pk));
    }
    out
}

/// Parse invite QR, multiaddr, or legacy `peerId@host:port` form.
pub fn parse_invite(raw: &str) -> Option<InviteTarget> {
    let input = raw.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(rest) = input.strip_prefix(PREFIX_V3) {
        return parse_invite_v3(rest);
    }
    if let Some(rest) = input.strip_prefix(PREFIX_V2) {
        return parse_invite_v2(rest);
    }
    if let Some(peer_id) = input.strip_prefix(PREFIX_V1) {
        let peer_id = peer_id.trim();
        if peer_id.is_empty() {
            return None;
        }
        return Some(InviteTarget::Peer(Box::new(PeerInviteInfo {
            peer_id: peer_id.to_string(),
            account_id: None,
            transport_epoch: 0,
            signature_valid: false,
            enc_pubkey_hex: None,
            direct_multiaddr: None,
            ratchet_bundle: None,
        })));
    }
    if input.starts_with('/') {
        return Some(InviteTarget::Multiaddr(input.to_string()));
    }
    if looks_like_peer_id(input) {
        return Some(InviteTarget::Peer(Box::new(PeerInviteInfo {
            peer_id: input.to_string(),
            account_id: None,
            transport_epoch: 0,
            signature_valid: false,
            enc_pubkey_hex: None,
            direct_multiaddr: None,
            ratchet_bundle: None,
        })));
    }
    legacy_host_port_to_multiaddr(input)
}

fn parse_invite_v2(rest: &str) -> Option<InviteTarget> {
    let parts: Vec<&str> = rest.splitn(3, '|').collect();
    if parts.len() < 2 {
        return None;
    }
    let peer_id = parts[0].trim();
    let enc = parts[1].trim();
    if peer_id.is_empty() || !valid_enc_pubkey_hex(enc) {
        return None;
    }
    let direct = parts
        .get(2)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(InviteTarget::Peer(Box::new(PeerInviteInfo {
        peer_id: peer_id.to_string(),
        account_id: None,
        transport_epoch: 0,
        signature_valid: false,
        enc_pubkey_hex: Some(enc.to_string()),
        direct_multiaddr: direct,
        ratchet_bundle: None,
    })))
}

fn parse_v3_tail(tail: &[&str]) -> (Option<String>, Option<PrekeyBundle>) {
    if tail.is_empty() {
        return (None, None);
    }
    let mut idx = 0;
    let direct = if tail[0].starts_with('/') {
        idx += 1;
        Some(tail[0].to_string())
    } else {
        None
    };
    let ratchet_parts = &tail[idx..];
    let bundle = parse_ratchet_tail(ratchet_parts);
    (direct, bundle)
}

fn parse_ratchet_tail(parts: &[&str]) -> Option<PrekeyBundle> {
    if parts.len() != 4 && parts.len() != 6 {
        return None;
    }
    let registration_id: u32 = parts[0].trim().parse().ok()?;
    let signed_prekey_id: u32 = parts[1].trim().parse().ok()?;
    let signed_prekey_hex = parts[2].trim();
    let signed_prekey_sig_hex = parts[3].trim();
    let (one_time_prekey_id, one_time_prekey_hex) = if parts.len() == 6 {
        (Some(parts[4].trim().parse().ok()?), Some(parts[5].trim()))
    } else {
        (None, None)
    };
    ratchet::prekey_bundle_from_peer_info(
        registration_id,
        [0u8; 32], // identity filled by caller after enc match
        signed_prekey_id,
        signed_prekey_hex,
        signed_prekey_sig_hex,
        one_time_prekey_id,
        one_time_prekey_hex,
    )
    .ok()
}

fn parse_invite_v3(rest: &str) -> Option<InviteTarget> {
    let parts: Vec<&str> = rest.split('|').collect();
    if parts.len() < 5 {
        return None;
    }
    let account_id = parts[0].trim();
    let peer_id = parts[1].trim();
    let enc = parts[2].trim();
    let epoch: u64 = parts[3].trim().parse().ok()?;
    let sig_hex = parts[4].trim();
    if account_id.is_empty() || peer_id.is_empty() || !valid_enc_pubkey_hex(enc) {
        return None;
    }
    let enc_bytes = hex::decode(enc).ok()?;
    if enc_bytes.len() != 32 {
        return None;
    }
    let mut identity_key = [0u8; 32];
    identity_key.copy_from_slice(&enc_bytes);

    let (direct, mut ratchet_bundle) = parse_v3_tail(&parts[5..]);
    if let Some(ref mut bundle) = ratchet_bundle {
        bundle.identity_key = identity_key;
        if !ratchet::verify_prekey_bundle(bundle) {
            return None;
        }
    }

    let sig = hex::decode(sig_hex).ok()?;
    let bytes = crate::account_identity::invite_v3_signable_bytes(
        account_id,
        peer_id,
        enc,
        epoch,
        ratchet_bundle.as_ref(),
    );
    let signature_valid =
        crate::account_identity::verify_account_signature(account_id, &bytes, &sig);
    if !signature_valid {
        return None;
    }
    Some(InviteTarget::Peer(Box::new(PeerInviteInfo {
        peer_id: peer_id.to_string(),
        account_id: Some(account_id.to_string()),
        transport_epoch: epoch,
        signature_valid: true,
        enc_pubkey_hex: Some(enc.to_string()),
        direct_multiaddr: direct,
        ratchet_bundle,
    })))
}

fn valid_enc_pubkey_hex(enc: &str) -> bool {
    enc.len() == 64 && enc.chars().all(|c| c.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteTarget {
    Peer(Box<PeerInviteInfo>),
    Multiaddr(String),
}

impl InviteTarget {
    pub fn peer_id(&self) -> Option<&str> {
        match self {
            InviteTarget::Peer(info) => Some(info.peer_id.as_str()),
            InviteTarget::Multiaddr(_) => None,
        }
    }

    pub fn to_dial_multiaddr(&self) -> Option<String> {
        match self {
            InviteTarget::Multiaddr(addr) => Some(addr.clone()),
            InviteTarget::Peer(info) => info.dial_multiaddrs().into_iter().next(),
        }
    }

    pub fn dial_multiaddrs(&self) -> Vec<String> {
        match self {
            InviteTarget::Multiaddr(addr) => vec![addr.clone()],
            InviteTarget::Peer(info) => info.dial_multiaddrs(),
        }
    }
}

fn looks_like_peer_id(s: &str) -> bool {
    s.len() >= 40 && s.starts_with("12D3Koo")
}

fn legacy_host_port_to_multiaddr(input: &str) -> Option<InviteTarget> {
    let parts = input.split('@').collect::<Vec<_>>();
    if parts.len() != 2 {
        return None;
    }
    let peer_id = parts[0].trim();
    let host_and_port = parts[1].split(':').collect::<Vec<_>>();
    if peer_id.is_empty() || host_and_port.len() != 2 {
        return None;
    }
    let host = host_and_port[0];
    let port = host_and_port[1];
    if port.parse::<u16>().is_err() {
        return None;
    }
    Some(InviteTarget::Multiaddr(format!(
        "/ip4/{host}/tcp/{port}/p2p/{peer_id}"
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn roundtrip_invite_v1() {
        let id = "12D3KooWGv6goWd2fHwcigDjqQm5Dfw28UijwEjnMcYwTpPRPZy6";
        let encoded = encode_invite(id);
        match parse_invite(&encoded).unwrap() {
            InviteTarget::Peer(info) => {
                assert_eq!(info.peer_id, id);
                assert!(info.enc_pubkey_hex.is_none());
            }
            _ => panic!("expected peer"),
        }
    }

    #[test]
    fn roundtrip_invite_v2_with_direct() {
        let id = "12D3KooWGv6goWd2fHwcigDjqQm5Dfw28UijwEjnMcYwTpPRPZy6";
        let enc = "a".repeat(64);
        let direct = "/ip4/192.168.1.5/tcp/7761/p2p/12D3KooWOther";
        let encoded = encode_invite_v2(id, &enc, Some(direct));
        match parse_invite(&encoded).unwrap() {
            InviteTarget::Peer(info) => {
                assert_eq!(info.peer_id, id);
                assert_eq!(info.enc_pubkey_hex.as_deref(), Some(enc.as_str()));
                assert_eq!(info.direct_multiaddr.as_deref(), Some(direct));
                let addrs = info.dial_multiaddrs();
                assert_eq!(addrs.first().map(String::as_str), Some(direct));
                assert!(addrs.len() >= 2);
            }
            _ => panic!("expected peer"),
        }
    }

    #[test]
    fn roundtrip_signed_invite_v3() {
        use libp2p::identity::Keypair;
        let kp = Keypair::generate_ed25519();
        let account_id = crate::account_identity::account_id_from_keypair(&kp);
        let peer_id = "12D3KooWGv6goWd2fHwcigDjqQm5Dfw28UijwEjnMcYwTpPRPZy6";
        let enc = "b".repeat(64);
        let encoded =
            encode_signed_invite_v3(&account_id, peer_id, &enc, 2, &kp, None, None).unwrap();
        match parse_invite(&encoded).unwrap() {
            InviteTarget::Peer(info) => {
                assert_eq!(info.peer_id, peer_id);
                assert_eq!(info.account_id.as_deref(), Some(account_id.as_str()));
                assert!(info.signature_valid);
                assert_eq!(info.transport_epoch, 2);
            }
            _ => panic!("expected peer"),
        }
        let tampered = encoded.replace('b', "c");
        assert!(parse_invite(&tampered).is_none());
    }

    #[test]
    fn roundtrip_signed_invite_v3_with_ratchet() {
        use crate::crypto::{ratchet::LocalPrekeyStore, EncryptionKeypair};
        use libp2p::identity::Keypair;
        let kp = Keypair::generate_ed25519();
        let account_id = crate::account_identity::account_id_from_keypair(&kp);
        let peer_id = "12D3KooWGv6goWd2fHwcigDjqQm5Dfw28UijwEjnMcYwTpPRPZy6";
        let enc_kp = EncryptionKeypair::generate();
        let enc = enc_kp.public_hex();
        let prekeys = LocalPrekeyStore::generate(&enc_kp, 4);
        let bundle = prekeys.public_bundle(&enc_kp);
        let encoded =
            encode_signed_invite_v3(&account_id, peer_id, &enc, 2, &kp, None, Some(&bundle))
                .unwrap();
        match parse_invite(&encoded).unwrap() {
            InviteTarget::Peer(info) => {
                assert!(info.signature_valid);
                assert!(info.ratchet_bundle.is_some());
                assert_eq!(
                    info.ratchet_bundle.as_ref().unwrap().registration_id,
                    bundle.registration_id
                );
            }
            _ => panic!("expected peer"),
        }
    }

    #[test]
    fn rejects_malformed_v3() {
        assert!(parse_invite("mycena://invite/v3#bad|data").is_none());
    }

    #[test]
    fn parses_dns4_multiaddr() {
        let addr = "/dns4/mycena-relay.fly.dev/tcp/4001/p2p/12D3KooW/p2p-circuit/p2p/12D3KooWOther";
        match parse_invite(addr).unwrap() {
            InviteTarget::Multiaddr(m) => assert_eq!(m, addr),
            _ => panic!("expected multiaddr"),
        }
    }
}
