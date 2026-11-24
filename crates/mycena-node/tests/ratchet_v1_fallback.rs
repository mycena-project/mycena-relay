// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Ratchet v1 encrypt/decrypt with legacy v0 fallback on the wire.

use mycena_core::crypto::ratchet::{
    self, LocalPrekeyStore, RatchetSession, E2E_VERSION_LEGACY, E2E_VERSION_RATCHET,
};
use mycena_core::crypto::{encrypt_for, EncryptionKeypair};
use mycena_core::e2e_direct_wrap;

#[test]
fn ratchet_v1_roundtrip_and_legacy_v0_still_works() {
    let alice = EncryptionKeypair::generate();
    let bob = EncryptionKeypair::generate();
    let mut bob_prekeys = LocalPrekeyStore::generate(&bob, 4);
    let bundle = bob_prekeys.public_bundle(&bob);

    let author = "12D3KooWAlice";
    let app = b"ratchet chat payload";
    let wrapped = e2e_direct_wrap::wrap_inner(author, app);

    let (mut alice_sess, init_wire) =
        RatchetSession::init_outbound("bob".into(), &alice, &bundle, &wrapped).expect("init");
    assert_eq!(
        ratchet::wire_e2e_version(&init_wire, E2E_VERSION_RATCHET),
        1
    );

    let (mut bob_sess, plain1) =
        RatchetSession::init_inbound("alice".into(), &bob, &mut bob_prekeys, &init_wire)
            .expect("init in");
    let (logical, app_bytes) = e2e_direct_wrap::unwrap_inner(&plain1, "relay");
    assert_eq!(logical, author);
    assert_eq!(app_bytes, app);

    let (wire2, _) = bob_sess.encrypt(b"reply").expect("enc");
    let plain2 = alice_sess.decrypt(&wire2).expect("dec");
    assert_eq!(plain2, b"reply");

    // Legacy peer still decrypts v0 blobs.
    let legacy = encrypt_for(b"legacy-only", &bob.public).unwrap();
    let legacy_plain =
        ratchet::decrypt_direct_e2e(&legacy, E2E_VERSION_LEGACY, None, &bob).unwrap();
    assert_eq!(legacy_plain, b"legacy-only");

    let _ = alice_sess;
    let _ = bob_sess;
}
