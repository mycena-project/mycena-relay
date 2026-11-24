// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Stable account identity (SD-190 spike) — long-lived signing key separate from rotatable transport PeerId.

use libp2p::identity::{Keypair, PeerId, PublicKey};

/// Vault secret name for the stable account Ed25519 key (see `mycena_node::secrets`).
pub const ACCOUNT_SECRET: &str = "account_ed25519";

pub fn account_id_from_keypair(keypair: &Keypair) -> String {
    keypair.public().to_peer_id().to_string()
}

pub fn invite_v3_signable_bytes(
    account_id: &str,
    peer_id: &str,
    enc_pubkey_hex: &str,
    epoch: u64,
    ratchet: Option<&crate::crypto::ratchet::PrekeyBundle>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"MYCENA/INVITE/v3\0");
    bytes.extend_from_slice(account_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(peer_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(enc_pubkey_hex.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&epoch.to_le_bytes());
    if let Some(b) = ratchet {
        bytes.extend_from_slice(b"MYCENA/RATCHET\0");
        bytes.extend_from_slice(&b.registration_id.to_le_bytes());
        bytes.extend_from_slice(&b.signed_prekey_id.to_le_bytes());
        bytes.extend_from_slice(&b.signed_prekey);
        bytes.extend_from_slice(&b.signed_prekey_sig);
        match (b.one_time_prekey_id, &b.one_time_prekey) {
            (Some(id), Some(pk)) => {
                bytes.push(1);
                bytes.extend_from_slice(&id.to_le_bytes());
                bytes.extend_from_slice(pk);
            }
            _ => bytes.push(0),
        }
    }
    bytes
}

pub fn contact_hello_signable_bytes(
    peer_id: &str,
    account_id: &str,
    display_name: &str,
    enc_pubkey_hex: &str,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"MYCENA/CONTACT/v1\0");
    bytes.extend_from_slice(peer_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(account_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(display_name.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(enc_pubkey_hex.as_bytes());
    bytes
}

pub fn identity_rotation_signable_bytes(
    account_id: &str,
    previous_peer_id: &str,
    new_peer_id: &str,
    epoch: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"MYCENA/IDENTITY/v2\0");
    bytes.extend_from_slice(account_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(previous_peer_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(new_peer_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&epoch.to_le_bytes());
    bytes
}

pub fn sign_account_message(keypair: &Keypair, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    keypair
        .sign(bytes)
        .map_err(|e| anyhow::anyhow!("account sign failed: {e}"))
}

pub fn verify_account_signature(account_id: &str, bytes: &[u8], signature: &[u8]) -> bool {
    if signature.is_empty() {
        return false;
    }
    let Ok(peer_id) = account_id.parse::<PeerId>() else {
        return false;
    };
    let Some(pk) = public_key_from_peer_id(&peer_id) else {
        return false;
    };
    pk.verify(bytes, signature)
}

pub fn public_key_from_peer_id(peer_id: &PeerId) -> Option<PublicKey> {
    let mh = peer_id.as_ref();
    if mh.code() != 0 {
        return None;
    }
    PublicKey::try_decode_protobuf(mh.digest()).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn account_sign_verify_roundtrip() {
        let kp = Keypair::generate_ed25519();
        let account_id = account_id_from_keypair(&kp);
        let bytes = invite_v3_signable_bytes(&account_id, "12D3KooWpeer", &"a".repeat(64), 1, None);
        let sig = sign_account_message(&kp, &bytes).unwrap();
        assert!(verify_account_signature(&account_id, &bytes, &sig));
        assert!(!verify_account_signature(&account_id, &bytes, &[0u8; 64]));
    }
}
