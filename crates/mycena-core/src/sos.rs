// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Emergency SOS scope helpers (Phase C3 / E1).

pub const SOS_SCOPE: &str = "mycena/sos/v1";
pub const MESHAID_SCOPE: &str = "mycena/meshaid/v1";
pub const MESHAID_APP_ID: &str = "io.github.mycena_project.meshaid";
pub const SOS_MAX_MESSAGE_LEN: usize = 280;
pub const SOS_MAX_GEOHASH_LEN: usize = 12;
pub const SOS_MIN_GEOHASH_LEN: usize = 4;
pub const SOS_DEFAULT_TTL_SECS: u64 = 3600;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SosPayload {
    pub geohash: String,
    pub message: String,
    pub created_at_ms: u64,
}

impl SosPayload {
    pub fn validate(&self) -> anyhow::Result<()> {
        let gh = self.geohash.trim();
        if gh.len() < SOS_MIN_GEOHASH_LEN || gh.len() > SOS_MAX_GEOHASH_LEN {
            anyhow::bail!("geohash length must be {SOS_MIN_GEOHASH_LEN}–{SOS_MAX_GEOHASH_LEN}");
        }
        if !gh
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'b'..='h' | 'j'..='n' | 'p'..='z'))
        {
            anyhow::bail!("invalid geohash characters");
        }
        let msg = self.message.trim();
        if msg.is_empty() || msg.len() > SOS_MAX_MESSAGE_LEN {
            anyhow::bail!("message must be 1–{SOS_MAX_MESSAGE_LEN} chars");
        }
        Ok(())
    }

    pub fn encode_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn decode_json(raw: &str) -> anyhow::Result<Self> {
        let payload: Self = serde_json::from_str(raw)?;
        payload.validate()?;
        Ok(payload)
    }
}

/// Prefix length for mesh routing / custody geographic diversity (~20 km at 4 chars).
pub const GEOHASH_DIVERSITY_PREFIX_LEN: usize = 4;

/// Coarse geohash bucket for local mesh peer diversity (BLE / Wi‑Fi Direct have no ASN).
pub fn geohash_diversity_bucket(geohash: &str, prefix_len: usize) -> Option<String> {
    let gh = geohash.trim();
    if gh.len() < SOS_MIN_GEOHASH_LEN {
        return None;
    }
    if !gh
        .chars()
        .all(|c| matches!(c, '0'..='9' | 'b'..='h' | 'j'..='n' | 'p'..='z'))
    {
        return None;
    }
    let n = prefix_len.min(gh.len());
    Some(gh[..n].to_string())
}

/// Parse geohash from a SOS JSON bulletin body (for rate limiting at ingest).
pub fn geohash_from_payload_bytes(payload: &[u8]) -> Option<String> {
    let raw = std::str::from_utf8(payload).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("geohash")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn sos_payload_validates() {
        let ok = SosPayload {
            geohash: "u4pru".into(),
            message: "Need help".into(),
            created_at_ms: 1,
        };
        ok.validate().unwrap();
        let bad = SosPayload {
            geohash: "!!!".into(),
            message: "".into(),
            created_at_ms: 1,
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn geohash_diversity_bucket_truncates_prefix() {
        assert_eq!(
            geohash_diversity_bucket("u4pruydq", 4).as_deref(),
            Some("u4pr")
        );
        assert!(geohash_diversity_bucket("!!!", 4).is_none());
    }

    #[test]
    fn geohash_from_payload_bytes_parses_json() {
        let json = r#"{"geohash":"u4pru","message":"help","created_at_ms":1}"#;
        assert_eq!(
            geohash_from_payload_bytes(json.as_bytes()).as_deref(),
            Some("u4pru")
        );
    }
}
