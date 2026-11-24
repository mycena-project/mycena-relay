// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Signed transport PeerId rotation bound to stable account identity (SD-190).

use serde::{Deserialize, Serialize};

use crate::account_identity::{identity_rotation_signable_bytes, verify_account_signature};

/// Prefix for E2E direct ciphertext inner plaintext (not visible on gossip).
pub const E2E_ANNOUNCE_PREFIX: &[u8] = b"MYCENA/IDROT/E2E\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityRotationGossip {
    pub account_id: String,
    /// Transport PeerId before this rotation (helps ratchet/session remap on receive).
    #[serde(default)]
    pub previous_peer_id: String,
    pub new_peer_id: String,
    pub epoch: u64,
    pub signature: Vec<u8>,
}

impl IdentityRotationGossip {
    pub fn sign(
        account_id: &str,
        previous_peer_id: &str,
        new_peer_id: &str,
        epoch: u64,
        account_keypair: &libp2p::identity::Keypair,
    ) -> anyhow::Result<Self> {
        let bytes =
            identity_rotation_signable_bytes(account_id, previous_peer_id, new_peer_id, epoch);
        let signature = crate::account_identity::sign_account_message(account_keypair, &bytes)?;
        Ok(Self {
            account_id: account_id.to_string(),
            previous_peer_id: previous_peer_id.to_string(),
            new_peer_id: new_peer_id.to_string(),
            epoch,
            signature,
        })
    }

    pub fn verify(&self) -> bool {
        if self.account_id.is_empty() || self.new_peer_id.is_empty() {
            return false;
        }
        let bytes = identity_rotation_signable_bytes(
            &self.account_id,
            &self.previous_peer_id,
            &self.new_peer_id,
            self.epoch,
        );
        verify_account_signature(&self.account_id, &bytes, &self.signature)
    }

    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Inner plaintext for Double-Ratchet / legacy E2E direct (opaque to mesh observers).
    pub fn encode_e2e_announce(&self) -> anyhow::Result<Vec<u8>> {
        let body = self.encode()?;
        let mut out = Vec::with_capacity(E2E_ANNOUNCE_PREFIX.len() + body.len());
        out.extend_from_slice(E2E_ANNOUNCE_PREFIX);
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn decode_e2e_announce(bytes: &[u8]) -> Option<Self> {
        if bytes.len() <= E2E_ANNOUNCE_PREFIX.len() || !bytes.starts_with(E2E_ANNOUNCE_PREFIX) {
            return None;
        }
        Self::decode(&bytes[E2E_ANNOUNCE_PREFIX.len()..]).ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use libp2p::identity::Keypair;

    #[test]
    fn rotation_gossip_roundtrip() {
        let kp = Keypair::generate_ed25519();
        let account_id = crate::account_identity::account_id_from_keypair(&kp);
        let gossip = IdentityRotationGossip::sign(
            &account_id,
            "12D3KooWOldTransportPeerIdForRotationTestOnly",
            "12D3KooWNewTransportPeerIdForRotationTestOnly",
            2,
            &kp,
        )
        .unwrap();
        assert!(gossip.verify());
        let decoded = IdentityRotationGossip::decode(&gossip.encode().unwrap()).unwrap();
        assert_eq!(decoded, gossip);
    }

    #[test]
    fn e2e_announce_roundtrip() {
        let kp = Keypair::generate_ed25519();
        let account_id = crate::account_identity::account_id_from_keypair(&kp);
        let gossip =
            IdentityRotationGossip::sign(&account_id, "12D3KooWOld", "12D3KooWNew", 1, &kp)
                .unwrap();
        let wrapped = gossip.encode_e2e_announce().unwrap();
        let decoded = IdentityRotationGossip::decode_e2e_announce(&wrapped).unwrap();
        assert_eq!(decoded, gossip);
    }
}
