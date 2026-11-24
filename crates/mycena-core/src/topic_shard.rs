// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Geohash-prefix gossip topic sharding (AR-06) — avoids one global mesh topic.

use crate::sos::{geohash_from_payload_bytes, SOS_SCOPE};

const GEO_SEGMENT: &str = "g";

/// Default geohash prefix length for gossip shards (`MYCENA_GEOHASH_SHARD_LEN`).
pub const DEFAULT_SHARD_PREFIX_LEN: usize = 4;

/// Read shard prefix length from env (4–8).
pub fn shard_prefix_len() -> usize {
    std::env::var("MYCENA_GEOHASH_SHARD_LEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SHARD_PREFIX_LEN)
        .clamp(4, 8)
}

/// Scopes that shard by geohash prefix instead of a single global topic.
pub fn supports_geohash_sharding(base_scope: &str) -> bool {
    base_scope == SOS_SCOPE || base_scope.starts_with("mycena/bulletin/")
}

/// Normalize a geohash to the gossip shard prefix.
pub fn shard_prefix(geohash: &str) -> Option<String> {
    let gh = geohash.trim();
    if gh.len() < 4 {
        return None;
    }
    if !gh
        .chars()
        .all(|c| matches!(c, '0'..='9' | 'b'..='h' | 'j'..='n' | 'p'..='z'))
    {
        return None;
    }
    let len = shard_prefix_len().min(gh.len());
    Some(gh[..len].to_string())
}

/// Gossip topic string: `{base}/g/{prefix}`.
pub fn gossip_topic(base_scope: &str, geohash: &str) -> Option<String> {
    let prefix = shard_prefix(geohash)?;
    gossip_topic_for_prefix(base_scope, &prefix)
}

/// Gossip topic for an already-normalized prefix (4+ valid geohash chars).
pub fn gossip_topic_for_prefix(base_scope: &str, prefix: &str) -> Option<String> {
    let p = prefix.trim();
    if p.len() < 3 {
        return None;
    }
    if !p
        .chars()
        .all(|c| matches!(c, '0'..='9' | 'b'..='h' | 'j'..='n' | 'p'..='z'))
    {
        return None;
    }
    Some(format!("{base_scope}/{GEO_SEGMENT}/{p}"))
}

/// Prefix lengths to subscribe for SOS listen (local shard + parent prefixes).
pub fn listen_shard_prefixes(geohash: &str) -> Vec<String> {
    let min_len = std::env::var("MYCENA_SOS_LISTEN_MIN_PREFIX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    listen_shard_prefixes_min(geohash, min_len)
}

pub fn listen_shard_prefixes_min(geohash: &str, min_len: usize) -> Vec<String> {
    let gh = geohash.trim();
    if gh.len() < 4 {
        return Vec::new();
    }
    let max_len = shard_prefix_len().min(gh.len());
    let min_len = min_len.clamp(3, max_len);
    let mut out = Vec::new();
    for len in (min_len..=max_len).rev() {
        let p = &gh[..len];
        if p.chars()
            .all(|c| matches!(c, '0'..='9' | 'b'..='h' | 'j'..='n' | 'p'..='z'))
        {
            out.push(p.to_string());
        }
    }
    out
}

/// Pattern stored in subscribed_scopes for wildcard ingest matching.
pub fn subscribe_pattern(base_scope: &str) -> String {
    format!("{base_scope}/{GEO_SEGMENT}/*")
}

/// Resolve gossip publish topics for a scoped payload.
pub fn resolve_publish_topics(base_scope: &str, payload: &[u8]) -> Vec<String> {
    resolve_publish_topics_with_hint(base_scope, payload, None)
}

/// Like [`resolve_publish_topics`] but falls back to an explicit geohash hint (bulletin post API).
pub fn resolve_publish_topics_with_hint(
    base_scope: &str,
    payload: &[u8],
    geohash_hint: Option<&str>,
) -> Vec<String> {
    if supports_geohash_sharding(base_scope) {
        let gh = geohash_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| geohash_from_payload_bytes(payload))
            .or_else(|| extract_geohash_field(payload));
        if let Some(gh) = gh {
            if let Some(topic) = gossip_topic(base_scope, &gh) {
                return vec![topic];
            }
        }
    }
    vec![base_scope.to_string()]
}

/// Gossip topics to join when subscribing to a location-aware scope.
pub fn resolve_subscribe_topics(base_scope: &str, local_geohash: Option<&str>) -> Vec<String> {
    if !supports_geohash_sharding(base_scope) {
        return vec![base_scope.to_string()];
    }
    let Some(gh) = local_geohash.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    listen_shard_prefixes(gh)
        .into_iter()
        .filter_map(|prefix| gossip_topic_for_prefix(base_scope, &prefix))
        .collect()
}

fn extract_geohash_field(payload: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.get("geohash")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn sos_shards_by_geohash_prefix() {
        let topic = gossip_topic(SOS_SCOPE, "u4pruyd").expect("topic");
        assert_eq!(topic, "mycena/sos/v1/g/u4pr");
    }

    #[test]
    fn publish_resolves_shard_from_json() {
        let payload = br#"{"geohash":"u4pru","message":"help","created_at_ms":1}"#;
        let topics = resolve_publish_topics(SOS_SCOPE, payload);
        assert_eq!(topics, vec!["mycena/sos/v1/g/u4pr".to_string()]);
    }

    #[test]
    fn subscribe_requires_local_geohash() {
        assert!(resolve_subscribe_topics(SOS_SCOPE, None).is_empty());
        let topics: Vec<String> = listen_shard_prefixes_min("u4pru", 4)
            .into_iter()
            .filter_map(|p| gossip_topic_for_prefix(SOS_SCOPE, &p))
            .collect();
        assert_eq!(topics, vec!["mycena/sos/v1/g/u4pr".to_string()]);
    }

    #[test]
    fn subscribe_includes_parent_prefixes() {
        let topics: Vec<String> = listen_shard_prefixes_min("u4pruyd", 3)
            .into_iter()
            .filter_map(|p| gossip_topic_for_prefix(SOS_SCOPE, &p))
            .collect();
        assert!(topics.contains(&"mycena/sos/v1/g/u4pr".to_string()));
        assert!(topics.contains(&"mycena/sos/v1/g/u4p".to_string()));
    }

    #[test]
    fn publish_uses_geohash_hint_for_bulletin() {
        let scope = "mycena/bulletin/city";
        let payload = b"not-json";
        let topics = resolve_publish_topics_with_hint(scope, payload, Some("u4pru"));
        assert_eq!(topics, vec!["mycena/bulletin/city/g/u4pr".to_string()]);
    }

    #[test]
    fn non_sharded_scope_stays_global() {
        let topics = resolve_publish_topics("mycena/chat", b"hello");
        assert_eq!(topics, vec!["mycena/chat".to_string()]);
    }
}
