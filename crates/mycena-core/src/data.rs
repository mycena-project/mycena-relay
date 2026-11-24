// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct ContentId(pub String);

impl ContentId {
    pub fn from_bytes(data: &[u8]) -> Self {
        Self(blake3::hash(data).to_hex().to_string())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
pub enum Priority {
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: ContentId,
    pub from_peer: String,
    pub to_peer: Option<String>,
    pub payload: Vec<u8>,
    pub created_at_ms: u64,
    pub ttl_secs: u32,
    pub priority: Priority,
    #[serde(default)]
    pub signature: Option<Vec<u8>>,
    /// 0 = legacy (no ID in signed bytes); 1 = v1 (ID included, replay-safe).
    #[serde(default)]
    pub sig_version: u8,
    #[serde(default)]
    pub hop_count: u8,
    #[serde(default = "default_max_hops")]
    pub max_hops: u8,
    /// Original author when `from_peer` was replaced by a relay hop identity.
    #[serde(default)]
    pub author_peer: Option<String>,
    /// E2E crypto protocol: 0 = static X25519 (legacy), 1 = Double Ratchet.
    #[serde(default)]
    pub e2e_version: u8,
    /// DTN store-and-forward — use custody queue instead of live epidemic fanout.
    #[serde(default)]
    pub custody: bool,
    /// Spray-and-wait copies already dispatched network-wide.
    #[serde(default)]
    pub custody_spray_sent: u8,
    /// Max active custody copies allowed in the mesh for this message.
    #[serde(default = "default_max_custody_spray")]
    pub max_custody_spray: u8,
    /// Previous custody holder — avoids immediate ping-pong on handoff.
    #[serde(default)]
    pub last_custody_from: Option<String>,
}

impl Envelope {
    pub fn new(from_peer: String, to_peer: Option<String>, payload: Vec<u8>) -> Self {
        let now = now_ms();
        let mut digest_input = from_peer.as_bytes().to_vec();
        digest_input.extend_from_slice(&payload);
        digest_input.extend_from_slice(&now.to_le_bytes());
        Self {
            id: ContentId::from_bytes(&digest_input),
            from_peer,
            to_peer,
            payload,
            created_at_ms: now,
            ttl_secs: 7 * 24 * 60 * 60,
            priority: Priority::Normal,
            signature: None,
            sig_version: 1,
            hop_count: 0,
            max_hops: default_max_hops(),
            author_peer: None,
            e2e_version: 0,
            custody: false,
            custody_spray_sent: 0,
            max_custody_spray: default_max_custody_spray(),
            last_custody_from: None,
        }
    }

    /// Directed DTN message with spray-and-wait custody (disaster / city carry).
    pub fn new_custody(from_peer: String, to_peer: String, payload: Vec<u8>) -> Self {
        let mut e = Self::new(from_peer, Some(to_peer), payload);
        e.custody = true;
        e.max_hops = default_custody_max_hops();
        e.ttl_secs = 48 * 60 * 60;
        e
    }

    /// Like [`Self::new`] but records which E2E wire protocol decrypted the payload.
    pub fn new_with_e2e_version(
        from_peer: String,
        to_peer: Option<String>,
        payload: Vec<u8>,
        e2e_version: u8,
    ) -> Self {
        let mut e = Self::new(from_peer, to_peer, payload);
        e.e2e_version = e2e_version;
        e
    }

    /// Envelope without a mesh signature (gossip relay; author auth lives in payload).
    ///
    /// `ContentId` is `blake3(scope || 0xFF || from_peer || payload)` — no wall clock —
    /// so redelivery after restart dedups. `created_at_ms` prefers a JSON `created_at_ms`
    /// field when present so ingest freshness is not vacuous.
    pub fn new_unsigned(from_peer: String, to_peer: Option<String>, payload: Vec<u8>) -> Self {
        Self::new_unsigned_gossip(from_peer, to_peer, payload, "")
    }

    /// Like [`Self::new_unsigned`] with a gossip scope mixed into the content id.
    pub fn new_unsigned_gossip(
        from_peer: String,
        to_peer: Option<String>,
        payload: Vec<u8>,
        scope: &str,
    ) -> Self {
        let created_at_ms = created_at_from_payload(&payload).unwrap_or_else(now_ms);
        let mut digest_input =
            Vec::with_capacity(scope.len() + from_peer.len() + payload.len() + 1);
        digest_input.extend_from_slice(scope.as_bytes());
        digest_input.push(0xFF);
        digest_input.extend_from_slice(from_peer.as_bytes());
        digest_input.extend_from_slice(&payload);
        Self {
            id: ContentId::from_bytes(&digest_input),
            from_peer,
            to_peer,
            payload,
            created_at_ms,
            ttl_secs: 7 * 24 * 60 * 60,
            priority: Priority::Normal,
            signature: None,
            sig_version: 0,
            hop_count: 0,
            max_hops: default_max_hops(),
            author_peer: None,
            e2e_version: 0,
            custody: false,
            custody_spray_sent: 0,
            max_custody_spray: default_max_custody_spray(),
            last_custody_from: None,
        }
    }

    pub fn sign(&mut self, keypair: &libp2p::identity::Keypair) -> anyhow::Result<()> {
        self.sig_version = 1;
        let bytes_to_sign = self.bytes_for_signing();
        let sig = keypair
            .sign(&bytes_to_sign)
            .map_err(|e| anyhow::anyhow!("failed to sign envelope: {e}"))?;
        self.signature = Some(sig);
        Ok(())
    }

    pub fn verify(&self, from_peer_id: &libp2p::PeerId) -> bool {
        let Some(sig) = &self.signature else {
            return false;
        };
        let bytes = if self.sig_version == 0 {
            self.bytes_for_signing_v0()
        } else {
            self.bytes_for_signing()
        };
        let multihash = from_peer_id.as_ref();
        if multihash.code() != 0 {
            return false;
        }
        let Ok(pk) = libp2p::identity::PublicKey::try_decode_protobuf(multihash.digest()) else {
            return false;
        };
        pk.verify(&bytes, sig)
    }

    /// Peer ID whose key signed this envelope (author, not relay hop).
    pub fn signature_author_peer(&self) -> &str {
        self.author_peer.as_deref().unwrap_or(&self.from_peer)
    }

    /// Verify mesh signature. The v1 transition window (May 2026) has expired.
    pub fn verify_or_transition(&self, from_peer_id: &libp2p::PeerId) -> bool {
        self.verify(from_peer_id)
    }

    fn bytes_for_signing(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.id.0.as_bytes());
        bytes.extend_from_slice(self.from_peer.as_bytes());
        if let Some(to) = &self.to_peer {
            bytes.extend_from_slice(to.as_bytes());
        }
        bytes.extend_from_slice(&self.payload);
        bytes.extend_from_slice(&self.created_at_ms.to_le_bytes());
        bytes.extend_from_slice(&self.ttl_secs.to_le_bytes());
        bytes.push(self.priority as u8);
        bytes.push(self.hop_count);
        bytes.push(self.max_hops);
        bytes.push(self.e2e_version);
        bytes.push(u8::from(self.custody));
        bytes.push(self.custody_spray_sent);
        bytes.push(self.max_custody_spray);
        bytes
    }

    fn bytes_for_signing_v0(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.from_peer.as_bytes());
        if let Some(to) = &self.to_peer {
            bytes.extend_from_slice(to.as_bytes());
        }
        bytes.extend_from_slice(&self.payload);
        bytes.extend_from_slice(&self.created_at_ms.to_le_bytes());
        bytes.extend_from_slice(&self.ttl_secs.to_le_bytes());
        bytes.push(self.priority as u8);
        bytes.push(self.hop_count);
        bytes.push(self.max_hops);
        bytes
    }
}

fn default_max_custody_spray() -> u8 {
    3
}

fn default_custody_max_hops() -> u8 {
    CUSTODY_DEFAULT_MAX_HOPS
}

/// Default hop budget for DTN custody messages (disaster profile).
pub const CUSTODY_DEFAULT_MAX_HOPS: u8 = 64;

fn created_at_from_payload(payload: &[u8]) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.get("created_at_ms").and_then(|x| x.as_u64())
}

fn default_max_hops() -> u8 {
    8
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn replay_attack_rejected() {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();

        let mut envelope = Envelope::new(peer_id.to_string(), None, b"original message".to_vec());
        envelope.sign(&keypair).unwrap();

        assert!(envelope.verify(&peer_id));

        let mut replayed = envelope.clone();
        replayed.created_at_ms += 1000;

        assert!(
            !replayed.verify(&peer_id),
            "replay with changed timestamp must fail verification"
        );
    }

    #[test]
    fn unsigned_gossip_id_is_stable_across_time() {
        let payload = br#"{"created_at_ms":1700000000000,"message":"hi"}"#;
        let a =
            Envelope::new_unsigned_gossip("peer-a".into(), None, payload.to_vec(), "mycena/sos/v1");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b =
            Envelope::new_unsigned_gossip("peer-a".into(), None, payload.to_vec(), "mycena/sos/v1");
        assert_eq!(a.id, b.id);
        assert_eq!(a.created_at_ms, 1_700_000_000_000);
        let other_scope =
            Envelope::new_unsigned_gossip("peer-a".into(), None, payload.to_vec(), "mycena/chat");
        assert_ne!(a.id, other_scope.id);
    }
}
