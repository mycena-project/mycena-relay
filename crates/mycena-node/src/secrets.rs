// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Load identity and encryption keys through [`mycena_core::at_rest::SecretVault`].

use libp2p::identity;
use mycena_core::lock_or_recover;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

const IDENTITY_SECRET: &str = "ed25519_identity";

fn account_load_lock(db_path: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    // POISON-RECOVER: lock map only guards insert; no partial HashMap mutation on panic.
    let mut guard = mycena_core::lock_recover::lock_or_recover(locks, "account load lock map");
    guard
        .entry(db_path.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

use mycena_core::account_identity::{self, ACCOUNT_SECRET};
use mycena_core::identity_rotation::IdentityRotationGossip;

use crate::transport_meta::{TransportMeta, TransportMetaStore};

/// Result of loading account + transport keys, optionally rotating transport PeerId.
pub struct IdentityBootstrap {
    pub account_keypair: identity::Keypair,
    pub account_id: String,
    pub transport_meta: TransportMeta,
    pub pending_rotation: Option<IdentityRotationGossip>,
}

impl IdentityBootstrap {
    /// Test/sim helper when transport meta store is not used.
    pub fn ephemeral() -> Self {
        let account_keypair = identity::Keypair::generate_ed25519();
        let account_id = account_identity::account_id_from_keypair(&account_keypair);
        Self {
            account_keypair,
            account_id,
            transport_meta: TransportMeta {
                epoch: 1,
                rotated_at_ms: mycena_core::data::now_ms(),
                previous_peer_ids: Vec::new(),
            },
            pending_rotation: None,
        }
    }
}

pub fn bootstrap_identities(
    db_path: &str,
    keypair_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<IdentityBootstrap> {
    let account_keypair = load_or_create_account_keypair(db_path, storage_key)?;
    let account_id = account_identity::account_id_from_keypair(&account_keypair);
    let meta_store = TransportMetaStore::open(db_path)?;
    let mut meta = meta_store.load()?;
    meta_store.mark_initial(&mut meta)?;

    let mut pending_rotation = None;
    if meta_store.should_rotate(&meta) {
        let old_kp = load_or_create_keypair(keypair_path, storage_key)?;
        let old_peer_id = old_kp.public().to_peer_id().to_string();
        regenerate_transport_keypair(keypair_path, storage_key)?;
        meta.epoch = meta.epoch.saturating_add(1);
        meta.rotated_at_ms = mycena_core::data::now_ms();
        if !meta.previous_peer_ids.contains(&old_peer_id) {
            meta.previous_peer_ids.push(old_peer_id.clone());
        }
        meta_store.save(&meta)?;
        let new_kp = load_or_create_keypair(keypair_path, storage_key)?;
        let new_peer_id = new_kp.public().to_peer_id().to_string();
        pending_rotation = Some(IdentityRotationGossip::sign(
            &account_id,
            &old_peer_id,
            &new_peer_id,
            meta.epoch,
            &account_keypair,
        )?);
        tracing::info!(
            "transport identity rotated epoch={} peer_id={new_peer_id}",
            meta.epoch
        );
    }

    Ok(IdentityBootstrap {
        account_keypair,
        account_id,
        transport_meta: meta,
        pending_rotation,
    })
}

pub fn load_or_create_account_keypair(
    db_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<identity::Keypair> {
    let lock = account_load_lock(db_path);
    // POISON-RECOVER: serializes identity load; vault IO is idempotent on retry.
    let _guard = lock_or_recover!(lock, "account load mutex");
    let vault = open_vault(db_path, storage_key)?;
    match vault.read_secret(ACCOUNT_SECRET) {
        Ok(Some(bytes)) => return keypair_from_ed25519_secret(&bytes),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("unreadable account identity: {e}; regenerating");
            let _ = vault.delete_secret(ACCOUNT_SECRET);
        }
    }
    let keypair = identity::Keypair::generate_ed25519();
    let secret = ed25519_secret_bytes(&keypair)?;
    vault.write_secret(ACCOUNT_SECRET, &secret)?;
    tracing::info!("generated stable account identity (SD-190 spike)");
    Ok(keypair)
}

pub fn regenerate_transport_keypair(
    path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<identity::Keypair> {
    let db_path = Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(path);
    let vault = open_vault(db_path, storage_key)?;
    vault.delete_secret(IDENTITY_SECRET)?;
    load_or_create_keypair(path, storage_key)
}

/// Build a signed v3 invite QR string for sharing (reloads account key from vault).
/// Prefer [`NodeHandle::encode_signed_invite_v3`] while the node is running.
pub fn encode_signed_invite_v3(
    db_path: &str,
    storage_key: StorageKey,
    peer_id: &str,
    enc_pubkey_hex: &str,
    direct_multiaddr: Option<&str>,
) -> anyhow::Result<String> {
    let account_keypair = load_or_create_account_keypair(db_path, storage_key)?;
    let account_id = account_identity::account_id_from_keypair(&account_keypair);
    let meta = TransportMetaStore::open(db_path)?.load()?;
    let epoch = meta.epoch.max(1);
    mycena_core::invite::encode_signed_invite_v3(
        &account_id,
        peer_id,
        enc_pubkey_hex,
        epoch,
        &account_keypair,
        direct_multiaddr,
        None,
    )
}

/// Optional 32-byte master key (Android); when `None`, OS keyring or fallback is used.
pub use mycena_l0_facade::secrets::{load_or_create_enc_keypair, open_vault, StorageKey};

const ENC_SECRET: &str = "enc_x25519";

/// Load X25519 encryption key for node ratchet/session (uses core type — not exposed via [`EncKey`]).
pub fn load_or_create_encryption_keypair(
    db_path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<mycena_core::crypto::EncryptionKeypair> {
    use mycena_core::crypto::EncryptionKeypair;
    use std::path::Path;

    let vault = open_vault(db_path, storage_key)?;

    match vault.read_secret(ENC_SECRET) {
        Ok(Some(bytes)) => return enc_keypair_from_secret_bytes(&bytes),
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
        return enc_keypair_from_secret_bytes(&bytes);
    }

    let kp = EncryptionKeypair::generate();
    vault.write_secret(ENC_SECRET, &kp.secret_bytes())?;
    tracing::info!("generated new X25519 encryption keypair (encrypted at rest)");
    Ok(kp)
}

fn enc_keypair_from_secret_bytes(
    bytes: &[u8],
) -> anyhow::Result<mycena_core::crypto::EncryptionKeypair> {
    use mycena_core::crypto::EncryptionKeypair;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid enc key length"))?;
    Ok(EncryptionKeypair::from_secret_bytes(arr))
}

pub fn load_or_create_keypair(
    path: &str,
    storage_key: StorageKey,
) -> anyhow::Result<identity::Keypair> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db_path = Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(path);
    let vault = open_vault(db_path, storage_key)?;

    match vault.read_secret(IDENTITY_SECRET) {
        Ok(Some(bytes)) => return keypair_from_ed25519_secret(&bytes),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                "unreadable encrypted identity at {:?}: {e}; removing corrupt secret",
                vault.secrets_dir().join(format!("{IDENTITY_SECRET}.enc"))
            );
            let _ = vault.delete_secret(IDENTITY_SECRET);
        }
    }

    if let Some(legacy) = read_legacy_sled_identity(path)? {
        vault.write_secret(IDENTITY_SECRET, &legacy)?;
        remove_legacy_sled_secret(path)?;
        tracing::info!("migrated Ed25519 identity to encrypted at-rest storage");
        return keypair_from_ed25519_secret(&legacy);
    }

    let keypair = identity::Keypair::generate_ed25519();
    let secret = ed25519_secret_bytes(&keypair)?;
    vault.write_secret(IDENTITY_SECRET, &secret)?;
    tracing::info!("generated new Ed25519 identity (encrypted at rest)");
    Ok(keypair)
}

fn keypair_from_ed25519_secret(secret: &[u8]) -> anyhow::Result<identity::Keypair> {
    let mut secret_bytes = secret.to_vec();
    let sk = identity::ed25519::SecretKey::try_from_bytes(&mut secret_bytes)
        .map_err(|e| anyhow::anyhow!("invalid ed25519 secret: {e}"))?;
    Ok(identity::Keypair::from(identity::ed25519::Keypair::from(
        sk,
    )))
}

fn ed25519_secret_bytes(keypair: &identity::Keypair) -> anyhow::Result<Vec<u8>> {
    let ed = keypair
        .clone()
        .try_into_ed25519()
        .map_err(|_| anyhow::anyhow!("keypair is not ed25519"))?;
    Ok(ed.secret().as_ref().to_vec())
}

fn read_legacy_sled_identity(path: &str) -> anyhow::Result<Option<Vec<u8>>> {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn account_load_mutex_survives_poison() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let lock = account_load_lock(path);
            let _guard = lock_or_recover!(lock, "account load mutex");
            panic!("deliberate account load mutex poison");
        }));
        let kp = load_or_create_account_keypair(path, None).expect("load after poison");
        assert!(kp.public().to_peer_id().to_string().starts_with("12D3"));
    }
}
