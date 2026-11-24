// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! PeerInfo protocol capability wire compatibility.

use mycena_core::protocol_capabilities::ProtocolCapabilities;
use mycena_core::transport::WireMessage;

#[test]
fn peerinfo_cbor_roundtrip_with_capabilities() {
    let msg = WireMessage::PeerInfo {
        enc_pubkey_hex: "aabb".into(),
        display_name: Some("test".into()),
        supported_scopes: vec!["mycena/chat".into()],
        account_id: None,
        transport_epoch: 1,
        ratchet_registration_id: None,
        ratchet_signed_prekey_id: None,
        ratchet_signed_prekey_hex: None,
        ratchet_signed_prekey_sig_hex: None,
        ratchet_one_time_prekey_id: None,
        ratchet_one_time_prekey_hex: None,
        protocol_capabilities: ProtocolCapabilities::current(true, true),
    };
    let bytes = serde_cbor::to_vec(&msg).expect("encode");
    let decoded: WireMessage = serde_cbor::from_slice(&bytes).expect("decode");
    match decoded {
        WireMessage::PeerInfo {
            protocol_capabilities,
            ..
        } => {
            assert!(protocol_capabilities.ratchet_max >= 1);
            assert!(protocol_capabilities.custody_max >= 1);
        }
        _ => panic!("expected PeerInfo"),
    }
}

#[test]
fn peerinfo_cbor_legacy_without_capabilities_field() {
    #[derive(serde::Serialize)]
    struct LegacyPeerInfoBody {
        enc_pubkey_hex: String,
        display_name: Option<String>,
        supported_scopes: Vec<String>,
    }
    #[derive(serde::Serialize)]
    struct LegacyWire {
        #[serde(rename = "PeerInfo")]
        peer_info: LegacyPeerInfoBody,
    }
    let legacy = LegacyWire {
        peer_info: LegacyPeerInfoBody {
            enc_pubkey_hex: "deadbeef".into(),
            display_name: None,
            supported_scopes: vec![],
        },
    };
    let bytes = serde_cbor::to_vec(&legacy).expect("encode legacy");
    let decoded: WireMessage = serde_cbor::from_slice(&bytes).expect("decode legacy peerinfo");
    match decoded {
        WireMessage::PeerInfo {
            protocol_capabilities,
            ..
        } => {
            assert_eq!(protocol_capabilities, ProtocolCapabilities::legacy_client());
        }
        _ => panic!("expected PeerInfo"),
    }
}

#[test]
fn downgrade_old_remote_blocks_ratchet_encrypt_path() {
    let local = ProtocolCapabilities::current(true, true);
    let remote = ProtocolCapabilities::legacy_client();
    let negotiated = ProtocolCapabilities::negotiate(&local, &remote);
    assert!(!negotiated.ratchet_v1_allowed());
}

#[test]
fn upgrade_both_new_enables_ratchet_and_custody() {
    let local = ProtocolCapabilities::current(true, true);
    let remote = ProtocolCapabilities::current(true, true);
    let negotiated = ProtocolCapabilities::negotiate(&local, &remote);
    assert!(negotiated.ratchet_v1_allowed());
    assert!(negotiated.custody_v1_allowed());
}
