// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Epidemic mini-app bundle propagation over gossip (village / cluster seeding).
//!
//! Listings travel on `mycena/appstore/v1`; bundle bytes use chunked gossip on
//! `mycena/appstore/bundles/v1`. Every chunk is verified against the signed listing
//! hash before install — see [`docs/OFFLINE_DISTRIBUTION.md`](../../docs/OFFLINE_DISTRIBUTION.md).

use crate::envelope::{AppId, AppMessage, AppPayload};
use crate::miniapp::install_policy::{bundle_hash_hex, InstallTrust};
use crate::miniapp::store::{AppStore, AppStoreListing};
use mycena_l0_facade::consistency::OP_GOSSIP_BULLETIN_PUBLISH;
use mycena_l1_facade::NodeHandle;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

pub const BUNDLE_GOSSIP_SCOPE: &str = "mycena/appstore/bundles/v1";
pub const CHUNK_SIZE: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MiniAppBundleChunk {
    pub app_id: String,
    pub bundle_hash: String,
    pub chunk_index: u32,
    pub total_chunks: u32,
    pub data: Vec<u8>,
    pub from_peer: String,
}

#[derive(Default)]
pub struct BundleReassembly {
    partial: HashMap<(String, String), PartialBundle>,
}

struct PartialBundle {
    total_chunks: u32,
    chunks: HashMap<u32, Vec<u8>>,
}

impl BundleReassembly {
    pub fn push_chunk(&mut self, chunk: MiniAppBundleChunk) -> Option<Vec<u8>> {
        if chunk.data.is_empty() || chunk.total_chunks == 0 {
            return None;
        }
        let key = (chunk.app_id.clone(), chunk.bundle_hash.clone());
        let entry = self.partial.entry(key).or_insert_with(|| PartialBundle {
            total_chunks: chunk.total_chunks,
            chunks: HashMap::new(),
        });
        if entry.total_chunks != chunk.total_chunks {
            return None;
        }
        entry.chunks.insert(chunk.chunk_index, chunk.data);
        if entry.chunks.len() as u32 != entry.total_chunks {
            return None;
        }
        let mut out = Vec::new();
        for idx in 0..entry.total_chunks {
            out.extend_from_slice(entry.chunks.get(&idx)?);
        }
        self.partial.remove(&(chunk.app_id, chunk.bundle_hash));
        Some(out)
    }
}

pub fn split_bundle(
    bundle_bytes: &[u8],
    app_id: &str,
    bundle_hash: &str,
    from_peer: &str,
) -> Vec<MiniAppBundleChunk> {
    let total_chunks = bundle_bytes.len().div_ceil(CHUNK_SIZE).max(1) as u32;
    bundle_bytes
        .chunks(CHUNK_SIZE)
        .enumerate()
        .map(|(i, data)| MiniAppBundleChunk {
            app_id: app_id.to_string(),
            bundle_hash: bundle_hash.to_string(),
            chunk_index: i as u32,
            total_chunks,
            data: data.to_vec(),
            from_peer: from_peer.to_string(),
        })
        .collect()
}

pub fn verify_bundle_hash(bundle_bytes: &[u8], expected_hash: &str) -> bool {
    bundle_hash_hex(bundle_bytes) == expected_hash
}

/// Publish signed listings + chunked bundle gossip for every installed app we hold archives for.
pub async fn propagate_installed_miniapps(
    handle: &NodeHandle,
    store: &AppStore,
    local_peer_id: &str,
) -> anyhow::Result<usize> {
    let mut sent = 0usize;
    for manifest in store.installed_apps()? {
        let app_id = &manifest.id;
        let Some(bytes) = store.bundle_archive_bytes(app_id)? else {
            continue;
        };
        let hash = bundle_hash_hex(&bytes);
        let listings = store.browse_listings()?;
        let listing = listings.into_iter().find(|l| l.manifest.id == *app_id);
        if let Some(listing) = listing {
            if listing.verify_signature()? {
                publish_listing(handle, &listing).await?;
            }
        }
        for chunk in split_bundle(&bytes, app_id, &hash, local_peer_id) {
            publish_chunk(handle, &chunk).await?;
            sent += 1;
        }
        info!("epidemic: gossiped {} chunks for miniapp {app_id}", sent);
    }
    Ok(sent)
}

pub async fn publish_chunk(handle: &NodeHandle, chunk: &MiniAppBundleChunk) -> anyhow::Result<()> {
    let payload = AppMessage {
        app_id: AppId::MiniAppBundleChunk,
        payload: AppPayload::MiniAppBundleChunk(Box::new(chunk.clone())),
    }
    .encode()?;
    handle
        .mesh_broadcast(
            BUNDLE_GOSSIP_SCOPE.into(),
            format!("[bundle:{}:{}]", chunk.app_id, chunk.chunk_index),
            payload,
            OP_GOSSIP_BULLETIN_PUBLISH,
        )
        .await?;
    Ok(())
}

async fn publish_listing(handle: &NodeHandle, listing: &AppStoreListing) -> anyhow::Result<()> {
    let payload = AppMessage {
        app_id: AppId::AppStore,
        payload: AppPayload::AppStoreListing(Box::new(listing.clone())),
    }
    .encode()?;
    handle
        .mesh_broadcast(
            "mycena/appstore/v1".into(),
            format!("[app:{}]", listing.manifest.id),
            payload,
            OP_GOSSIP_BULLETIN_PUBLISH,
        )
        .await?;
    Ok(())
}

/// Ingest one gossip chunk; returns complete bundle bytes when reassembly finishes.
pub fn ingest_chunk(
    reassembly: &mut BundleReassembly,
    chunk: MiniAppBundleChunk,
) -> Option<Vec<u8>> {
    reassembly.push_chunk(chunk)
}

/// Verify listing + bundle signature policy, then install.
pub fn try_install_epidemic_bundle(
    store: &AppStore,
    bundle_bytes: &[u8],
    expected_hash: &str,
) -> anyhow::Result<bool> {
    if !verify_bundle_hash(bundle_bytes, expected_hash) {
        warn!("epidemic: bundle hash mismatch");
        return Ok(false);
    }
    let listings = store.browse_listings()?;
    let hash = bundle_hash_hex(bundle_bytes);
    let listing = listings
        .into_iter()
        .find(|l| l.bundle_hash == hash && l.verify_signature().unwrap_or(false));
    if let Some(listing) = listing {
        store.install_verified(bundle_bytes, InstallTrust::VerifiedListing, false)?;
        info!(
            "epidemic: installed {} v{} from gossip",
            listing.manifest.id, listing.manifest.version
        );
        return Ok(true);
    }
    // Unsigned epidemic payloads are rejected — sideload ack required for manual install.
    warn!("epidemic: no verified listing for hash {hash}; not installing");
    Ok(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reassembly_roundtrip() {
        let data: Vec<u8> = (0..CHUNK_SIZE + 100).map(|i| (i % 251) as u8).collect();
        let hash = bundle_hash_hex(&data);
        let chunks = split_bundle(&data, "app.test", &hash, "12D3KooWLocal");
        assert_eq!(chunks.len(), 2);
        let mut r = BundleReassembly::default();
        let mut complete = None;
        for c in chunks {
            if let Some(v) = r.push_chunk(c) {
                complete = Some(v);
            }
        }
        assert_eq!(complete.as_deref(), Some(data.as_slice()));
    }
}
