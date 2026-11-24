// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Invite parsing helpers used by transport-only vs full contact connect paths.

use libp2p::identity::Keypair;
use mycena_l0_facade::account_identity;
use mycena_l0_facade::invite::{encode_signed_invite_v3, parse_invite, InviteTarget};

#[test]
fn signed_v3_invite_exposes_dial_targets_for_ble_handoff() {
    let kp = Keypair::generate_ed25519();
    let account_id = account_identity::account_id_from_keypair(&kp);
    let peer_id = "12D3KooWGv6goWd2fHwcigDjqQm5Dfw28UijwEjnMcYwTpPRPZy6";
    let enc = "b".repeat(64);
    let encoded = encode_signed_invite_v3(&account_id, peer_id, &enc, 2, &kp, None, None).unwrap();
    match parse_invite(&encoded).unwrap() {
        InviteTarget::Peer(info) => {
            assert_eq!(info.peer_id, peer_id);
            assert!(info.enc_pubkey_hex.is_some());
            assert!(!info.dial_multiaddrs().is_empty());
        }
        _ => panic!("expected peer invite"),
    }
}
