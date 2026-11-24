// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Offline **resource map** entries (water, food, shelter, medicine, power).
//!
//! Spread like SOS via scoped gossip; TTL prevents stale disaster data.

pub const RESOURCE_MAP_SCOPE: &str = "mycena/resource-map/v1";
pub const RESOURCE_MAX_DESC_LEN: usize = 200;
pub const RESOURCE_DEFAULT_TTL_SECS: u64 = 6 * 3600;
pub const RESOURCE_MAX_TTL_SECS: u64 = 72 * 3600;

use crate::sos::{SOS_MAX_GEOHASH_LEN, SOS_MIN_GEOHASH_LEN};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCategory {
    Water,
    Food,
    Shelter,
    Medicine,
    Power,
}

impl ResourceCategory {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "water" => Some(Self::Water),
            "food" => Some(Self::Food),
            "shelter" => Some(Self::Shelter),
            "medicine" | "medical" => Some(Self::Medicine),
            "power" | "electricity" => Some(Self::Power),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Water => "water",
            Self::Food => "food",
            Self::Shelter => "shelter",
            Self::Medicine => "medicine",
            Self::Power => "power",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceMapEntry {
    pub geohash: String,
    pub category: ResourceCategory,
    pub description: String,
    pub created_at_ms: u64,
    pub ttl_secs: u64,
}

impl ResourceMapEntry {
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
        let desc = self.description.trim();
        if desc.is_empty() || desc.len() > RESOURCE_MAX_DESC_LEN {
            anyhow::bail!("description must be 1–{RESOURCE_MAX_DESC_LEN} chars");
        }
        if self.ttl_secs == 0 || self.ttl_secs > RESOURCE_MAX_TTL_SECS {
            anyhow::bail!("ttl_secs must be 1–{RESOURCE_MAX_TTL_SECS}");
        }
        Ok(())
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.created_at_ms
            .saturating_add(self.ttl_secs.saturating_mul(1000))
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms()
    }

    pub fn encode_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn decode_json(raw: &str) -> anyhow::Result<Self> {
        let entry: Self = serde_json::from_str(raw)?;
        entry.validate()?;
        Ok(entry)
    }
}

pub fn geohash_from_resource_payload_bytes(payload: &[u8]) -> Option<String> {
    geohash_from_json_field(payload, "geohash")
}

fn geohash_from_json_field(payload: &[u8], field: &str) -> Option<String> {
    let raw = std::str::from_utf8(payload).ok()?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get(field)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn is_essential_gossip_scope(scope: &str) -> bool {
    scope.starts_with(crate::sos::SOS_SCOPE)
        || scope.starts_with(RESOURCE_MAP_SCOPE)
        || scope.starts_with(crate::volunteer::VOLUNTEER_SCOPE)
}

pub fn is_background_gossip_scope(scope: &str) -> bool {
    scope.starts_with("mycena/coin/v1")
        || scope == "mycena/appstore/v1"
        || scope.starts_with("mycena/appstore/bundles")
}

/// Non-emergency gossip deferred while Crisis Mode is active (bulletin boards, coin, app store).
pub fn should_defer_gossip_in_crisis(scope: &str) -> bool {
    if is_essential_gossip_scope(scope) {
        return false;
    }
    if is_background_gossip_scope(scope) {
        return true;
    }
    if scope.starts_with("mycena/chat")
        || scope.starts_with("mycena/group/")
        || scope.starts_with("mycena/identity/")
        || scope.starts_with("mycena/proximity/")
    {
        return false;
    }
    scope.starts_with("mycena/")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn resource_entry_roundtrip() {
        let e = ResourceMapEntry {
            geohash: "u4pru".into(),
            category: ResourceCategory::Water,
            description: "Fountain at plaza".into(),
            created_at_ms: 1_000,
            ttl_secs: 3600,
        };
        e.validate().unwrap();
        let json = e.encode_json().unwrap();
        let back = ResourceMapEntry::decode_json(&json).unwrap();
        assert_eq!(back.category, ResourceCategory::Water);
        assert!(!e.is_expired(500));
        assert!(e.is_expired(4_000_000));
    }

    #[test]
    fn defer_bulletin_not_chat_in_crisis() {
        assert!(should_defer_gossip_in_crisis("mycena/general"));
        assert!(!should_defer_gossip_in_crisis("mycena/chat"));
        assert!(!should_defer_gossip_in_crisis("mycena/sos/v1"));
        assert!(!should_defer_gossip_in_crisis(
            crate::volunteer::VOLUNTEER_SCOPE
        ));
    }
}
