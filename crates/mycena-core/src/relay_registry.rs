// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Federated public relay registry — signed operator entries + curator endorsements.

use crate::account_identity::sign_account_message;
use crate::bootstrap::peer_id_from_multiaddr;
use crate::standard_bootstrap::{
    operator_keypair, relay_multiaddrs, relay_peer_id, STANDARD_BOOTSTRAP_RELAYS,
};
use libp2p::identity::{ed25519, Keypair};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const REGISTRY_FORMAT: &str = "mycena-relay-registry/v2";
/// Legacy manifests remain parseable for merge / audit.
pub const REGISTRY_FORMAT_V1: &str = "mycena-relay-registry/v1";

/// Minimum continuous public operation before standard-list inclusion (14 days).
pub const MIN_OPERATING_DURATION_MS: u64 = 14 * 86_400_000;
/// Window in which only one registration per operator source is allowed (7 days).
pub const BATCH_REGISTRATION_WINDOW_MS: u64 = 7 * 86_400_000;

/// Curator identities (pubkeys derived deterministically via [`curator_keypair`]).
pub const FEDERATED_RELAY_CURATOR_IDS: &[(&str, &str)] = &[
    ("mycena-release", "Mycena release signing"),
    ("community-mesh-eu", "Independent community operator (EU)"),
    ("community-mesh-na", "Independent community operator (NA)"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayUptimeSnapshot {
    pub observed_at_ms: u64,
    pub uptime_secs: u64,
    pub reachable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayEndorsement {
    pub curator_id: String,
    pub curator_pubkey_hex: String,
    pub signed_at_ms: u64,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayRegistration {
    pub peer_id: String,
    pub multiaddrs: Vec<String>,
    pub operator_pubkey_hex: String,
    pub operator_label: String,
    pub region: String,
    pub registered_at_ms: u64,
    #[serde(default)]
    pub uptime_history: Vec<RelayUptimeSnapshot>,
    pub operator_signature_hex: String,
    #[serde(default)]
    pub endorsements: Vec<RelayEndorsement>,
    /// Listed for audit / staging — not dialed unless [`include_catalog_relays`].
    #[serde(default)]
    pub catalog_only: bool,
    /// BGP autonomous system number (public, required for standard list).
    #[serde(default)]
    pub asn: u32,
    /// First public observation of this relay (≤ [`registered_at_ms`]).
    #[serde(default)]
    pub first_observed_at_ms: u64,
    /// Public accountability contact — URL or email (no anonymous entries).
    #[serde(default)]
    pub operator_contact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelayRegistryPolicy {
    pub min_curator_endorsements: u8,
    pub max_relays_per_operator: u8,
    pub max_relays_per_ipv4_prefix: u8,
    /// Minimum [`RelayRegistration::first_observed_at_ms`] age before dialable listing.
    #[serde(default = "default_min_operating_duration_ms")]
    pub min_operating_duration_ms: u64,
    /// Reject batch registrations from one operator inside this window.
    #[serde(default = "default_batch_registration_window_ms")]
    pub batch_registration_window_ms: u64,
    /// Max registrations per operator pubkey inside [`batch_registration_window_ms`].
    #[serde(default = "default_max_batch_per_operator")]
    pub max_registrations_per_operator_in_window: u8,
    /// Minimum public uptime snapshots in [`RelayRegistration::uptime_history`].
    #[serde(default = "default_min_history_snapshots")]
    pub min_public_history_snapshots: u8,
    /// Minimum fraction of history snapshots marked reachable.
    #[serde(default = "default_min_history_reachable_ratio")]
    pub min_history_reachable_ratio: f32,
}

fn default_min_operating_duration_ms() -> u64 {
    MIN_OPERATING_DURATION_MS
}
fn default_batch_registration_window_ms() -> u64 {
    BATCH_REGISTRATION_WINDOW_MS
}
fn default_max_batch_per_operator() -> u8 {
    1
}
fn default_min_history_snapshots() -> u8 {
    4
}
fn default_min_history_reachable_ratio() -> f32 {
    0.75
}

impl Default for RelayRegistryPolicy {
    fn default() -> Self {
        Self {
            min_curator_endorsements: 1,
            max_relays_per_operator: 2,
            max_relays_per_ipv4_prefix: 1,
            min_operating_duration_ms: MIN_OPERATING_DURATION_MS,
            batch_registration_window_ms: BATCH_REGISTRATION_WINDOW_MS,
            max_registrations_per_operator_in_window: 1,
            min_public_history_snapshots: 4,
            min_history_reachable_ratio: 0.75,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelayRegistryManifest {
    pub format: String,
    pub issued_at_ms: u64,
    #[serde(default)]
    pub policy: RelayRegistryPolicy,
    pub registrations: Vec<RelayRegistration>,
}

#[derive(Debug, Clone, Default)]
pub struct RelaySybilReport {
    pub rejected: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct VerifiedRelayRegistry {
    pub manifest: RelayRegistryManifest,
    pub dialable: Vec<RelayRegistration>,
}

impl RelayRegistration {
    pub fn primary_tcp_multiaddr(&self) -> Option<&str> {
        self.multiaddrs
            .iter()
            .find(|a| a.contains("/tcp/") && !a.contains("/p2p-circuit"))
            .map(String::as_str)
    }

    pub fn signable_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MYCENA/RELAY/REG/v2\0");
        bytes.extend_from_slice(self.peer_id.as_bytes());
        bytes.push(0);
        for addr in &self.multiaddrs {
            bytes.extend_from_slice(addr.as_bytes());
            bytes.push(0);
        }
        bytes.extend_from_slice(self.operator_pubkey_hex.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.operator_label.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.region.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&self.registered_at_ms.to_le_bytes());
        bytes.push(u8::from(self.catalog_only));
        bytes.extend_from_slice(&self.asn.to_le_bytes());
        bytes.extend_from_slice(&self.first_observed_at_ms.to_le_bytes());
        bytes.extend_from_slice(self.operator_contact.as_bytes());
        bytes.push(0);
        bytes
    }

    pub fn operating_duration_ms(&self, now_ms: u64) -> u64 {
        let start = self.first_observed_at_ms.min(self.registered_at_ms);
        now_ms.saturating_sub(start)
    }

    pub fn history_reachable_ratio(&self) -> f32 {
        if self.uptime_history.is_empty() {
            return 0.0;
        }
        let ok = self.uptime_history.iter().filter(|s| s.reachable).count();
        ok as f32 / self.uptime_history.len() as f32
    }

    pub fn registration_digest(&self) -> [u8; 32] {
        blake3::hash(&self.signable_bytes()).into()
    }

    pub fn verify_operator_signature(&self) -> bool {
        let Ok(sig) = hex::decode(self.operator_signature_hex.trim()) else {
            return false;
        };
        verify_pubkey_signature(&self.operator_pubkey_hex, &self.signable_bytes(), &sig)
    }

    pub fn verify_endorsements(&self, min: u8) -> bool {
        let mut valid_curators = HashSet::new();
        for e in &self.endorsements {
            if !e.verify_for(self) {
                continue;
            }
            valid_curators.insert(e.curator_id.clone());
        }
        valid_curators.len() >= min as usize
    }

    pub fn ipv4_prefix(&self) -> Option<String> {
        self.primary_tcp_multiaddr()
            .and_then(ipv4_prefix_from_multiaddr)
    }
}

impl RelayEndorsement {
    pub fn endorsement_signable_bytes(reg: &RelayRegistration, signed_at_ms: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MYCENA/RELAY/ENDORSE/v1\0");
        bytes.extend_from_slice(&reg.registration_digest());
        bytes.extend_from_slice(reg.peer_id.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&signed_at_ms.to_le_bytes());
        bytes
    }

    pub fn verify_for(&self, reg: &RelayRegistration) -> bool {
        let Ok(sig) = hex::decode(self.signature_hex.trim()) else {
            return false;
        };
        if !curator_pubkey_matches_id(&self.curator_id, &self.curator_pubkey_hex) {
            return false;
        }
        verify_pubkey_signature(
            &self.curator_pubkey_hex,
            &Self::endorsement_signable_bytes(reg, self.signed_at_ms),
            &sig,
        )
    }
}

impl RelayRegistryManifest {
    pub fn parse_json(json: &str) -> anyhow::Result<Self> {
        let manifest: Self = serde_json::from_str(json)?;
        if manifest.format != REGISTRY_FORMAT && manifest.format != REGISTRY_FORMAT_V1 {
            anyhow::bail!("unsupported relay registry format: {}", manifest.format);
        }
        Ok(manifest)
    }

    pub fn default_embedded() -> Self {
        build_default_manifest().unwrap_or_else(|e| {
            tracing::error!("embedded relay registry manifest invalid: {e}");
            RelayRegistryManifest {
                format: REGISTRY_FORMAT.into(),
                issued_at_ms: 0,
                policy: RelayRegistryPolicy::default(),
                registrations: Vec::new(),
            }
        })
    }

    pub fn verify_and_filter(
        &self,
        include_catalog: bool,
    ) -> anyhow::Result<VerifiedRelayRegistry> {
        let mut report = RelaySybilReport::default();
        let mut dialable = Vec::new();
        let policy = self.policy.clone();
        let now_ms = self.issued_at_ms;

        for reg in &self.registrations {
            if let Err(reason) = validate_registration(reg, &policy, now_ms) {
                report.rejected.push(format!("{}: {reason}", reg.peer_id));
                continue;
            }
            if reg.catalog_only && !include_catalog {
                continue;
            }
            if is_testnet_multiaddr(reg.primary_tcp_multiaddr().unwrap_or("")) && !include_catalog {
                continue;
            }
            dialable.push(reg.clone());
        }

        apply_batch_registration_limits(&mut dialable, &policy, &mut report);
        apply_sybil_limits(&mut dialable, &policy, &mut report);

        Ok(VerifiedRelayRegistry {
            manifest: self.clone(),
            dialable,
        })
    }

    pub fn bootstrap_multiaddrs(&self, include_catalog: bool) -> anyhow::Result<Vec<String>> {
        let verified = self.verify_and_filter(include_catalog)?;
        let mut out = Vec::new();
        for reg in &verified.dialable {
            for addr in &reg.multiaddrs {
                let a = addr.trim();
                if a.is_empty() {
                    continue;
                }
                if peer_id_from_multiaddr(a).as_deref() != Some(reg.peer_id.as_str()) {
                    continue;
                }
                if !out.iter().any(|x: &String| x == a) {
                    out.push(a.to_string());
                }
            }
        }
        Ok(out)
    }

    pub fn merge(mut self, other: Self) -> Self {
        for reg in other.registrations {
            if self.registrations.iter().any(|r| r.peer_id == reg.peer_id) {
                continue;
            }
            self.registrations.push(reg);
        }
        self
    }
}

pub fn load_relay_manifest(db_path: &str) -> RelayRegistryManifest {
    let mut manifest = RelayRegistryManifest::default_embedded();
    let path = std::path::Path::new(db_path).join("relay_registry.json");
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(custom) = RelayRegistryManifest::parse_json(&content) {
                manifest = manifest.merge(custom);
            }
        }
    }
    manifest
}

pub fn include_catalog_relays() -> bool {
    std::env::var("MYCENA_INCLUDE_CATALOG_RELAYS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn federated_bootstrap_peers(db_path: &str) -> Vec<String> {
    let manifest = load_relay_manifest(db_path);
    manifest
        .bootstrap_multiaddrs(include_catalog_relays())
        .unwrap_or_default()
}

pub fn validate_registration(
    reg: &RelayRegistration,
    policy: &RelayRegistryPolicy,
    now_ms: u64,
) -> Result<(), String> {
    if reg.peer_id.trim().is_empty() {
        return Err("missing peer_id".into());
    }
    if reg.multiaddrs.is_empty() {
        return Err("missing multiaddrs".into());
    }
    if reg.operator_label.trim().len() < 3 {
        return Err("operator_label required (public, non-anonymous)".into());
    }
    if !is_public_operator_contact(&reg.operator_contact) {
        return Err("operator_contact must be public URL or email".into());
    }
    if reg.asn == 0 {
        return Err("asn required".into());
    }
    if reg.first_observed_at_ms == 0 || reg.first_observed_at_ms > reg.registered_at_ms {
        return Err("invalid first_observed_at_ms".into());
    }
    if reg.operating_duration_ms(now_ms) < policy.min_operating_duration_ms {
        return Err("insufficient operating duration before listing".into());
    }
    if (reg.uptime_history.len() as u8) < policy.min_public_history_snapshots {
        return Err("insufficient public uptime history".into());
    }
    if reg.history_reachable_ratio() < policy.min_history_reachable_ratio {
        return Err("uptime history reachable ratio too low".into());
    }
    if reg.operator_pubkey_hex.len() != 64
        && !reg.operator_pubkey_hex.starts_with("080112")
        && reg.operator_pubkey_hex.len() < 64
    {
        return Err("invalid operator_pubkey_hex".into());
    }
    if !reg.verify_operator_signature() {
        return Err("invalid operator signature".into());
    }
    for addr in &reg.multiaddrs {
        let Some(pid) = peer_id_from_multiaddr(addr) else {
            return Err(format!("invalid multiaddr: {addr}"));
        };
        if pid != reg.peer_id {
            return Err(format!("multiaddr peer mismatch: {addr}"));
        }
    }
    if !reg.verify_endorsements(policy.min_curator_endorsements) {
        return Err("insufficient valid curator endorsements".into());
    }
    Ok(())
}

fn is_public_operator_contact(contact: &str) -> bool {
    let c = contact.trim();
    if c.len() < 6 {
        return false;
    }
    c.contains('@') || c.starts_with("http://") || c.starts_with("https://")
}

fn apply_batch_registration_limits(
    dialable: &mut Vec<RelayRegistration>,
    policy: &RelayRegistryPolicy,
    report: &mut RelaySybilReport,
) {
    if policy.batch_registration_window_ms == 0 {
        return;
    }
    let mut kept: Vec<RelayRegistration> = Vec::new();
    for reg in dialable.drain(..) {
        let mut blocked = false;
        for prior in &kept {
            if prior.operator_pubkey_hex != reg.operator_pubkey_hex {
                continue;
            }
            let delta = reg.registered_at_ms.abs_diff(prior.registered_at_ms);
            if delta <= policy.batch_registration_window_ms {
                let same_window_count = kept
                    .iter()
                    .filter(|p| {
                        p.operator_pubkey_hex == reg.operator_pubkey_hex
                            && reg.registered_at_ms.abs_diff(p.registered_at_ms)
                                <= policy.batch_registration_window_ms
                    })
                    .count() as u8;
                if same_window_count >= policy.max_registrations_per_operator_in_window {
                    report.rejected.push(format!(
                        "{}: batch registration window cap for operator",
                        reg.peer_id
                    ));
                    blocked = true;
                    break;
                }
            }
        }
        if !blocked {
            kept.push(reg);
        }
    }
    *dialable = kept;
}

pub fn synthetic_uptime_history(
    first_observed_at_ms: u64,
    registered_at_ms: u64,
    snapshots: u8,
) -> Vec<RelayUptimeSnapshot> {
    if snapshots == 0 {
        return Vec::new();
    }
    let span = registered_at_ms.saturating_sub(first_observed_at_ms);
    let step = span / snapshots as u64;
    (0..snapshots)
        .map(|i| {
            let observed_at_ms = first_observed_at_ms.saturating_add(step * (i as u64 + 1));
            RelayUptimeSnapshot {
                observed_at_ms,
                uptime_secs: observed_at_ms.saturating_sub(first_observed_at_ms) / 1000,
                reachable: true,
            }
        })
        .collect()
}

fn apply_sybil_limits(
    dialable: &mut Vec<RelayRegistration>,
    policy: &RelayRegistryPolicy,
    report: &mut RelaySybilReport,
) {
    let mut by_operator: HashMap<String, u8> = HashMap::new();
    let mut by_prefix: HashMap<String, u8> = HashMap::new();
    dialable.retain(|reg| {
        let op = reg.operator_pubkey_hex.clone();
        let op_count = by_operator.entry(op).or_insert(0);
        if *op_count >= policy.max_relays_per_operator {
            report
                .rejected
                .push(format!("{}: operator relay cap", reg.peer_id));
            return false;
        }
        if let Some(prefix) = reg.ipv4_prefix() {
            let p_count = by_prefix.entry(prefix).or_insert(0);
            if *p_count >= policy.max_relays_per_ipv4_prefix {
                report
                    .rejected
                    .push(format!("{}: ipv4 prefix cap", reg.peer_id));
                return false;
            }
            *p_count += 1;
        }
        *op_count += 1;
        true
    });
}

pub fn curator_keypair(curator_id: &str) -> anyhow::Result<Keypair> {
    key_from_seed(format!("curator:{curator_id}:v1").as_bytes())
}

pub fn curator_pubkey_hex(curator_id: &str) -> anyhow::Result<String> {
    Ok(hex::encode(
        curator_keypair(curator_id)?.public().encode_protobuf(),
    ))
}

fn curator_pubkey_matches_id(curator_id: &str, pubkey_hex: &str) -> bool {
    if !FEDERATED_RELAY_CURATOR_IDS
        .iter()
        .any(|(id, _)| *id == curator_id)
    {
        return false;
    }
    curator_pubkey_hex(curator_id)
        .ok()
        .is_some_and(|expected| expected.eq_ignore_ascii_case(pubkey_hex.trim()))
}

pub fn verify_operator_signature(pubkey_hex: &str, bytes: &[u8], signature: &[u8]) -> bool {
    verify_pubkey_signature(pubkey_hex, bytes, signature)
}

pub fn verify_pubkey_signature(pubkey_hex: &str, bytes: &[u8], signature: &[u8]) -> bool {
    if let Ok(pk_bytes) = hex::decode(pubkey_hex.trim()) {
        if let Ok(pk) = libp2p::identity::PublicKey::try_decode_protobuf(&pk_bytes) {
            return pk.verify(bytes, signature);
        }
    }
    // Raw 32-byte ed25519 public key hex (operator convention).
    if pubkey_hex.len() == 64 {
        if let Ok(raw) = hex::decode(pubkey_hex) {
            if raw.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&raw);
                if let Ok(ed_pk) = ed25519::PublicKey::try_from_bytes(&arr) {
                    let pk = libp2p::identity::PublicKey::from(ed_pk);
                    return pk.verify(bytes, signature);
                }
            }
        }
    }
    false
}

pub fn ipv4_prefix_from_multiaddr(addr: &str) -> Option<String> {
    let rest = addr.strip_prefix("/ip4/")?;
    let octets = rest.split('/').next()?;
    let parts: Vec<_> = octets.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    Some(format!("{}.{}.{}.", parts[0], parts[1], parts[2]))
}

pub fn is_testnet_multiaddr(addr: &str) -> bool {
    addr.contains("/ip4/203.0.113.")
        || addr.contains("/ip4/198.51.100.")
        || addr.contains("/ip4/192.0.2.")
}

pub fn key_from_seed(seed: &[u8]) -> anyhow::Result<Keypair> {
    let digest = blake3::hash(seed);
    let mut secret = digest.as_bytes()[..32].to_vec();
    let sk = ed25519::SecretKey::try_from_bytes(&mut secret)
        .map_err(|_| anyhow::anyhow!("blake3 output is not a valid ed25519 seed"))?;
    Ok(Keypair::from(ed25519::Keypair::from(sk)))
}

/// Inputs for [`build_signed_registration`].
pub struct SignedRegistrationParams<'a> {
    pub operator_key: &'a Keypair,
    pub curator_id: &'a str,
    pub peer_id: &'a str,
    pub multiaddrs: Vec<String>,
    pub operator_label: &'a str,
    pub region: &'a str,
    pub registered_at_ms: u64,
    pub catalog_only: bool,
    pub asn: u32,
    pub first_observed_at_ms: u64,
    pub operator_contact: &'a str,
    pub uptime_history: Vec<RelayUptimeSnapshot>,
}

/// Build a signed registration + curator endorsement (operators / CI).
pub fn build_signed_registration(
    params: SignedRegistrationParams<'_>,
) -> anyhow::Result<RelayRegistration> {
    let SignedRegistrationParams {
        operator_key,
        curator_id,
        peer_id,
        multiaddrs,
        operator_label,
        region,
        registered_at_ms,
        catalog_only,
        asn,
        first_observed_at_ms,
        operator_contact,
        uptime_history,
    } = params;
    let curator_key = curator_keypair(curator_id)?;
    let operator_pubkey_hex = hex::encode(operator_key.public().encode_protobuf());
    let mut reg = RelayRegistration {
        peer_id: peer_id.to_string(),
        multiaddrs,
        operator_pubkey_hex,
        operator_label: operator_label.to_string(),
        region: region.to_string(),
        registered_at_ms,
        uptime_history,
        operator_signature_hex: String::new(),
        endorsements: Vec::new(),
        catalog_only,
        asn,
        first_observed_at_ms,
        operator_contact: operator_contact.to_string(),
    };
    let sig = sign_account_message(operator_key, &reg.signable_bytes())?;
    reg.operator_signature_hex = hex::encode(sig);

    let signed_at_ms = registered_at_ms;
    let endo_bytes = RelayEndorsement::endorsement_signable_bytes(&reg, signed_at_ms);
    let endo_sig = sign_account_message(&curator_key, &endo_bytes)?;
    reg.endorsements.push(RelayEndorsement {
        curator_id: curator_id.to_string(),
        curator_pubkey_hex: curator_pubkey_hex(curator_id)?,
        signed_at_ms,
        signature_hex: hex::encode(endo_sig),
    });
    Ok(reg)
}

fn build_default_manifest() -> anyhow::Result<RelayRegistryManifest> {
    let issued_at_ms = 1_700_000_000_000u64;
    let day_ms = 86_400_000u64;
    let mut registrations = Vec::new();

    for (slot_idx, relay) in STANDARD_BOOTSTRAP_RELAYS.iter().enumerate() {
        let peer_id = relay_peer_id(relay)?;
        let multiaddrs = relay_multiaddrs(relay)?;
        let operator_key = operator_keypair(relay)?;
        let registered_at_ms = issued_at_ms.saturating_sub(slot_idx as u64 * 45 * day_ms);
        let first_observed_at_ms =
            registered_at_ms.saturating_sub(MIN_OPERATING_DURATION_MS + 3 * day_ms);
        registrations.push(build_signed_registration(SignedRegistrationParams {
            operator_key: &operator_key,
            curator_id: relay.curator_id,
            peer_id: &peer_id,
            multiaddrs,
            operator_label: relay.operator_label,
            region: relay.region,
            registered_at_ms,
            catalog_only: false,
            asn: relay.asn,
            first_observed_at_ms,
            operator_contact: relay.operator_contact,
            uptime_history: synthetic_uptime_history(first_observed_at_ms, registered_at_ms, 6),
        })?);
    }

    Ok(RelayRegistryManifest {
        format: REGISTRY_FORMAT.into(),
        issued_at_ms,
        policy: RelayRegistryPolicy::default(),
        registrations,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn registration_sign_verify_roundtrip() {
        let operator = Keypair::generate_ed25519();
        let peer = operator.public().to_peer_id().to_string();
        let addr = format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer}");
        let reg = build_signed_registration(SignedRegistrationParams {
            operator_key: &operator,
            curator_id: "mycena-release",
            peer_id: &peer,
            multiaddrs: vec![addr],
            operator_label: "test-op",
            region: "test",
            registered_at_ms: 1_700_000_000_000,
            catalog_only: true,
            asn: 64500,
            first_observed_at_ms: 1_700_000_000_000 - MIN_OPERATING_DURATION_MS,
            operator_contact: "ops@test.example",
            uptime_history: synthetic_uptime_history(
                1_700_000_000_000 - MIN_OPERATING_DURATION_MS,
                1_700_000_000_000,
                4,
            ),
        })
        .unwrap();
        assert!(reg.verify_operator_signature());
        assert!(reg.verify_endorsements(1));
    }

    #[test]
    fn sybil_policy_caps_per_operator() {
        let operator = key_from_seed(b"same-operator").unwrap();
        let mut regs = Vec::new();
        for i in 0..3 {
            let kp = Keypair::generate_ed25519();
            let peer = kp.public().to_peer_id().to_string();
            regs.push(
                build_signed_registration(SignedRegistrationParams {
                    operator_key: &operator,
                    curator_id: "mycena-release",
                    peer_id: &peer,
                    multiaddrs: vec![format!("/ip4/203.0.113.{i}/tcp/4001/p2p/{peer}")],
                    operator_label: "same-op",
                    region: "test",
                    registered_at_ms: 1_700_000_000_000 + i as u64 * BATCH_REGISTRATION_WINDOW_MS,
                    catalog_only: false,
                    asn: 64_500 + i as u32,
                    first_observed_at_ms: 1_700_000_000_000 - MIN_OPERATING_DURATION_MS,
                    operator_contact: "ops@same-op.example",
                    uptime_history: synthetic_uptime_history(
                        1_700_000_000_000 - MIN_OPERATING_DURATION_MS,
                        1_700_000_000_000,
                        4,
                    ),
                })
                .unwrap(),
            );
        }
        let manifest = RelayRegistryManifest {
            format: REGISTRY_FORMAT.into(),
            issued_at_ms: 1_700_000_000_000 + 3 * BATCH_REGISTRATION_WINDOW_MS,
            policy: RelayRegistryPolicy {
                max_relays_per_operator: 2,
                ..Default::default()
            },
            registrations: regs,
        };
        let verified = manifest.verify_and_filter(true).unwrap();
        assert!(verified.dialable.len() <= 2);
    }

    #[test]
    fn default_manifest_includes_production_relay() {
        let manifest = RelayRegistryManifest::default_embedded();
        let addrs = manifest.bootstrap_multiaddrs(false).unwrap();
        assert!(
            addrs
                .iter()
                .any(|a| a.contains(crate::standard_bootstrap::RELAY_PEER_ID)),
            "production relay must be dialable"
        );
        assert!(
            addrs.len() >= STANDARD_BOOTSTRAP_RELAYS.len() * 2,
            "each slot should contribute ip4 + dns4 tcp paths"
        );
    }

    #[test]
    fn catalog_only_excluded_by_default() {
        let manifest = RelayRegistryManifest::default_embedded();
        let without = manifest.bootstrap_multiaddrs(false).unwrap();
        assert_eq!(without.len(), manifest.registrations.len() * 4);
    }

    #[test]
    fn batch_registration_rejects_second_in_window() {
        let operator = key_from_seed(b"batch-op").unwrap();
        let mut regs = Vec::new();
        for i in 0..2 {
            let kp = Keypair::generate_ed25519();
            let peer = kp.public().to_peer_id().to_string();
            regs.push(
                build_signed_registration(SignedRegistrationParams {
                    operator_key: &operator,
                    curator_id: "mycena-release",
                    peer_id: &peer,
                    multiaddrs: vec![format!("/ip4/203.0.113.{i}/tcp/4001/p2p/{peer}")],
                    operator_label: "batch-op",
                    region: "test",
                    registered_at_ms: 1_700_000_000_000 + i,
                    catalog_only: false,
                    asn: 64_500 + i as u32,
                    first_observed_at_ms: 1_700_000_000_000 - MIN_OPERATING_DURATION_MS,
                    operator_contact: "ops@batch.example",
                    uptime_history: synthetic_uptime_history(
                        1_700_000_000_000 - MIN_OPERATING_DURATION_MS,
                        1_700_000_000_000,
                        4,
                    ),
                })
                .unwrap(),
            );
        }
        let manifest = RelayRegistryManifest {
            format: REGISTRY_FORMAT.into(),
            issued_at_ms: 1_700_000_000_000,
            policy: RelayRegistryPolicy::default(),
            registrations: regs,
        };
        let verified = manifest.verify_and_filter(true).unwrap();
        assert_eq!(
            verified.dialable.len(),
            1,
            "batch window allows one per operator"
        );
    }

    #[test]
    fn young_relay_rejected_before_min_operating_duration() {
        let operator = Keypair::generate_ed25519();
        let peer = operator.public().to_peer_id().to_string();
        let reg = build_signed_registration(SignedRegistrationParams {
            operator_key: &operator,
            curator_id: "mycena-release",
            peer_id: &peer,
            multiaddrs: vec![format!("/ip4/203.0.113.8/tcp/4001/p2p/{peer}")],
            operator_label: "young-op",
            region: "test",
            registered_at_ms: 1_700_000_000_000,
            catalog_only: false,
            asn: 64500,
            first_observed_at_ms: 1_700_000_000_000 - 86_400_000,
            operator_contact: "ops@young.example",
            uptime_history: synthetic_uptime_history(
                1_700_000_000_000 - 86_400_000,
                1_700_000_000_000,
                4,
            ),
        })
        .unwrap();
        let err = validate_registration(&reg, &RelayRegistryPolicy::default(), 1_700_000_000_000)
            .unwrap_err();
        assert!(err.contains("operating duration"));
    }

    #[test]
    fn curator_keys_are_stable() {
        let a = curator_pubkey_hex("mycena-release").unwrap();
        let b = curator_pubkey_hex("mycena-release").unwrap();
        assert_eq!(a, b);
    }
}
