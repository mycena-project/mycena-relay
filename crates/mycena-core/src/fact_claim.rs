// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Extract SOS / resource-map fact claims from mesh gossip payloads at node ingest.

use crate::resource_map::{ResourceMapEntry, RESOURCE_MAP_SCOPE};
use crate::sos::{SosPayload, SOS_SCOPE};
use serde_json::Value;

/// Parsed fact claim from a scoped gossip payload (plaintext or embedded JSON).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractedFactClaim {
    Sos(SosPayload),
    Resource(ResourceMapEntry),
}

/// Best-effort extraction for node-layer corroboration recording before app decrypt.
pub fn extract_fact_claim_from_gossip_payload(
    scope: &str,
    payload: &[u8],
) -> Option<ExtractedFactClaim> {
    if payload.starts_with(b"enc1:") {
        return None;
    }
    let json = extract_json_with_geohash(payload)?;
    if scope.starts_with(SOS_SCOPE) {
        let payload = SosPayload {
            geohash: json_string(&json, "geohash")?,
            message: json_string(&json, "message")?,
            created_at_ms: json
                .get("created_at_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
        payload.validate().ok()?;
        return Some(ExtractedFactClaim::Sos(payload));
    }
    if scope.starts_with(RESOURCE_MAP_SCOPE) {
        let category = json_string(&json, "category")
            .and_then(|s| crate::resource_map::ResourceCategory::parse(&s))?;
        let entry = ResourceMapEntry {
            geohash: json_string(&json, "geohash")?,
            category,
            description: json_string(&json, "description")?,
            created_at_ms: json
                .get("created_at_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            ttl_secs: json.get("ttl_secs").and_then(Value::as_u64).unwrap_or(0),
        };
        entry.validate().ok()?;
        return Some(ExtractedFactClaim::Resource(entry));
    }
    None
}

fn json_string(v: &Value, field: &str) -> Option<String> {
    v.get(field)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn extract_json_with_geohash(payload: &[u8]) -> Option<Value> {
    if let Ok(raw) = std::str::from_utf8(payload) {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            if v.get("geohash").is_some() {
                return Some(v);
            }
        }
    }
    for (offset, byte) in payload.iter().enumerate() {
        if *byte != b'{' {
            continue;
        }
        let tail = &payload[offset..];
        if let Ok(raw) = std::str::from_utf8(tail) {
            if let Ok(v) = serde_json::from_str::<Value>(raw) {
                if v.get("geohash").is_some() {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn extracts_sos_from_plain_json() {
        let json = r#"{"geohash":"u4pru","message":"need help","created_at_ms":1}"#;
        let claim =
            extract_fact_claim_from_gossip_payload(SOS_SCOPE, json.as_bytes()).expect("sos");
        match claim {
            ExtractedFactClaim::Sos(p) => assert_eq!(p.message, "need help"),
            _ => panic!("expected sos"),
        }
    }

    #[test]
    fn extracts_resource_from_embedded_json() {
        let prefix = b"\x00\x01bulletin\xff";
        let mut payload = prefix.to_vec();
        payload.extend_from_slice(
            br#"{"geohash":"u4pruyd","category":"water","description":"Fountain","created_at_ms":1,"ttl_secs":3600}"#,
        );
        let claim =
            extract_fact_claim_from_gossip_payload(RESOURCE_MAP_SCOPE, &payload).expect("resource");
        match claim {
            ExtractedFactClaim::Resource(e) => assert_eq!(e.description, "Fountain"),
            _ => panic!("expected resource"),
        }
    }

    #[test]
    fn encrypted_payload_skipped() {
        assert!(extract_fact_claim_from_gossip_payload(SOS_SCOPE, b"enc1:deadbeef").is_none());
    }
}
