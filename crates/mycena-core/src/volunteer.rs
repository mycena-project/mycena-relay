// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Volunteer coordination tasks (phase 2) — assignment slots on top of resource map pins.

use crate::resource_map::{ResourceCategory, RESOURCE_MAX_DESC_LEN, RESOURCE_MAX_TTL_SECS};
use crate::sos::{SOS_MAX_GEOHASH_LEN, SOS_MIN_GEOHASH_LEN};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const VOLUNTEER_SCOPE: &str = "mycena/volunteer/v1";
pub const VOLUNTEER_DEFAULT_TTL_SECS: u64 = 12 * 3600;
pub const VOLUNTEER_MAX_SLOTS: u32 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolunteerClaim {
    pub peer_id: String,
    pub claimed_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolunteerTask {
    pub task_id: String,
    pub geohash: String,
    pub category: ResourceCategory,
    pub description: String,
    pub slots_needed: u32,
    pub claims: Vec<VolunteerClaim>,
    pub created_at_ms: u64,
    pub ttl_secs: u64,
}

impl VolunteerTask {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.task_id.trim().is_empty() {
            anyhow::bail!("task_id required");
        }
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
        if self.slots_needed == 0 || self.slots_needed > VOLUNTEER_MAX_SLOTS {
            anyhow::bail!("slots_needed must be 1–{VOLUNTEER_MAX_SLOTS}");
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

    pub fn slots_open(&self) -> u32 {
        self.slots_needed.saturating_sub(self.claims.len() as u32)
    }

    pub fn encode_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn decode_json(raw: &str) -> anyhow::Result<Self> {
        let task: Self = serde_json::from_str(raw)?;
        task.validate()?;
        Ok(task)
    }

    pub fn try_claim(&mut self, peer_id: &str, now_ms: u64) -> bool {
        let peer = peer_id.trim();
        if peer.is_empty() {
            return false;
        }
        if self.claims.iter().any(|c| c.peer_id == peer) {
            return true;
        }
        if self.slots_open() == 0 {
            return false;
        }
        self.claims.push(VolunteerClaim {
            peer_id: peer.to_string(),
            claimed_at_ms: now_ms,
        });
        true
    }
}

/// Merge duplicate task posts by `task_id` — union claims, keep newest metadata fields.
pub fn merge_volunteer_tasks(tasks: Vec<VolunteerTask>) -> Vec<VolunteerTask> {
    let mut by_id: std::collections::HashMap<String, VolunteerTask> =
        std::collections::HashMap::new();
    for task in tasks {
        by_id
            .entry(task.task_id.clone())
            .and_modify(|existing| merge_into(existing, &task))
            .or_insert(task);
    }
    let mut out: Vec<_> = by_id.into_values().collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.created_at_ms));
    out
}

fn merge_into(existing: &mut VolunteerTask, incoming: &VolunteerTask) {
    if incoming.created_at_ms > existing.created_at_ms {
        existing.geohash = incoming.geohash.clone();
        existing.category = incoming.category;
        existing.description = incoming.description.clone();
        existing.slots_needed = incoming.slots_needed;
        existing.ttl_secs = incoming.ttl_secs;
        existing.created_at_ms = incoming.created_at_ms;
    }
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for claim in existing.claims.iter().chain(incoming.claims.iter()) {
        if seen.insert(claim.peer_id.clone()) {
            merged.push(claim.clone());
        }
    }
    merged.sort_by_key(|c| c.claimed_at_ms);
    existing.claims = merged;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn sample_task(id: &str, slots: u32) -> VolunteerTask {
        VolunteerTask {
            task_id: id.into(),
            geohash: "u4pru".into(),
            category: ResourceCategory::Water,
            description: "Deliver bottles".into(),
            slots_needed: slots,
            claims: Vec::new(),
            created_at_ms: 1_000,
            ttl_secs: 3600,
        }
    }

    #[test]
    fn volunteer_task_roundtrip() {
        let task = sample_task("task-a", 3);
        let json = task.encode_json().unwrap();
        let back = VolunteerTask::decode_json(&json).unwrap();
        assert_eq!(back.task_id, "task-a");
    }

    #[test]
    fn claim_respects_slot_cap() {
        let mut task = sample_task("task-a", 2);
        assert!(task.try_claim("peer-a", 2_000));
        assert!(task.try_claim("peer-b", 2_001));
        assert!(!task.try_claim("peer-c", 2_002));
        assert!(task.try_claim("peer-a", 2_003));
    }

    #[test]
    fn merge_unions_claims() {
        let mut a = sample_task("task-a", 3);
        a.try_claim("peer-a", 1_100);
        let mut b = sample_task("task-a", 3);
        b.created_at_ms = 2_000;
        b.try_claim("peer-b", 2_100);
        let merged = merge_volunteer_tasks(vec![a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].claims.len(), 2);
        assert_eq!(merged[0].created_at_ms, 2_000);
    }
}
