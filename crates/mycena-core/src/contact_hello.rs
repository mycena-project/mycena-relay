// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Signed direct message sent after scanning a peer's QR so the other device adds us as a contact.

use serde::{Deserialize, Serialize};

use crate::account_identity::{contact_hello_signable_bytes, verify_account_signature};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContactHello {
    pub peer_id: String,
    pub account_id: String,
    pub display_name: String,
    pub enc_pubkey_hex: String,
    pub signature: Vec<u8>,
}

impl ContactHello {
    pub fn sign(
        peer_id: &str,
        account_id: &str,
        display_name: &str,
        enc_pubkey_hex: &str,
        account_keypair: &libp2p::identity::Keypair,
    ) -> anyhow::Result<Self> {
        let bytes = contact_hello_signable_bytes(peer_id, account_id, display_name, enc_pubkey_hex);
        let signature = crate::account_identity::sign_account_message(account_keypair, &bytes)?;
        Ok(Self {
            peer_id: peer_id.to_string(),
            account_id: account_id.to_string(),
            display_name: display_name.to_string(),
            enc_pubkey_hex: enc_pubkey_hex.to_string(),
            signature,
        })
    }

    pub fn verify(&self) -> bool {
        if self.peer_id.is_empty() || self.account_id.is_empty() || self.enc_pubkey_hex.is_empty() {
            return false;
        }
        if self.enc_pubkey_hex.len() != 64
            || !self.enc_pubkey_hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            return false;
        }
        let bytes = contact_hello_signable_bytes(
            &self.peer_id,
            &self.account_id,
            &self.display_name,
            &self.enc_pubkey_hex,
        );
        verify_account_signature(&self.account_id, &bytes, &self.signature)
    }

    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use libp2p::identity::Keypair;

    #[test]
    fn contact_hello_roundtrip_and_verify() {
        let kp = Keypair::generate_ed25519();
        let account_id = crate::account_identity::account_id_from_keypair(&kp);
        let enc = "a".repeat(64);
        let hello = ContactHello::sign(
            "12D3KooWContactHelloPeerIdForTestOnly1",
            &account_id,
            "Alex",
            &enc,
            &kp,
        )
        .unwrap();
        assert!(hello.verify());
        let decoded = ContactHello::decode(&hello.encode().unwrap()).unwrap();
        assert_eq!(decoded, hello);
    }
}
