// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Per-peer protocol capability advertisement and negotiation (SD-140).
//!
//! Exchanged in [`crate::transport::WireMessage::PeerInfo`] on first contact.

use crate::bearer_transport::{default_wire_version, BearerWireVersion};
use crate::crypto::ratchet::{E2E_VERSION_LEGACY, E2E_VERSION_RATCHET};
use serde::{Deserialize, Serialize};

/// Minimal bitfield helper without an extra dependency.
macro_rules! bitfield_like_flags {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident($ty:ty) {
            $(
                $(#[$flag_meta:meta])*
                const $flag:ident = $val:expr;
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
        #[serde(transparent)]
        $vis struct $name($ty);

        impl $name {
            $(
                $(#[$flag_meta])*
                pub const $flag: Self = Self($val);
            )*

            pub const fn empty() -> Self {
                Self(0)
            }

            pub const fn all_supported() -> Self {
                Self(0 $(| $val)*)
            }

            pub const fn contains(self, other: Self) -> bool {
                (self.0 & other.0) == other.0
            }

            pub const fn intersect(self, other: Self) -> Self {
                Self(self.0 & other.0)
            }
        }
    };
}

/// Schema version for the `ProtocolCapabilities` blob itself.
pub const PROTOCOL_CAPABILITIES_WIRE_VERSION: u16 = 1;

/// E2E ratchet generations (`EncryptedDirect::e2e_version` family).
pub mod ratchet {
    pub const LEGACY_X25519: u8 = 0;
    pub const V1_DOUBLE_RATCHET: u8 = 1;
    /// Reserved — not implemented yet.
    pub const V2: u8 = 2;
}

/// Bearer framing (`MYBL` / `MYBR`).
pub mod bearer {
    pub const LEGACY_MYBL: u8 = 0;
    pub const V1_MYBR: u8 = 1;
}

/// Offline payment voucher gossip format.
pub mod payment_voucher {
    pub const NONE: u8 = 0;
    pub const V1: u8 = 1;
}

/// Mesh relay selection / fanout protocol generation.
pub mod relay {
    pub const LEGACY_FANOUT: u8 = 0;
    pub const V1_TRUST_SCORED: u8 = 1;
    pub const V2_DIVERSITY: u8 = 2;
}

/// DTN custody spray-and-wait.
pub mod custody {
    pub const NONE: u8 = 0;
    pub const V1: u8 = 1;
}

bitfield_like_flags!(
    /// Boolean features advertised alongside version fields.
    pub struct ProtocolCapabilityFlags(u32) {
        /// Wi‑Fi Direct bulk bearer upgrade path.
        const WIFI_DIRECT = 1 << 0;
        /// Signed mesh `Data` frames (SD-030).
        const SIGNED_DATA_V1 = 1 << 1;
        /// Peer diversity checks during relay selection.
        const RELAY_DIVERSITY = 1 << 2;
        /// WebRTC RTCDataChannel bearer (desktop / browser clients).
        const WEBRTC = 1 << 3;
    }
);

/// Capabilities this node supports (sent in `PeerInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    #[serde(default = "default_cap_wire_version")]
    pub wire_version: u16,
    #[serde(default)]
    pub ratchet_max: u8,
    #[serde(default)]
    pub bearer_wire_max: u8,
    #[serde(default)]
    pub payment_voucher_max: u8,
    #[serde(default)]
    pub relay_max: u8,
    #[serde(default)]
    pub custody_max: u8,
    #[serde(default)]
    pub flags: ProtocolCapabilityFlags,
}

fn default_cap_wire_version() -> u16 {
    PROTOCOL_CAPABILITIES_WIRE_VERSION
}

impl Default for ProtocolCapabilities {
    fn default() -> Self {
        Self::legacy_client()
    }
}

impl ProtocolCapabilities {
    /// Pre-capability clients: legacy X25519, MYBL, no custody/voucher extensions.
    pub const fn legacy_client() -> Self {
        Self {
            wire_version: 0,
            ratchet_max: ratchet::LEGACY_X25519,
            bearer_wire_max: bearer::LEGACY_MYBL,
            payment_voucher_max: payment_voucher::NONE,
            relay_max: relay::LEGACY_FANOUT,
            custody_max: custody::NONE,
            flags: ProtocolCapabilityFlags(0),
        }
    }

    /// Current Mycena release capabilities.
    pub fn current(custody_enabled: bool, coin_enabled: bool) -> Self {
        let bearer_wire_max = match default_wire_version() {
            BearerWireVersion::V1 => bearer::V1_MYBR,
            BearerWireVersion::Legacy => bearer::LEGACY_MYBL,
        };
        Self {
            wire_version: PROTOCOL_CAPABILITIES_WIRE_VERSION,
            ratchet_max: ratchet::V1_DOUBLE_RATCHET,
            bearer_wire_max,
            payment_voucher_max: if coin_enabled {
                payment_voucher::V1
            } else {
                payment_voucher::NONE
            },
            relay_max: relay::V2_DIVERSITY,
            custody_max: if custody_enabled {
                custody::V1
            } else {
                custody::NONE
            },
            flags: ProtocolCapabilityFlags::all_supported(),
        }
    }

    /// Negotiate the highest mutually supported level per capability.
    pub fn negotiate(local: &Self, remote: &Self) -> NegotiatedProtocol {
        NegotiatedProtocol {
            ratchet: local.ratchet_max.min(remote.ratchet_max),
            bearer_wire: local.bearer_wire_max.min(remote.bearer_wire_max),
            payment_voucher: local.payment_voucher_max.min(remote.payment_voucher_max),
            relay: local.relay_max.min(remote.relay_max),
            custody: local.custody_max.min(remote.custody_max),
            flags: local.flags.intersect(remote.flags),
        }
    }
}

/// Per-peer negotiated protocol levels (stored after `PeerInfo` exchange).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NegotiatedProtocol {
    pub ratchet: u8,
    pub bearer_wire: u8,
    pub payment_voucher: u8,
    pub relay: u8,
    pub custody: u8,
    pub flags: ProtocolCapabilityFlags,
}

impl NegotiatedProtocol {
    pub fn from_legacy_assumption() -> Self {
        Self {
            ratchet: ratchet::LEGACY_X25519,
            bearer_wire: bearer::LEGACY_MYBL,
            payment_voucher: payment_voucher::NONE,
            relay: relay::LEGACY_FANOUT,
            custody: custody::NONE,
            flags: ProtocolCapabilityFlags(0),
        }
    }

    pub fn ratchet_v1_allowed(&self) -> bool {
        self.ratchet >= ratchet::V1_DOUBLE_RATCHET
    }

    pub fn custody_v1_allowed(&self) -> bool {
        self.custody >= custody::V1
    }

    pub fn payment_voucher_v1_allowed(&self) -> bool {
        self.payment_voucher >= payment_voucher::V1
    }

    pub fn preferred_e2e_version(&self) -> u8 {
        if self.ratchet_v1_allowed() {
            E2E_VERSION_RATCHET
        } else {
            E2E_VERSION_LEGACY
        }
    }

    pub fn relay_diversity_allowed(&self) -> bool {
        self.relay >= relay::V2_DIVERSITY
            && self
                .flags
                .contains(ProtocolCapabilityFlags::RELAY_DIVERSITY)
    }

    pub fn wifi_direct_allowed(&self) -> bool {
        self.flags.contains(ProtocolCapabilityFlags::WIFI_DIRECT)
    }

    pub fn webrtc_allowed(&self) -> bool {
        self.flags.contains(ProtocolCapabilityFlags::WEBRTC)
    }

    pub fn bearer_wire_version(&self) -> BearerWireVersion {
        if self.bearer_wire >= bearer::V1_MYBR {
            BearerWireVersion::V1
        } else {
            BearerWireVersion::Legacy
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn new_caps() -> ProtocolCapabilities {
        ProtocolCapabilities::current(true, true)
    }

    fn old_caps() -> ProtocolCapabilities {
        ProtocolCapabilities::legacy_client()
    }

    #[test]
    fn legacy_default_matches_old_client() {
        assert_eq!(ProtocolCapabilities::default(), old_caps());
    }

    #[test]
    fn negotiate_old_local_new_remote() {
        let n = ProtocolCapabilities::negotiate(&old_caps(), &new_caps());
        assert_eq!(n.ratchet, ratchet::LEGACY_X25519);
        assert_eq!(n.custody, custody::NONE);
        assert_eq!(n.payment_voucher, payment_voucher::NONE);
        assert_eq!(n.relay, relay::LEGACY_FANOUT);
        assert_eq!(n.preferred_e2e_version(), E2E_VERSION_LEGACY);
        assert!(!n.custody_v1_allowed());
    }

    #[test]
    fn negotiate_new_local_old_remote() {
        let n = ProtocolCapabilities::negotiate(&new_caps(), &old_caps());
        assert_eq!(n.ratchet, ratchet::LEGACY_X25519);
        assert_eq!(n.custody, custody::NONE);
        assert!(!n.ratchet_v1_allowed());
    }

    #[test]
    fn negotiate_both_current() {
        let n = ProtocolCapabilities::negotiate(&new_caps(), &new_caps());
        assert_eq!(n.ratchet, ratchet::V1_DOUBLE_RATCHET);
        assert_eq!(n.custody, custody::V1);
        assert_eq!(n.payment_voucher, payment_voucher::V1);
        assert_eq!(n.relay, relay::V2_DIVERSITY);
        assert!(n.ratchet_v1_allowed());
        assert!(n.custody_v1_allowed());
        assert!(n.relay_diversity_allowed());
        assert_eq!(n.preferred_e2e_version(), E2E_VERSION_RATCHET);
    }

    #[test]
    fn negotiate_partial_ratchet() {
        let mut peer_a = new_caps();
        let mut peer_b = new_caps();
        peer_a.ratchet_max = ratchet::V1_DOUBLE_RATCHET;
        peer_b.ratchet_max = ratchet::V2;
        let n = ProtocolCapabilities::negotiate(&peer_a, &peer_b);
        assert_eq!(n.ratchet, ratchet::V1_DOUBLE_RATCHET);
    }

    #[test]
    fn negotiate_custody_only_when_both_support() {
        let mut with = new_caps();
        let mut without = new_caps();
        without.custody_max = custody::NONE;
        assert!(!ProtocolCapabilities::negotiate(&with, &without).custody_v1_allowed());
        with.custody_max = custody::NONE;
        assert!(!ProtocolCapabilities::negotiate(&with, &without).custody_v1_allowed());
        assert!(ProtocolCapabilities::negotiate(&new_caps(), &new_caps()).custody_v1_allowed());
    }

    #[test]
    fn negotiate_relay_diversity_requires_flag_and_version() {
        let mut no_flag = new_caps();
        no_flag.flags = ProtocolCapabilityFlags(0);
        assert!(!ProtocolCapabilities::negotiate(&new_caps(), &no_flag).relay_diversity_allowed());
        let mut low_relay = new_caps();
        low_relay.relay_max = relay::V1_TRUST_SCORED;
        assert!(!ProtocolCapabilities::negotiate(&new_caps(), &low_relay).relay_diversity_allowed());
    }

    #[test]
    fn serde_roundtrip_current() {
        let caps = new_caps();
        let json = serde_json::to_string(&caps).unwrap();
        let back: ProtocolCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);
    }

    #[test]
    fn serde_old_peerinfo_without_capabilities_field() {
        #[derive(Deserialize)]
        struct OldPeerInfo {
            #[allow(dead_code)]
            enc_pubkey_hex: String,
            #[serde(default)]
            protocol_capabilities: ProtocolCapabilities,
        }
        let json = r#"{"enc_pubkey_hex":"abcd"}"#;
        let parsed: OldPeerInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.protocol_capabilities, old_caps());
    }

    #[test]
    fn negotiate_wifi_and_bearer_wire_helpers() {
        use crate::bearer_transport::BearerWireVersion;

        let legacy = NegotiatedProtocol::from_legacy_assumption();
        assert_eq!(legacy.bearer_wire_version(), BearerWireVersion::Legacy);
        assert!(!legacy.wifi_direct_allowed());

        let mut local = new_caps();
        local.bearer_wire_max = bearer::V1_MYBR;
        let n = ProtocolCapabilities::negotiate(&local, &local);
        assert_eq!(n.bearer_wire_version(), BearerWireVersion::V1);
        assert!(n.wifi_direct_allowed());
    }

    #[test]
    fn matrix_all_version_combinations_ratchet() {
        for local in [
            ratchet::LEGACY_X25519,
            ratchet::V1_DOUBLE_RATCHET,
            ratchet::V2,
        ] {
            for remote in [
                ratchet::LEGACY_X25519,
                ratchet::V1_DOUBLE_RATCHET,
                ratchet::V2,
            ] {
                let mut l = new_caps();
                let mut r = new_caps();
                l.ratchet_max = local;
                r.ratchet_max = remote;
                let n = ProtocolCapabilities::negotiate(&l, &r);
                assert_eq!(n.ratchet, local.min(remote));
                if n.ratchet >= ratchet::V1_DOUBLE_RATCHET {
                    assert_eq!(n.preferred_e2e_version(), E2E_VERSION_RATCHET);
                } else {
                    assert_eq!(n.preferred_e2e_version(), E2E_VERSION_LEGACY);
                }
            }
        }
    }

    #[test]
    fn matrix_all_version_combinations_custody() {
        for local in [custody::NONE, custody::V1] {
            for remote in [custody::NONE, custody::V1] {
                let mut l = new_caps();
                let mut r = new_caps();
                l.custody_max = local;
                r.custody_max = remote;
                let n = ProtocolCapabilities::negotiate(&l, &r);
                assert_eq!(n.custody, local.min(remote));
                assert_eq!(
                    n.custody_v1_allowed(),
                    local >= custody::V1 && remote >= custody::V1
                );
            }
        }
    }

    #[test]
    fn matrix_all_version_combinations_payment_voucher() {
        for local in [payment_voucher::NONE, payment_voucher::V1] {
            for remote in [payment_voucher::NONE, payment_voucher::V1] {
                let mut l = new_caps();
                let mut r = new_caps();
                l.payment_voucher_max = local;
                r.payment_voucher_max = remote;
                let n = ProtocolCapabilities::negotiate(&l, &r);
                assert_eq!(n.payment_voucher, local.min(remote));
            }
        }
    }

    #[test]
    fn matrix_all_version_combinations_relay() {
        for local in [
            relay::LEGACY_FANOUT,
            relay::V1_TRUST_SCORED,
            relay::V2_DIVERSITY,
        ] {
            for remote in [
                relay::LEGACY_FANOUT,
                relay::V1_TRUST_SCORED,
                relay::V2_DIVERSITY,
            ] {
                let mut l = new_caps();
                let mut r = new_caps();
                l.relay_max = local;
                r.relay_max = remote;
                let n = ProtocolCapabilities::negotiate(&l, &r);
                assert_eq!(n.relay, local.min(remote));
            }
        }
    }

    #[test]
    fn matrix_old_new_client_quadrants() {
        let cases = [
            (
                old_caps(),
                old_caps(),
                ratchet::LEGACY_X25519,
                custody::NONE,
            ),
            (
                old_caps(),
                new_caps(),
                ratchet::LEGACY_X25519,
                custody::NONE,
            ),
            (
                new_caps(),
                old_caps(),
                ratchet::LEGACY_X25519,
                custody::NONE,
            ),
            (
                new_caps(),
                new_caps(),
                ratchet::V1_DOUBLE_RATCHET,
                custody::V1,
            ),
        ];
        for (local, remote, want_ratchet, want_custody) in cases {
            let n = ProtocolCapabilities::negotiate(&local, &remote);
            assert_eq!(n.ratchet, want_ratchet, "local={local:?} remote={remote:?}");
            assert_eq!(n.custody, want_custody, "local={local:?} remote={remote:?}");
        }
    }
}
