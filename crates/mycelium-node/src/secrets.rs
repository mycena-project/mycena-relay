// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycelium Project
//! Load identity and encryption keys through [`mycelium_core::at_rest::SecretVault`].

use libp2p::identity;
use mycelium_core::at_rest::SecretVault;
use mycelium_core::crypto::EncryptionKeypair;
use mycelium_core::identity::{self as core_identity};
use std::path::Path;

pub use mycelium_core::identity::StorageKey;

const ENC_SECRET: &str = "enc_x25519";

pub fn open_vault(db_path: &str, storage_key: StorageKey) -> anyhow::Result<SecretVault> {
    SecretVault::open_with_migration(db_path, storage_key)
}

pub fn load_or_create_keypair(
    path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<identity::Keypair> {
    if let Some(kp) = core_identity::load_keypair(path, storage_key)? {
        return Ok(kp);
    }

    if let Some(legacy) = read_legacy_sled_identity(path)? {
        core_identity::persist_keypair(
            path,
            storage_key,
            &core_identity::keypair_from_ed25519_secret(&legacy)?,
        )?;
        remove_legacy_sled_secret(path)?;
        tracing::info!("migrated Ed25519 identity to encrypted at-rest storage");
        return core_identity::keypair_from_ed25519_secret(&legacy);
    }

    core_identity::load_or_create_keypair(path, storage_key)
}

pub fn load_or_create_enc_keypair(
    db_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<EncryptionKeypair> {
    let vault = open_vault(db_path, storage_key)?;

    match vault.read_secret(ENC_SECRET) {
        Ok(Some(bytes)) => return enc_from_secret_bytes(&bytes),
        Ok(None) => {}
        Err(e) => {
            anyhow::bail!(
                "unreadable encrypted enc key at {:?}: {e}; refusing to mint a new key",
                vault.secrets_dir().join(format!("{ENC_SECRET}.enc"))
            );
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

    let kp = EncryptionKeypair::generate();
    vault.write_secret(ENC_SECRET, &kp.secret_bytes())?;
    tracing::info!("generated new X25519 encryption keypair (encrypted at rest)");
    Ok(kp)
}

fn enc_from_secret_bytes(bytes: &[u8]) -> anyhow::Result<EncryptionKeypair> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid enc key length"))?;
    Ok(EncryptionKeypair::from_secret_bytes(arr))
}

fn read_legacy_sled_identity(path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    if !Path::new(path).exists() {
        return Ok(None);
    }
    let db = sled::open(path)?;
    let tree = db.open_tree("identity")?;
    Ok(tree.get(b"ed25519_secret_key")?.map(|v| v.to_vec()))
}

fn remove_legacy_sled_secret(path: &str) -> anyhow::Result<()> {
    let db = sled::open(path)?;
    let tree = db.open_tree("identity")?;
    tree.remove(b"ed25519_secret_key")?;
    tree.flush()?;
    Ok(())
}
