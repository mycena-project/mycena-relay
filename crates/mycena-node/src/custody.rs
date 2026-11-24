// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Store-and-forward custody queue — spray-and-wait with bounded network copies.

use crate::encounter_log::{
    CustodyCandidateQuery, CustodyCandidateScoring, EncounterLogStore, ExpectedDeliveryBasis,
    DEFAULT_EXPECTED_ETA_MAX_MS, DEFAULT_EXPECTED_ETA_MIN_MS, ENCOUNTER_LOG_TTL_MS,
};
use crate::peer_trust::PeerTrust;
use crate::security::{
    apply_peer_diversity, build_routing_meta_map, peer_routing_meta, PeerDiversityConfig,
    PeerRoutingMeta,
};
use mycena_core::data::now_ms;
use mycena_core::protocol_capabilities::NegotiatedProtocol;
use mycena_core::transport::{DirectMessage, WireMessage};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Default max active custody copies in the mesh (spray-and-wait).
pub const DEFAULT_MAX_CUSTODY_SPRAY: u8 = 3;

/// Optional trust / diversity inputs for custody carrier selection.
#[derive(Debug, Clone, Default)]
pub struct CustodyRoutingContext {
    pub peer_trust: HashMap<String, PeerTrust>,
    pub routing_meta: HashMap<String, PeerRoutingMeta>,
    pub diversity: PeerDiversityConfig,
    pub negotiated_caps: HashMap<String, NegotiatedProtocol>,
    pub candidate_scoring: CustodyCandidateScoring,
}

impl CustodyRoutingContext {
    pub fn diversity_for_dest(&self, dest_peer: Option<&str>) -> PeerDiversityConfig {
        if self.negotiated_caps.is_empty() {
            return PeerDiversityConfig::score_only();
        }
        let Some(dest) = dest_peer.filter(|d| !d.is_empty()) else {
            return self.diversity;
        };
        if self
            .negotiated_caps
            .get(dest)
            .is_some_and(|n| n.relay_diversity_allowed())
        {
            self.diversity
        } else {
            PeerDiversityConfig::score_only()
        }
    }

    pub fn from_encounters(
        connected: &[String],
        encounter_log: &EncounterLogStore,
        _now: u64,
    ) -> anyhow::Result<Self> {
        let mut encounter_first = HashMap::new();
        for p in connected {
            if let Some(rec) = encounter_log.record(p)? {
                encounter_first.insert(p.clone(), rec.first_seen_ms);
            }
        }
        Ok(Self {
            peer_trust: HashMap::new(),
            routing_meta: build_routing_meta_map(
                connected,
                &HashMap::new(),
                &encounter_first,
                &HashMap::new(),
            ),
            diversity: PeerDiversityConfig::default(),
            negotiated_caps: HashMap::new(),
            candidate_scoring: CustodyCandidateScoring::RecencyOnly,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustodyConfig {
    pub max_spray_copies: u8,
    pub max_queue_entries: usize,
    /// Background flush interval; `None` for deterministic single-threaded tests.
    #[serde(default = "default_flush_every_ms")]
    pub flush_every_ms: Option<u64>,
}

fn default_flush_every_ms() -> Option<u64> {
    Some(5000)
}

impl Default for CustodyConfig {
    fn default() -> Self {
        Self {
            max_spray_copies: DEFAULT_MAX_CUSTODY_SPRAY,
            max_queue_entries: 512,
            flush_every_ms: default_flush_every_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCustody {
    message: DirectMessage,
    /// Peers already used for spray (origin) or relay handoff (carrier).
    targets_tried: HashSet<String>,
    enqueued_at_ms: u64,
    /// Origin-only: custody copies released into the mesh.
    #[serde(default)]
    sprays_released: u8,
    #[serde(default)]
    app_chat_id: Option<String>,
    #[serde(default)]
    encrypted_wire: Option<WireMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpectedDelivery {
    /// Earliest plausible delivery (milliseconds from now).
    pub eta_min_ms: u64,
    /// Latest plausible delivery (milliseconds from now).
    pub eta_max_ms: u64,
    /// Confidence in the window, 0.0–1.0.
    pub confidence: f64,
    /// Connected peers that could carry toward the destination now.
    pub active_carrier_count: u32,
    /// Highest-ranked nearby carrier, if any.
    pub best_carrier_peer: Option<String>,
    pub basis: ExpectedDeliveryBasis,
}

impl ExpectedDelivery {
    pub fn no_dest_fallback() -> Self {
        Self {
            eta_min_ms: DEFAULT_EXPECTED_ETA_MIN_MS,
            eta_max_ms: DEFAULT_EXPECTED_ETA_MAX_MS,
            confidence: 0.05,
            active_carrier_count: 0,
            best_carrier_peer: None,
            basis: ExpectedDeliveryBasis::NoCarriersNearby,
        }
    }
}

/// Pending custody message with structured delivery estimate (for UI / FFI).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustodyPendingStatus {
    pub message_id: String,
    pub dest_peer: Option<String>,
    /// Plaintext body from the stored custody message (for UI correlation with chat).
    pub chat_body: String,
    pub app_chat_id: String,
    pub enqueued_at_ms: u64,
    pub sprays_released: u8,
    pub max_spray: u8,
    pub hop_count: u8,
    pub expected: ExpectedDelivery,
}

#[derive(Debug, Clone)]
pub struct CustodyForwardPlan {
    pub message_id: String,
    pub to_peer: String,
    pub message: DirectMessage,
    pub wire: Option<WireMessage>,
}

#[derive(Debug)]
pub struct CustodyQueue {
    db: sled::Db,
    config: CustodyConfig,
}

impl CustodyQueue {
    pub fn open(base_path: &str, config: CustodyConfig) -> anyhow::Result<Self> {
        let path = Path::new(base_path).join("custody");
        std::fs::create_dir_all(&path)?;
        let db = sled::Config::new()
            .path(path)
            .flush_every_ms(config.flush_every_ms)
            .open()?;
        Ok(Self { db, config })
    }

    pub fn config(&self) -> &CustodyConfig {
        &self.config
    }

    pub fn len(&self) -> anyhow::Result<usize> {
        Ok(self.tree()?.len())
    }

    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.tree()?.is_empty())
    }

    pub fn enqueue(&self, message: &DirectMessage) -> anyhow::Result<bool> {
        self.enqueue_with_meta(message, None, None)
    }

    pub fn enqueue_with_meta(
        &self,
        message: &DirectMessage,
        app_chat_id: Option<String>,
        encrypted_wire: Option<WireMessage>,
    ) -> anyhow::Result<bool> {
        if !Self::eligible_for_custody(message) {
            return Ok(false);
        }
        let tree = self.tree()?;
        let id = message.envelope.id.0.clone();
        if tree.contains_key(id.as_bytes())? {
            return Ok(false);
        }
        if tree.len() >= self.config.max_queue_entries {
            anyhow::bail!("custody queue full");
        }
        let stored = StoredCustody {
            message: message.clone(),
            targets_tried: HashSet::new(),
            enqueued_at_ms: now_ms(),
            sprays_released: 0,
            app_chat_id,
            encrypted_wire,
        };
        tree.insert(id.as_bytes(), bincode::serialize(&stored)?)?;
        Ok(true)
    }

    pub fn remove(&self, message_id: &str) -> anyhow::Result<()> {
        let _ = self.tree()?.remove(message_id.as_bytes())?;
        Ok(())
    }

    pub fn prune_expired(&self, now: u64) -> anyhow::Result<usize> {
        let mut removed = 0usize;
        for item in self.tree()?.iter() {
            let (k, v) = item?;
            let stored: StoredCustody = bincode::deserialize(&v)?;
            if !Self::relay_allowed(&stored, now) {
                self.tree()?.remove(k)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn next_forward_plan(
        &self,
        local_peer: &str,
        connected_peers: &[String],
        encounter_log: &EncounterLogStore,
    ) -> anyhow::Result<Option<CustodyForwardPlan>> {
        self.next_forward_plan_at(local_peer, connected_peers, encounter_log, now_ms(), None)
    }

    /// Plan at most one custody forward per tick (spray-and-wait pacing).
    pub fn next_forward_plan_at(
        &self,
        local_peer: &str,
        connected_peers: &[String],
        encounter_log: &EncounterLogStore,
        now: u64,
        routing: Option<&CustodyRoutingContext>,
    ) -> anyhow::Result<Option<CustodyForwardPlan>> {
        let routing_ctx = match routing {
            Some(ctx) => ctx.clone(),
            None => CustodyRoutingContext::from_encounters(connected_peers, encounter_log, now)?,
        };
        let mut entries: Vec<(Vec<u8>, StoredCustody)> = Vec::new();
        for item in self.tree()?.iter() {
            let (k, v) = item?;
            entries.push((k.to_vec(), bincode::deserialize(&v)?));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, mut stored) in entries {
            if !Self::relay_allowed(&stored, now) {
                continue;
            }
            let mode = Self::forward_mode(&stored, local_peer);
            if matches!(mode, ForwardMode::OriginSpray) && !Self::origin_spray_budget(&stored) {
                continue;
            }
            let tried: Vec<String> = stored.targets_tried.iter().cloned().collect();
            let dest = stored.message.envelope.to_peer.as_deref();
            let candidates = encounter_log.custody_candidates_at(&CustodyCandidateQuery {
                connected: connected_peers,
                exclude: local_peer,
                tried: &tried,
                limit: 8,
                now,
                dest_peer: dest,
                scoring: routing_ctx.candidate_scoring,
            })?;
            let peer = Self::pick_custody_carrier(
                &candidates,
                local_peer,
                &stored,
                encounter_log,
                &routing_ctx,
                now,
            );
            let Some(peer) = peer else {
                continue;
            };
            stored.targets_tried.insert(peer.clone());
            let outbound = Self::prepare_outbound(&mut stored, local_peer);
            if matches!(mode, ForwardMode::OriginSpray) {
                self.tree()?.insert(k, bincode::serialize(&stored)?)?;
            } else {
                self.tree()?.remove(k)?;
            }
            return Ok(Some(CustodyForwardPlan {
                message_id: stored.message.envelope.id.0.clone(),
                to_peer: peer,
                message: outbound,
                wire: stored.encrypted_wire.clone(),
            }));
        }
        Ok(None)
    }

    pub fn plans_for_encounter(
        &self,
        local_peer: &str,
        encounter_peer: &str,
        max: usize,
    ) -> anyhow::Result<Vec<CustodyForwardPlan>> {
        self.plans_for_encounter_at(local_peer, encounter_peer, max, now_ms())
    }

    pub fn plans_for_encounter_at(
        &self,
        local_peer: &str,
        encounter_peer: &str,
        max: usize,
        now: u64,
    ) -> anyhow::Result<Vec<CustodyForwardPlan>> {
        let mut out = Vec::new();
        let mut entries: Vec<(Vec<u8>, StoredCustody)> = Vec::new();
        for item in self.tree()?.iter() {
            let (k, v) = item?;
            entries.push((k.to_vec(), bincode::deserialize(&v)?));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, mut stored) in entries {
            if out.len() >= max {
                break;
            }
            if !Self::relay_allowed(&stored, now) {
                continue;
            }
            let mode = Self::forward_mode(&stored, local_peer);
            if matches!(mode, ForwardMode::OriginSpray) && !Self::origin_spray_budget(&stored) {
                continue;
            }
            if stored.targets_tried.contains(encounter_peer) {
                continue;
            }
            if !Self::peer_allowed(&stored, encounter_peer, local_peer) {
                continue;
            }
            stored.targets_tried.insert(encounter_peer.to_string());
            let outbound = Self::prepare_outbound(&mut stored, local_peer);
            if matches!(mode, ForwardMode::OriginSpray) {
                self.tree()?.insert(k, bincode::serialize(&stored)?)?;
            } else {
                self.tree()?.remove(k)?;
            }
            out.push(CustodyForwardPlan {
                message_id: stored.message.envelope.id.0.clone(),
                to_peer: encounter_peer.to_string(),
                message: outbound,
                wire: stored.encrypted_wire.clone(),
            });
        }
        Ok(out)
    }

    pub fn eligible_for_custody(message: &DirectMessage) -> bool {
        message.envelope.custody || message.envelope.to_peer.is_some()
    }

    /// Network copy budget for origin spray phase (not per-hop relay).
    pub fn spray_budget_remaining(message: &DirectMessage) -> bool {
        message.envelope.custody_spray_sent < message.envelope.max_custody_spray
    }

    pub fn mark_delivered_to_local(&self, message_id: &str) -> anyhow::Result<()> {
        self.remove(message_id)
    }

    /// List pending custody entries with expected delivery windows and nearby carrier counts.
    pub fn list_pending_statuses(
        &self,
        local_peer: &str,
        connected_peers: &[String],
        encounter_log: &EncounterLogStore,
        now: u64,
    ) -> anyhow::Result<Vec<CustodyPendingStatus>> {
        let mut out = Vec::new();
        let mut entries: Vec<StoredCustody> = Vec::new();
        for item in self.tree()?.iter() {
            let (_, v) = item?;
            entries.push(bincode::deserialize(&v)?);
        }
        entries.sort_by_key(|a| a.enqueued_at_ms);
        for stored in entries {
            if !Self::relay_allowed(&stored, now) {
                continue;
            }
            let expected = compute_expected_delivery(
                encounter_log,
                &stored,
                local_peer,
                connected_peers,
                now,
            )?;
            out.push(CustodyPendingStatus {
                message_id: stored.message.envelope.id.0.clone(),
                dest_peer: stored.message.envelope.to_peer.clone(),
                chat_body: stored.message.body.clone(),
                app_chat_id: stored.app_chat_id.clone().unwrap_or_default(),
                enqueued_at_ms: stored.enqueued_at_ms,
                sprays_released: stored.sprays_released,
                max_spray: stored.message.envelope.max_custody_spray,
                hop_count: stored.message.envelope.hop_count,
                expected,
            });
        }
        Ok(out)
    }

    fn forward_mode(stored: &StoredCustody, local_peer: &str) -> ForwardMode {
        if Self::is_origin(stored, local_peer) && Self::origin_spray_budget(stored) {
            ForwardMode::OriginSpray
        } else {
            ForwardMode::Transfer
        }
    }

    fn is_origin(stored: &StoredCustody, local_peer: &str) -> bool {
        stored.message.envelope.from_peer == local_peer
    }

    fn origin_spray_budget(stored: &StoredCustody) -> bool {
        stored.sprays_released < stored.message.envelope.max_custody_spray
    }

    fn relay_allowed(stored: &StoredCustody, now: u64) -> bool {
        !stored.message.is_expired(now)
            && stored.message.envelope.hop_count < stored.message.envelope.max_hops
    }

    fn pick_custody_carrier(
        candidates: &[String],
        local_peer: &str,
        stored: &StoredCustody,
        encounter_log: &EncounterLogStore,
        routing: &CustodyRoutingContext,
        now: u64,
    ) -> Option<String> {
        let allowed: Vec<String> = candidates
            .iter()
            .filter(|p| Self::peer_allowed(stored, p, local_peer))
            .cloned()
            .collect();
        if allowed.is_empty() {
            return None;
        }

        // Cold DTN start (no trust map): stable peer order (legacy spray behaviour).
        // Production nodes with co-encounter history use DeliveryProbabilityAndRecency instead.
        if routing.peer_trust.is_empty()
            && routing.candidate_scoring == CustodyCandidateScoring::RecencyOnly
        {
            let mut picks = allowed;
            picks.sort();
            return picks.into_iter().next();
        }

        let mut meta_map = routing.routing_meta.clone();
        for p in &allowed {
            if meta_map.contains_key(p) {
                continue;
            }
            let trust = routing
                .peer_trust
                .get(p)
                .cloned()
                .unwrap_or_else(|| PeerTrust::cold_unknown(now));
            let enc = encounter_log.record(p).ok().flatten();
            meta_map.insert(
                p.clone(),
                peer_routing_meta(&trust, enc.map(|r| r.first_seen_ms), None),
            );
        }

        let mut scored: Vec<(String, f64)> = allowed
            .iter()
            .map(|p| {
                let trust = routing
                    .peer_trust
                    .get(p)
                    .cloned()
                    .unwrap_or_else(|| PeerTrust::cold_unknown(now));
                let mut score = trust.relay_selection_score(now);
                if let Ok(Some(rec)) = encounter_log.record(p) {
                    let recency = now.saturating_sub(rec.last_seen_ms);
                    score +=
                        0.15 * (1.0 - recency as f64 / ENCOUNTER_LOG_TTL_MS as f64).clamp(0.0, 1.0);
                }
                if routing.candidate_scoring
                    == CustodyCandidateScoring::DeliveryProbabilityAndRecency
                {
                    if let Some(dest) = stored.message.envelope.to_peer.as_deref() {
                        if let Ok(est) = encounter_log.delivery_probability(p, dest, now) {
                            score += 0.35 * est.combined;
                        }
                    }
                }
                (p.clone(), score)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let dest = stored.message.envelope.to_peer.as_deref();
        let diversity = routing.diversity_for_dest(dest);
        let picks =
            apply_peer_diversity(&scored, &routing.peer_trust, &meta_map, now, 1, &diversity);
        picks.into_iter().next()
    }

    fn peer_allowed(stored: &StoredCustody, peer: &str, local_peer: &str) -> bool {
        if peer == local_peer {
            return false;
        }
        if peer == stored.message.envelope.from_peer {
            return false;
        }
        if stored
            .message
            .envelope
            .last_custody_from
            .as_deref()
            .is_some_and(|prev| prev == peer)
        {
            return false;
        }
        true
    }

    /// Origin spray: duplicate copy. Otherwise: hand off local custody (transfer).
    fn prepare_outbound(stored: &mut StoredCustody, local_peer: &str) -> DirectMessage {
        let mut outbound = stored.message.clone();
        outbound.envelope.custody = true;
        outbound.envelope.hop_count = outbound.envelope.hop_count.saturating_add(1);
        outbound.envelope.last_custody_from = Some(local_peer.to_string());
        if Self::is_origin(stored, local_peer) && Self::origin_spray_budget(stored) {
            stored.sprays_released = stored.sprays_released.saturating_add(1);
            outbound.envelope.custody_spray_sent = outbound.envelope.max_custody_spray;
        }
        outbound
    }

    fn tree(&self) -> anyhow::Result<sled::Tree> {
        Ok(self.db.open_tree("custody_queue")?)
    }
}

enum ForwardMode {
    OriginSpray,
    Transfer,
}

/// Estimate delivery window and nearby carrier count for one pending custody message.
fn compute_expected_delivery(
    encounter_log: &EncounterLogStore,
    stored: &StoredCustody,
    local_peer: &str,
    connected: &[String],
    now: u64,
) -> anyhow::Result<ExpectedDelivery> {
    let Some(dest_peer) = stored
        .message
        .envelope
        .to_peer
        .as_deref()
        .filter(|d| !d.is_empty())
    else {
        return Ok(ExpectedDelivery::no_dest_fallback());
    };

    let candidates = encounter_log.custody_candidates_at(&CustodyCandidateQuery {
        connected,
        exclude: local_peer,
        tried: &[],
        limit: 32,
        now,
        dest_peer: Some(dest_peer),
        scoring: CustodyCandidateScoring::DeliveryProbabilityAndRecency,
    })?;
    let viable: Vec<String> = candidates
        .into_iter()
        .filter(|p| CustodyQueue::peer_allowed(stored, p, local_peer))
        .collect();
    let active_carrier_count = viable.len() as u32;

    if viable.is_empty() {
        return Ok(ExpectedDelivery {
            eta_min_ms: DEFAULT_EXPECTED_ETA_MIN_MS,
            eta_max_ms: DEFAULT_EXPECTED_ETA_MAX_MS,
            confidence: 0.05,
            active_carrier_count: 0,
            best_carrier_peer: None,
            basis: ExpectedDeliveryBasis::NoCarriersNearby,
        });
    }

    let best_carrier_peer = viable.first().cloned();
    let mut min_eta = u64::MAX;
    let mut max_eta = 0u64;
    let mut confidence = 0.0f64;
    let mut basis = ExpectedDeliveryBasis::EncounterFrequency;

    for peer in viable.iter().take(3) {
        let window = encounter_log.estimate_encounter_window(peer, dest_peer, now)?;
        min_eta = min_eta.min(window.eta_min_ms);
        max_eta = max_eta.max(window.eta_max_ms);
        if window.confidence > confidence {
            confidence = window.confidence;
            basis = window.basis;
        }
    }

    if min_eta == u64::MAX {
        min_eta = DEFAULT_EXPECTED_ETA_MIN_MS;
    }
    if max_eta < min_eta {
        max_eta = min_eta.saturating_add(MIN_EXPECTED_ETA_FLOOR);
    }

    Ok(ExpectedDelivery {
        eta_min_ms: min_eta,
        eta_max_ms: max_eta.min(ENCOUNTER_LOG_TTL_MS),
        confidence: confidence.clamp(0.05, 1.0),
        active_carrier_count,
        best_carrier_peer,
        basis,
    })
}

const MIN_EXPECTED_ETA_FLOOR: u64 = 60_000;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_core::data::Envelope;
    use tempfile::tempdir;

    fn custody_msg(from: &str, to: &str) -> DirectMessage {
        let mut env = Envelope::new(from.to_string(), Some(to.to_string()), b"dtm".to_vec());
        env.custody = true;
        env.max_custody_spray = 3;
        DirectMessage {
            envelope: env,
            body: "dtm".into(),
        }
    }

    #[test]
    fn enqueue_respects_spray_budget() {
        let dir = tempdir().unwrap();
        let q = CustodyQueue::open(dir.path().to_str().unwrap(), CustodyConfig::default()).unwrap();
        let msg = custody_msg("a", "z");
        assert!(q.enqueue(&msg).unwrap());
        assert!(!q.enqueue(&msg).unwrap());
    }

    #[test]
    fn forward_plan_respects_tried_peers() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let q = CustodyQueue::open(dir.path().to_str().unwrap(), CustodyConfig::default()).unwrap();
        log.begin_encounter("b", 1).unwrap();
        log.end_encounter("b", 2).unwrap();
        let msg = custody_msg("a", "z");
        q.enqueue(&msg).unwrap();
        let plan = q
            .next_forward_plan("a", &["b".into()], &log)
            .unwrap()
            .expect("plan");
        assert_eq!(plan.to_peer, "b");
        assert_eq!(plan.message.envelope.custody_spray_sent, 3);
        assert_eq!(plan.message.envelope.hop_count, 1);
        assert!(q
            .next_forward_plan("a", &["b".into()], &log)
            .unwrap()
            .is_none());
    }

    #[test]
    fn carrier_relay_uses_hop_budget_not_spray_budget() {
        let dir = tempdir().unwrap();
        let q = CustodyQueue::open(dir.path().to_str().unwrap(), CustodyConfig::default()).unwrap();
        let mut msg = custody_msg("a", "z");
        msg.envelope.custody_spray_sent = 3;
        q.enqueue(&msg).unwrap();

        let plans = q.plans_for_encounter("b", "c", 1).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].message.envelope.hop_count, 1);
        assert_eq!(plans[0].message.envelope.custody_spray_sent, 3);
        assert_eq!(
            q.len().unwrap(),
            0,
            "carrier relinquishes custody on handoff"
        );
    }

    #[test]
    fn origin_relays_after_spray_budget_exhausted() {
        let dir = tempdir().unwrap();
        let q = CustodyQueue::open(dir.path().to_str().unwrap(), CustodyConfig::default()).unwrap();
        let mut msg = custody_msg("a", "z");
        msg.envelope.max_custody_spray = 1;
        q.enqueue(&msg).unwrap();
        let spray = q.plans_for_encounter("a", "b", 1).unwrap();
        assert_eq!(spray.len(), 1);
        assert_eq!(q.len().unwrap(), 1, "origin keeps copy after spray");
        let relay = q.plans_for_encounter("a", "c", 1).unwrap();
        assert_eq!(relay.len(), 1);
        assert_eq!(relay[0].to_peer, "c");
        assert_eq!(q.len().unwrap(), 0, "origin transfers after spray phase");
    }

    #[test]
    fn skips_immediate_custody_ping_pong() {
        let dir = tempdir().unwrap();
        let q = CustodyQueue::open(dir.path().to_str().unwrap(), CustodyConfig::default()).unwrap();
        let mut msg = custody_msg("a", "z");
        msg.envelope.last_custody_from = Some("c".into());
        q.enqueue(&msg).unwrap();
        assert!(q.plans_for_encounter("b", "c", 1).unwrap().is_empty());
        let plan = q.plans_for_encounter("b", "d", 1).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].to_peer, "d");
    }

    #[test]
    fn pending_status_includes_expected_delivery_and_carriers() {
        let dir = tempdir().unwrap();
        let log = EncounterLogStore::open(dir.path().to_str().unwrap()).unwrap();
        let q = CustodyQueue::open(dir.path().to_str().unwrap(), CustodyConfig::default()).unwrap();
        let day = 24 * 60 * 60_000;
        let base = 1_700_000_000_000u64;
        for d in 0..3 {
            let start = base + d * day + 8 * 3_600_000;
            log.begin_co_encounter("carrier", "z", start).unwrap();
            log.end_co_encounter("carrier", "z", start + 15 * 60_000)
                .unwrap();
        }
        log.begin_encounter("carrier", base + 3 * day).unwrap();
        log.end_encounter("carrier", base + 3 * day + 60_000)
            .unwrap();
        log.begin_encounter("other", base + 3 * day + 120_000)
            .unwrap();
        log.end_encounter("other", base + 3 * day + 180_000)
            .unwrap();

        let msg = custody_msg("a", "z");
        q.enqueue(&msg).unwrap();
        let now = base + 3 * day + 200_000;
        let statuses = q
            .list_pending_statuses("a", &["carrier".into(), "other".into()], &log, now)
            .unwrap();
        assert_eq!(statuses.len(), 1);
        let st = &statuses[0];
        assert_eq!(st.message_id, msg.envelope.id.0);
        assert_eq!(st.dest_peer.as_deref(), Some("z"));
        assert!(st.expected.active_carrier_count >= 1);
        assert!(st.expected.eta_max_ms >= st.expected.eta_min_ms);
        assert!(st.expected.confidence > 0.0);
    }
}
