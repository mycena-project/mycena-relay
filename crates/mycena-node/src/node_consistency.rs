// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! ADR 006 phase 2 — classify [`NodeCommand`] mesh writes and typed send helpers.

use mycena_core::consistency::{
    Consistency, EventuallyConsistentModel, LocalModel, StateOp, OP_DIRECT_MESSAGE_SEND,
    OP_GOSSIP_BULLETIN_PUBLISH, OP_INVITE_PARSE, OP_RESOURCE_MAP_BROADCAST, OP_SOS_BROADCAST,
};
use mycena_core::transport::WireMessage;

use crate::ingest_guard;
use crate::BroadcastPeerFilter;
use crate::NodeCommand;

impl NodeCommand {
    /// Consistency level for mesh-visible state changes (see ADR 006).
    pub fn mesh_consistency(&self) -> Option<Consistency> {
        match self {
            NodeCommand::SendDirect { .. }
            | NodeCommand::SendDirectPayload { .. }
            | NodeCommand::SendWire { .. }
            | NodeCommand::SendWireOrQueueBle { .. }
            | NodeCommand::SendEncryptedDirect { .. }
            | NodeCommand::EnqueueEncryptedDirectCustody { .. } => {
                Some(Consistency::EventuallyConsistent)
            }
            NodeCommand::Broadcast { scope, .. } | NodeCommand::BroadcastPayload { scope, .. } => {
                if ingest_guard::is_sos_scope(scope)
                    || scope.starts_with(mycena_core::resource_map::RESOURCE_MAP_SCOPE)
                {
                    Some(Consistency::Local)
                } else {
                    Some(Consistency::EventuallyConsistent)
                }
            }
            NodeCommand::SeedPeerEncKey { .. } | NodeCommand::SeedPeerRatchetBundle { .. } => {
                Some(Consistency::EventuallyConsistent)
            }
            _ => None,
        }
    }
}

impl crate::NodeHandle {
    /// Plaintext direct message ([`OP_DIRECT_MESSAGE_SEND`]).
    pub async fn mesh_send_direct_text(
        &self,
        to_peer: String,
        body: String,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_DIRECT_MESSAGE_SEND.name);
        self.send(NodeCommand::SendDirect { to_peer, body }).await
    }

    /// Direct mesh payload with optional priority ([`OP_DIRECT_MESSAGE_SEND`]).
    pub async fn mesh_send_direct(
        &self,
        to_peer: String,
        body: String,
        payload: Vec<u8>,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        self.mesh_send_direct_priority(to_peer, body, payload, None, _op)
            .await
    }

    pub async fn mesh_send_direct_priority(
        &self,
        to_peer: String,
        body: String,
        payload: Vec<u8>,
        priority: Option<mycena_core::data::Priority>,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_DIRECT_MESSAGE_SEND.name);
        self.send(NodeCommand::SendDirectPayload {
            to_peer,
            body,
            payload,
            priority,
            app_chat_id: None,
        })
        .await
    }

    /// Encrypted direct message queued for DTN custody when peer is offline ([`OP_DIRECT_MESSAGE_SEND`]).
    pub async fn enqueue_encrypted_direct_custody(
        &self,
        to_peer: String,
        inner_plaintext: Vec<u8>,
        priority: mycena_core::data::Priority,
        app_chat_id: Option<String>,
        display_body: String,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_DIRECT_MESSAGE_SEND.name);
        self.send(NodeCommand::EnqueueEncryptedDirectCustody {
            to_peer,
            inner_plaintext,
            priority,
            app_chat_id,
            display_body,
        })
        .await
    }

    /// Scoped gossip bulletin ([`OP_GOSSIP_BULLETIN_PUBLISH`]).
    pub async fn mesh_broadcast(
        &self,
        scope: String,
        body: String,
        payload: Vec<u8>,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_GOSSIP_BULLETIN_PUBLISH.name);
        self.send(NodeCommand::BroadcastPayload {
            scope,
            body,
            payload,
            peer_filter: BroadcastPeerFilter::All,
        })
        .await
    }

    /// Coin voucher gossip — only peers that negotiated payment voucher v1.
    pub async fn mesh_broadcast_coin_voucher(
        &self,
        scope: String,
        body: String,
        payload: Vec<u8>,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_GOSSIP_BULLETIN_PUBLISH.name);
        self.send(NodeCommand::BroadcastPayload {
            scope,
            body,
            payload,
            peer_filter: BroadcastPeerFilter::PaymentVoucherV1,
        })
        .await
    }

    /// SOS bulletin with local compose + propagation cap ([`OP_SOS_BROADCAST`]).
    pub async fn mesh_broadcast_sos(
        &self,
        scope: String,
        body: String,
        payload: Vec<u8>,
        _op: StateOp<LocalModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_SOS_BROADCAST.name);
        self.send(NodeCommand::BroadcastPayload {
            scope,
            body,
            payload,
            peer_filter: BroadcastPeerFilter::All,
        })
        .await
    }

    /// Resource-map pin with local compose + propagation cap ([`OP_RESOURCE_MAP_BROADCAST`]).
    pub async fn mesh_broadcast_resource_map(
        &self,
        scope: String,
        body: String,
        payload: Vec<u8>,
        _op: StateOp<LocalModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_RESOURCE_MAP_BROADCAST.name);
        self.send(NodeCommand::BroadcastPayload {
            scope,
            body,
            payload,
            peer_filter: BroadcastPeerFilter::All,
        })
        .await
    }

    /// Direct wire frame (libp2p or BLE queue) ([`OP_DIRECT_MESSAGE_SEND`]).
    pub async fn mesh_send_wire(
        &self,
        to_peer: String,
        message: WireMessage,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_DIRECT_MESSAGE_SEND.name);
        self.send(NodeCommand::SendWire { to_peer, message }).await
    }

    /// Wire over libp2p if connected, else BLE queue ([`OP_DIRECT_MESSAGE_SEND`]).
    pub async fn mesh_send_wire_or_ble(
        &self,
        to_peer: String,
        message: WireMessage,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_DIRECT_MESSAGE_SEND.name);
        self.send(NodeCommand::SendWireOrQueueBle { to_peer, message })
            .await
    }

    /// Invite / QR enc-key seed ([`OP_INVITE_PARSE`]).
    pub async fn mesh_seed_invite_enc_key(
        &self,
        peer_id: String,
        enc_pubkey_hex: String,
        _op: StateOp<EventuallyConsistentModel>,
    ) -> anyhow::Result<()> {
        debug_assert_eq!(_op.name, OP_INVITE_PARSE.name);
        self.send(NodeCommand::SeedPeerEncKey {
            peer_id,
            enc_pubkey_hex,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::consistency::OP_COIN_TRANSFER;

    #[test]
    fn command_consistency_matches_adr() {
        let dm = NodeCommand::SendDirect {
            to_peer: "p".into(),
            body: "hi".into(),
        };
        assert_eq!(
            dm.mesh_consistency(),
            Some(Consistency::EventuallyConsistent)
        );

        let sos = NodeCommand::BroadcastPayload {
            scope: "mycena/sos/v1".into(),
            body: "sos".into(),
            payload: vec![],
            peer_filter: BroadcastPeerFilter::All,
        };
        assert_eq!(sos.mesh_consistency(), Some(Consistency::Local));

        let resource = NodeCommand::BroadcastPayload {
            scope: "mycena/resource-map/v1".into(),
            body: "water".into(),
            payload: vec![],
            peer_filter: BroadcastPeerFilter::All,
        };
        assert_eq!(resource.mesh_consistency(), Some(Consistency::Local));

        let chat = NodeCommand::BroadcastPayload {
            scope: "mycena/chat".into(),
            body: "chat".into(),
            payload: vec![],
            peer_filter: BroadcastPeerFilter::All,
        };
        assert_eq!(
            chat.mesh_consistency(),
            Some(Consistency::EventuallyConsistent)
        );

        let seed = NodeCommand::SeedPeerEncKey {
            peer_id: "p".into(),
            enc_pubkey_hex: "aa".into(),
        };
        assert_eq!(
            seed.mesh_consistency(),
            Some(Consistency::EventuallyConsistent)
        );

        assert_eq!(
            NodeCommand::SetEnergyState(mycena_core::energy::NodeState::Active).mesh_consistency(),
            None
        );
    }

    #[test]
    fn mesh_ops_reference_adr_constants() {
        assert_eq!(
            OP_DIRECT_MESSAGE_SEND.consistency(),
            Consistency::EventuallyConsistent
        );
        assert_eq!(
            OP_GOSSIP_BULLETIN_PUBLISH.consistency(),
            Consistency::EventuallyConsistent
        );
        assert_eq!(OP_SOS_BROADCAST.consistency(), Consistency::Local);
        assert_eq!(OP_RESOURCE_MAP_BROADCAST.consistency(), Consistency::Local);
        assert_eq!(
            OP_COIN_TRANSFER.consistency(),
            Consistency::SettlementRequired
        );
    }
}
