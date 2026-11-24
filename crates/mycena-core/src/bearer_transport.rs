// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Unified bearer framing for BLE and future radio transports (ADR 002, ADR 006).
//!
//! F2 (side-channel wire) and F3 (libp2p stream CBOR) shared one GATT chunk layer (`MYLC`)
//! but used different inner magics (`MYBL` / `MYLP`). This module defines a single
//! [`BearerFrame`] envelope with a version byte and routes both legacy formats on decode.

pub const CHUNK_MAGIC: [u8; 4] = *b"MYLC";
/// Unified bearer envelope (v1+).
pub const BEARER_MAGIC: [u8; 4] = *b"MYBR";
pub const BEARER_VERSION_V1: u8 = 1;
/// Legacy F2 side-channel (wire-compatible with pre-v1 peers).
pub const LEGACY_WIRE_MAGIC: [u8; 4] = *b"MYBL";
/// Legacy F3 stream (wire-compatible with pre-v1 peers).
pub const LEGACY_STREAM_MAGIC: [u8; 4] = *b"MYLP";

pub const MAX_FRAME_LEN: usize = 4096;
pub const GATT_CHUNK_PAYLOAD: usize = 476;
const LEGACY_HEADER: usize = 8;
const V1_HEADER: usize = 11;

/// Delivery semantics exposed to the node layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReliabilityClass {
    /// F2-style side channel — no libp2p direct ack.
    BestEffort,
    /// F3-style libp2p direct request/response over the bearer.
    Acknowledged,
}

impl ReliabilityClass {
    pub fn from_payload_kind(kind: BearerPayloadKind) -> Self {
        match kind {
            BearerPayloadKind::SideChannelWire | BearerPayloadKind::LegacyWire => Self::BestEffort,
            BearerPayloadKind::StreamDirect | BearerPayloadKind::LegacyStream => Self::Acknowledged,
        }
    }
}

/// Logical payload carried inside a bearer frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerPayloadKind {
    SideChannelWire = 0,
    StreamDirect = 1,
    LegacyWire = 0x10,
    LegacyStream = 0x11,
}

impl BearerPayloadKind {
    pub fn legacy_magic(self) -> Option<[u8; 4]> {
        match self {
            Self::SideChannelWire | Self::LegacyWire => Some(LEGACY_WIRE_MAGIC),
            Self::StreamDirect | Self::LegacyStream => Some(LEGACY_STREAM_MAGIC),
        }
    }
}

/// MTU hints for host chunking (GATT today; Wi‑Fi Direct / LoRa later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BearerMtu {
    pub max_frame: usize,
    pub chunk_payload: usize,
}

impl Default for BearerMtu {
    fn default() -> Self {
        Self {
            max_frame: MAX_FRAME_LEN,
            chunk_payload: GATT_CHUNK_PAYLOAD,
        }
    }
}

/// Physical bearer selected by the host (BLE low-power idle vs Wi‑Fi Direct bulk vs Wi‑Fi Aware discovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BearerMedium {
    Ble = 0,
    WifiDirect = 1,
    /// Discovery-only: low-duty peer presence (Android Wi‑Fi Aware / NSD). Not used for bulk data.
    WifiAware = 2,
    /// WebRTC RTCDataChannel (phone↔desktop/browser). Signaling rides libp2p relay; data is direct.
    WebRtc = 3,
}

impl BearerMedium {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Ble),
            1 => Some(Self::WifiDirect),
            2 => Some(Self::WifiAware),
            3 => Some(Self::WebRtc),
            _ => None,
        }
    }

    pub fn is_discovery_only(self) -> bool {
        matches!(self, Self::WifiAware)
    }
}

pub const WIFI_DIRECT_MAX_FRAME_LEN: usize = 65_536;
pub const WIFI_DIRECT_CHUNK_PAYLOAD: usize = 60_000;
pub const WEBRTC_MAX_FRAME_LEN: usize = 65_536;
pub const WEBRTC_CHUNK_PAYLOAD: usize = 60_000;

pub fn wifi_direct_mtu() -> BearerMtu {
    BearerMtu {
        max_frame: WIFI_DIRECT_MAX_FRAME_LEN,
        chunk_payload: WIFI_DIRECT_CHUNK_PAYLOAD,
    }
}

pub fn webrtc_mtu() -> BearerMtu {
    BearerMtu {
        max_frame: WEBRTC_MAX_FRAME_LEN,
        chunk_payload: WEBRTC_CHUNK_PAYLOAD,
    }
}

/// Default 16 KiB — override with `MYCENA_BEARER_WIFI_UPGRADE_BYTES`.
pub fn bearer_wifi_upgrade_threshold() -> usize {
    std::env::var("MYCENA_BEARER_WIFI_UPGRADE_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16 * 1024)
}

/// True when payload should use Wi‑Fi Direct (bulk / mini-app bundle / large bulletin).
/// Prefer [`TransportScoreStore::prefer_wifi_over_ble`] when measured scores exist.
pub fn should_upgrade_to_wifi_direct(encoded_len: usize, force_bulk: bool) -> bool {
    let store = crate::transport_score::TransportScoreStore::default();
    store.prefer_wifi_over_ble(encoded_len, force_bulk, 0)
}

/// On-the-wire encoding style for outbound frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BearerWireVersion {
    /// Emit legacy `MYBL` / `MYLP` (default — old peers still decode).
    #[default]
    Legacy,
    /// Emit unified `MYBR` v1 (enable with `MYCENA_BEARER_WIRE_V1=1`).
    V1,
}

pub fn default_wire_version() -> BearerWireVersion {
    match std::env::var("MYCENA_BEARER_WIRE_V1").as_deref() {
        Ok("1") | Ok("true") | Ok("TRUE") => BearerWireVersion::V1,
        _ => BearerWireVersion::Legacy,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerFrame {
    pub wire_version: u8,
    pub kind: BearerPayloadKind,
    pub payload: Vec<u8>,
}

impl BearerFrame {
    pub fn side_channel_wire(payload: Vec<u8>) -> Self {
        Self {
            wire_version: BEARER_VERSION_V1,
            kind: BearerPayloadKind::SideChannelWire,
            payload,
        }
    }

    pub fn stream_direct(payload: Vec<u8>) -> Self {
        Self {
            wire_version: BEARER_VERSION_V1,
            kind: BearerPayloadKind::StreamDirect,
            payload,
        }
    }

    pub fn reliability(&self) -> ReliabilityClass {
        ReliabilityClass::from_payload_kind(self.kind)
    }

    pub fn encode(&self, style: BearerWireVersion) -> anyhow::Result<Vec<u8>> {
        encode_bearer_frame(self.kind, &self.payload, style)
    }
}

pub fn encode_bearer_frame(
    kind: BearerPayloadKind,
    payload: &[u8],
    style: BearerWireVersion,
) -> anyhow::Result<Vec<u8>> {
    encode_bearer_frame_with_limit(kind, payload, style, MAX_FRAME_LEN)
}

pub fn encode_bearer_frame_with_limit(
    kind: BearerPayloadKind,
    payload: &[u8],
    style: BearerWireVersion,
    max_payload_len: usize,
) -> anyhow::Result<Vec<u8>> {
    if payload.len() > max_payload_len {
        anyhow::bail!(
            "bearer payload too large ({len} > {max_payload_len})",
            len = payload.len()
        );
    }
    match style {
        BearerWireVersion::Legacy => {
            let magic = kind
                .legacy_magic()
                .ok_or_else(|| anyhow::anyhow!("payload kind has no legacy wire mapping"))?;
            let mut out = Vec::with_capacity(LEGACY_HEADER + payload.len());
            out.extend_from_slice(&magic);
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(payload);
            Ok(out)
        }
        BearerWireVersion::V1 => {
            let mut out = Vec::with_capacity(V1_HEADER + payload.len());
            out.extend_from_slice(&BEARER_MAGIC);
            out.push(BEARER_VERSION_V1);
            out.push(kind as u8);
            out.push(0); // flags reserved
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(payload);
            Ok(out)
        }
    }
}

pub fn decode_bearer_frame(bytes: &[u8]) -> anyhow::Result<BearerFrame> {
    if bytes.len() < 4 {
        anyhow::bail!("bearer frame too short");
    }
    match &bytes[..4] {
        magic if magic == BEARER_MAGIC => decode_v1(bytes),
        magic if magic == LEGACY_WIRE_MAGIC => decode_legacy(bytes, BearerPayloadKind::LegacyWire),
        magic if magic == LEGACY_STREAM_MAGIC => {
            decode_legacy(bytes, BearerPayloadKind::LegacyStream)
        }
        _ => anyhow::bail!("unknown bearer frame magic"),
    }
}

fn decode_v1(bytes: &[u8]) -> anyhow::Result<BearerFrame> {
    if bytes.len() < V1_HEADER {
        anyhow::bail!("MYBR frame truncated");
    }
    let version = bytes[4];
    if version != BEARER_VERSION_V1 {
        anyhow::bail!("unsupported MYBR version {version}");
    }
    let kind = match bytes[5] {
        0 => BearerPayloadKind::SideChannelWire,
        1 => BearerPayloadKind::StreamDirect,
        other => anyhow::bail!("unknown MYBR payload kind {other}"),
    };
    let len = u32::from_be_bytes(bytes[7..11].try_into()?) as usize;
    if len > MAX_FRAME_LEN {
        anyhow::bail!("MYBR length exceeds max");
    }
    if bytes.len() < V1_HEADER + len {
        anyhow::bail!("MYBR frame truncated");
    }
    Ok(BearerFrame {
        wire_version: version,
        kind,
        payload: bytes[V1_HEADER..V1_HEADER + len].to_vec(),
    })
}

fn decode_legacy(bytes: &[u8], kind: BearerPayloadKind) -> anyhow::Result<BearerFrame> {
    if bytes.len() < LEGACY_HEADER {
        anyhow::bail!("legacy bearer frame truncated");
    }
    let len = u32::from_be_bytes(bytes[4..8].try_into()?) as usize;
    if len > MAX_FRAME_LEN {
        anyhow::bail!("legacy bearer length exceeds max");
    }
    if bytes.len() < LEGACY_HEADER + len {
        anyhow::bail!("legacy bearer frame truncated");
    }
    Ok(BearerFrame {
        wire_version: 0,
        kind,
        payload: bytes[LEGACY_HEADER..LEGACY_HEADER + len].to_vec(),
    })
}

/// Split an encoded bearer frame into GATT-sized chunks (`MYLC | idx | total | payload`).
pub fn chunk_frame(frame: &[u8]) -> Vec<Vec<u8>> {
    if frame.is_empty() {
        return Vec::new();
    }
    let chunk_count = frame.len().div_ceil(GATT_CHUNK_PAYLOAD).min(255) as u8;
    let mut out = Vec::with_capacity(chunk_count as usize);
    for (idx, piece) in frame.chunks(GATT_CHUNK_PAYLOAD).enumerate() {
        if idx >= 255 {
            break;
        }
        let mut chunk = Vec::with_capacity(6 + piece.len());
        chunk.extend_from_slice(&CHUNK_MAGIC);
        chunk.push(idx as u8);
        chunk.push(chunk_count);
        chunk.extend_from_slice(piece);
        out.push(chunk);
    }
    out
}

/// Incrementally reassemble GATT chunks into a full bearer frame.
#[derive(Debug, Default)]
pub struct ChunkAssembler {
    total: u8,
    parts: Vec<Option<Vec<u8>>>,
}

impl ChunkAssembler {
    pub fn reset(&mut self) {
        self.total = 0;
        self.parts.clear();
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
        if chunk.len() < 6 || chunk[..4] != CHUNK_MAGIC {
            anyhow::bail!("invalid bearer chunk");
        }
        let idx = chunk[4] as usize;
        let total = chunk[5];
        if total == 0 {
            anyhow::bail!("invalid chunk total");
        }
        if idx >= total as usize {
            anyhow::bail!("chunk index out of range");
        }
        if self.total == 0 {
            self.total = total;
            self.parts = vec![None; total as usize];
        } else if self.total != total {
            self.reset();
            self.total = total;
            self.parts = vec![None; total as usize];
        }
        self.parts[idx] = Some(chunk[6..].to_vec());
        if self.parts.iter().any(|p| p.is_none()) {
            return Ok(None);
        }
        let mut frame = Vec::new();
        for part in self.parts.drain(..) {
            frame.extend_from_slice(&part.unwrap_or_default());
        }
        self.total = 0;
        Ok(Some(frame))
    }
}

/// Host-facing bearer I/O contract (BLE data, Wi‑Fi Direct bulk, Wi‑Fi Aware discovery).
pub trait BearerTransport: Send {
    fn mtu(&self) -> BearerMtu;
    fn reliability_class(&self) -> ReliabilityClass;
    /// False for discovery-only bearers ([`BearerMedium::WifiAware`]).
    fn carries_mesh_data(&self) -> bool {
        true
    }
    /// Queue an encoded bearer frame (post-chunk assembly) to `peer`.
    fn send_frame(&mut self, peer: &str, frame: &[u8]) -> anyhow::Result<()>;
    /// Take one completed inbound frame from `peer`, if any.
    fn recv_frame(&mut self, peer: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// Drain up to `max` outbound encoded frames for `peer`.
    fn drain_outbound(&mut self, peer: &str, max: usize) -> Vec<Vec<u8>>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn v1_roundtrip_side_channel() {
        let payload = b"wire-bytes".to_vec();
        let bytes = encode_bearer_frame(
            BearerPayloadKind::SideChannelWire,
            &payload,
            BearerWireVersion::V1,
        )
        .unwrap();
        let frame = decode_bearer_frame(&bytes).unwrap();
        assert_eq!(frame.kind, BearerPayloadKind::SideChannelWire);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn legacy_mybl_decode() {
        let payload = b"legacy".to_vec();
        let bytes = encode_bearer_frame(
            BearerPayloadKind::SideChannelWire,
            &payload,
            BearerWireVersion::Legacy,
        )
        .unwrap();
        assert_eq!(&bytes[..4], LEGACY_WIRE_MAGIC);
        let frame = decode_bearer_frame(&bytes).unwrap();
        assert_eq!(frame.kind, BearerPayloadKind::LegacyWire);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn legacy_mylp_decode() {
        let payload = b"stream".to_vec();
        let bytes = encode_bearer_frame(
            BearerPayloadKind::StreamDirect,
            &payload,
            BearerWireVersion::Legacy,
        )
        .unwrap();
        assert_eq!(&bytes[..4], LEGACY_STREAM_MAGIC);
        let frame = decode_bearer_frame(&bytes).unwrap();
        assert_eq!(frame.kind, BearerPayloadKind::LegacyStream);
    }

    #[test]
    fn wifi_upgrade_uses_score_not_static_threshold_only() {
        let store = crate::transport_score::TransportScoreStore::default();
        assert!(!store.prefer_wifi_over_ble(1024, false, 0));
        assert!(store.prefer_wifi_over_ble(32 * 1024, false, 0));
        assert!(should_upgrade_to_wifi_direct(64, true));
    }

    #[test]
    fn chunk_reassembly() {
        let frame = encode_bearer_frame(
            BearerPayloadKind::StreamDirect,
            &vec![0u8; 900],
            BearerWireVersion::V1,
        )
        .unwrap();
        let chunks = chunk_frame(&frame);
        assert!(chunks.len() > 1);
        let mut asm = ChunkAssembler::default();
        let mut done = None;
        for c in chunks {
            if let Some(f) = asm.push_chunk(&c).unwrap() {
                done = Some(f);
            }
        }
        assert_eq!(done.as_deref(), Some(frame.as_slice()));
    }
}
