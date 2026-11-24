// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! NodeHandle ↔ BLE host bridge (same commands as Android FFI).
#![allow(dead_code)] // F2/F3 drain helpers; desktop relay uses `drain_bearer_outbound` today.

use mycena_app::node::AppNode;
use mycena_l1_facade::{NodeCommand, NodeHandle};
use std::sync::Arc;

pub async fn invite_uri(handle: &NodeHandle, local_peer_id: &str) -> Option<String> {
    let listen = handle.listen_addrs().await;
    let direct = handle.stable_direct_multiaddr(local_peer_id).await;
    let signed_invite = handle
        .encode_signed_invite_v3(local_peer_id, direct.as_deref())
        .await
        .ok();
    mycena_l0_facade::bootstrap::shareable_dial_multiaddrs(
        local_peer_id,
        &handle.local_enc_pubkey_hex(),
        &listen,
        signed_invite.as_deref(),
    )
    .into_iter()
    .find(|a| a.starts_with("mycena://"))
}

pub async fn register_ble_link(handle: &NodeHandle, peer_id: &str, device_id: &str) {
    let _ = handle
        .send(NodeCommand::RegisterBleLink {
            peer_id: peer_id.to_string(),
            device_id: device_id.to_string(),
        })
        .await;
}

pub async fn ingest_ble_frame(
    handle: &NodeHandle,
    from_peer: &str,
    frame: Vec<u8>,
) -> anyhow::Result<()> {
    handle
        .send(NodeCommand::IngestBleFrame {
            from_peer: from_peer.to_string(),
            frame,
        })
        .await?;
    Ok(())
}

pub async fn drain_ble_outbound(
    handle: &NodeHandle,
    to_peer: Option<String>,
    max: usize,
) -> Vec<(String, Vec<u8>)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .send(NodeCommand::DrainBleOutbound {
            to_peer,
            max,
            reply: Some(tx),
        })
        .await;
    rx.await.unwrap_or_default()
}

pub async fn drain_ble_stream_outbound(
    handle: &NodeHandle,
    to_peer: &str,
    max: usize,
) -> Vec<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .send(NodeCommand::DrainBleStreamOutbound {
            to_peer: to_peer.to_string(),
            max,
            reply: Some(tx),
        })
        .await;
    rx.await.unwrap_or_default()
}

pub async fn drain_bearer_outbound(
    handle: &NodeHandle,
    to_peer: Option<String>,
    max: usize,
) -> Vec<(
    String,
    Vec<u8>,
    mycena_l0_facade::bearer_transport::BearerMedium,
)> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = handle
        .send(NodeCommand::DrainBearerOutbound {
            to_peer,
            max,
            reply: Some(tx),
        })
        .await;
    rx.await.unwrap_or_default()
}

pub async fn ble_peer_addrs(handle: &NodeHandle) -> Vec<String> {
    handle
        .listen_addrs()
        .await
        .into_iter()
        .filter(|a| a.starts_with("/ble/"))
        .collect()
}

pub async fn apply_transport_invite(app_node: &Arc<AppNode>, invite: &str) -> anyhow::Result<()> {
    app_node
        .clone()
        .register_transport_from_invite(invite)
        .await
}

pub fn peer_id_from_invite(invite: &str) -> Option<String> {
    mycena_l0_facade::invite::parse_invite(invite.trim()).map(|target| match target {
        mycena_l0_facade::invite::InviteTarget::Peer(info) => info.peer_id,
        mycena_l0_facade::invite::InviteTarget::Multiaddr(_) => String::new(),
    })
}

pub fn account_key_from_invite(invite: &str) -> Option<String> {
    mycena_l0_facade::invite::parse_invite(invite.trim()).and_then(|target| match target {
        mycena_l0_facade::invite::InviteTarget::Peer(info) => {
            Some(info.account_id.unwrap_or_else(|| info.peer_id.clone()))
        }
        mycena_l0_facade::invite::InviteTarget::Multiaddr(_) => None,
    })
}
