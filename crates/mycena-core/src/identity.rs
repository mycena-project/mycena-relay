// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Ed25519 libp2p identity via [`crate::at_rest::SecretVault`].
//!
//! Decrypt failure of an existing identity file is a hard error — never mint a
//! new Peer ID over an unreadable vault (public-relay bootstrap stability).

use crate::at_rest::{parse_storage_key_hex, SecretVault};
use libp2p::identity;
use std::path::Path;

const IDENTITY_SECRET: &str = "ed25519_identity";

/// Optional 32-byte master key (Android Keystore-backed prefs).
pub type StorageKey = Option<[u8; 32]>;

fn vault_db_path(identity_path: &str) -> &str {
    Path::new(identity_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(identity_path)
}

fn open_vault(identity_path: &str, storage_key: StorageKey) -> anyhow::Result<SecretVault> {
    if let Some(parent) = Path::new(identity_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    SecretVault::open_with_migration(vault_db_path(identity_path), storage_key)
}

/// Read the encrypted identity. `Ok(None)` if it has never been created.
/// An existing blob that does not decrypt is `Err` (fail closed).
pub fn load_keypair(
    identity_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<Option<identity::Keypair>> {
    let vault = open_vault(identity_path, storage_key)?;
    match vault.read_secret(IDENTITY_SECRET) {
        Ok(Some(bytes)) => Ok(Some(keypair_from_ed25519_secret(&bytes)?)),
        Ok(None) => Ok(None),
        Err(e) => Err(anyhow::anyhow!(
            "unreadable encrypted identity at {:?}: {e}; refusing to mint a new Peer ID",
            vault.secrets_dir().join(format!("{IDENTITY_SECRET}.enc"))
        )),
    }
}

pub fn persist_keypair(
    identity_path: &str,
    storage_key: StorageKey,
    keypair: &identity::Keypair,
) -> anyhow::Result<()> {
    let vault = open_vault(identity_path, storage_key)?;
    vault.write_secret(IDENTITY_SECRET, &ed25519_secret_bytes(keypair)?)?;
    Ok(())
}

/// Load the identity or generate one when no vault entry exists.
pub fn load_or_create_keypair(
    identity_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<identity::Keypair> {
    if let Some(kp) = load_keypair(identity_path, storage_key)? {
        return Ok(kp);
    }
    let keypair = identity::Keypair::generate_ed25519();
    persist_keypair(identity_path, storage_key, &keypair)?;
    tracing::info!("generated new Ed25519 identity (encrypted at rest)");
    Ok(keypair)
}

/// Like [`load_or_create_keypair`], but reads `MYCENA_STORAGE_KEY` (64 hex chars) when set.
pub fn load_or_create_keypair_from_env(identity_path: &str) -> anyhow::Result<identity::Keypair> {
    let storage_key = match std::env::var("MYCENA_STORAGE_KEY") {
        Ok(hex) => Some(parse_storage_key_hex(&hex)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => return Err(e.into()),
    };
    load_or_create_keypair(identity_path, storage_key)
}

pub fn keypair_from_ed25519_secret(secret: &[u8]) -> anyhow::Result<identity::Keypair> {
    let mut secret_bytes = secret.to_vec();
    let sk = identity::ed25519::SecretKey::try_from_bytes(&mut secret_bytes)
        .map_err(|e| anyhow::anyhow!("invalid ed25519 secret: {e}"))?;
    Ok(identity::Keypair::from(identity::ed25519::Keypair::from(
        sk,
    )))
}

pub fn ed25519_secret_bytes(keypair: &identity::Keypair) -> anyhow::Result<Vec<u8>> {
    let ed = keypair
        .clone()
        .try_into_ed25519()
        .map_err(|_| anyhow::anyhow!("keypair is not ed25519"))?;
    Ok(ed.secret().as_ref().to_vec())
}
