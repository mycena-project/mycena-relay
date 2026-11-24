// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! At-rest vault access and encryption key loading.

use crate::crypto::EncKey;
use mycena_core::at_rest::SecretVault;
use std::path::Path;

const ENC_SECRET: &str = "enc_x25519";

/// Optional 32-byte master key (Android); when `None`, OS keyring or fallback is used.
pub type StorageKey = Option<[u8; 32]>;

pub use mycena_core::at_rest::parse_storage_key_hex;

pub fn open_vault(db_path: &str, storage_key: StorageKey) -> anyhow::Result<SecretVault> {
    SecretVault::open_with_migration(db_path, storage_key)
}

pub fn load_or_create_enc_keypair(
    db_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<EncKey> {
    let vault = open_vault(db_path, storage_key)?;

    match vault.read_secret(ENC_SECRET) {
        Ok(Some(bytes)) => return enc_from_secret_bytes(&bytes),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                "unreadable encrypted enc key at {:?}: {e}; removing corrupt secret",
                vault.secrets_dir().join(format!("{ENC_SECRET}.enc"))
            );
            let _ = vault.delete_secret(ENC_SECRET);
        }
    }

    let legacy_path = Path::new(db_path).join("enc_x25519.secret");
    if legacy_path.exists() {
        let bytes = std::fs::read(&legacy_path)?;
        vault.write_secret(ENC_SECRET, &bytes)?;
        std::fs::remove_file(&legacy_path)?;
        tracing::info!("migrated X25519 enc key to encrypted at-rest storage");
        return enc_from_secret_bytes(&bytes);
    }

    let kp = EncKey::generate();
    let secret = kp.secret_bytes();
    vault.write_secret(ENC_SECRET, &secret)?;
    tracing::info!("generated new X25519 encryption keypair (encrypted at rest)");
    Ok(kp)
}

fn enc_from_secret_bytes(bytes: &[u8]) -> anyhow::Result<EncKey> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid enc key length"))?;
    Ok(EncKey::from_secret_bytes(arr))
}
