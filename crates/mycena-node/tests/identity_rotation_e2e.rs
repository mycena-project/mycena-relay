// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Transport identity rotation (SD-190).

use libp2p::identity::Keypair;
use mycena_core::identity_rotation::IdentityRotationGossip;

#[test]
fn identity_rotation_sign_verify_v2() {
    let kp = Keypair::generate_ed25519();
    let account_id = mycena_core::account_identity::account_id_from_keypair(&kp);
    let rot = IdentityRotationGossip::sign(
        &account_id,
        "12D3KooWOldTransportPeerIdTest",
        "12D3KooWNewTransportPeerIdTest",
        3,
        &kp,
    )
    .unwrap();
    assert!(rot.verify());
    let e2e = rot.encode_e2e_announce().unwrap();
    let decoded = IdentityRotationGossip::decode_e2e_announce(&e2e).unwrap();
    assert_eq!(decoded, rot);
}
