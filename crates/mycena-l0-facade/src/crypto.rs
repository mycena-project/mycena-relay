// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Opaque encryption key wrapper and free crypto helpers.

use mycena_core::crypto::{
    self as core_crypto, decrypt_group as core_decrypt_group, decrypt_with, EncryptionKeypair,
};

pub use core_crypto::{
    decrypt_group, encrypt_for, encrypt_group, parse_x25519_public_hex, public_key_from_bytes,
};

/// Opaque X25519 encryption keypair for app-layer E2E (no raw secret export).
pub struct EncKey(EncryptionKeypair);

impl EncKey {
    pub fn generate() -> Self {
        Self(EncryptionKeypair::generate())
    }

    pub fn public_hex(&self) -> String {
        self.0.public_hex()
    }

    pub fn decrypt_direct(&self, ciphertext_blob: &[u8]) -> anyhow::Result<Vec<u8>> {
        decrypt_with(ciphertext_blob, &self.0)
    }

    pub fn decrypt_group(&self, blob: &[u8], group_key: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
        core_decrypt_group(blob, group_key)
    }

    pub(crate) fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        Self(EncryptionKeypair::from_secret_bytes(bytes))
    }

    pub(crate) fn secret_bytes(&self) -> [u8; 32] {
        self.0.secret_bytes()
    }
}

impl Clone for EncKey {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
