// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Official release integrity — reproducible build attestation and M-of-N signing.
//!
//! Mini-app bundles use `mycena-miniapp::reproducible_build`; this module covers
//! **Mycena core** artifacts (Android APK, desktop bundles, etc.).

use crate::account_identity::sign_account_message;
use crate::relay_registry::{key_from_seed, verify_pubkey_signature};
use libp2p::identity::Keypair;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const RELEASE_INTEGRITY_FORMAT: &str = "mycena-release-integrity/v1";

/// Default official threshold: 3 independent signers of 5.
pub const DEFAULT_REQUIRED_SIGNATURES: u8 = 3;
pub const DEFAULT_TOTAL_SIGNERS: u8 = 5;

/// Independent release signers (Ed25519 keys derived via [`release_signer_keypair`]).
pub const RELEASE_SIGNER_IDS: &[(&str, &str)] = &[
    ("release-signer-eu", "Frankfurt / EU"),
    ("release-signer-na-east", "Virginia / US-East"),
    ("release-signer-na-west", "Oregon / US-West"),
    ("release-signer-ap", "Singapore / APAC"),
    ("release-signer-sa", "São Paulo / South America"),
];

/// Files that define the reproducible build recipe (sorted, hashed in order).
pub const RECIPE_PATHS: &[&str] = &[
    "rust-toolchain.toml",
    "Cargo.lock",
    "docker/reproducible-build/Dockerfile",
    "docker/reproducible-build/toolchain.env",
    "docker/reproducible-build/entrypoint.sh",
    "scripts/reproducible-build.sh",
    "android/build.sh",
    "android/gradle.properties",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseThreshold {
    pub required: u8,
    pub total: u8,
}

impl Default for ReleaseThreshold {
    fn default() -> Self {
        Self {
            required: DEFAULT_REQUIRED_SIGNATURES,
            total: DEFAULT_TOTAL_SIGNERS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseSignature {
    pub signer_id: String,
    pub signer_pubkey_hex: String,
    pub signed_at_ms: u64,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseIntegrityManifest {
    pub format: String,
    pub release_tag: String,
    pub artifact_name: String,
    pub artifact_blake3_hex: String,
    pub build_recipe_digest_hex: String,
    #[serde(default)]
    pub recipe_url: Option<String>,
    #[serde(default)]
    pub threshold: ReleaseThreshold,
    #[serde(default)]
    pub signatures: Vec<ReleaseSignature>,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseVerificationReport {
    pub valid_signatures: Vec<String>,
    pub rejected: Vec<String>,
}

impl ReleaseIntegrityManifest {
    pub fn parse_json(json: &str) -> anyhow::Result<Self> {
        let manifest: Self = serde_json::from_str(json)?;
        if manifest.format != RELEASE_INTEGRITY_FORMAT {
            anyhow::bail!("unsupported release integrity format: {}", manifest.format);
        }
        Ok(manifest)
    }

    pub fn attestation_signable_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MYCENA/RELEASE/ATTEST/v1\0");
        bytes.extend_from_slice(self.release_tag.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.artifact_name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.artifact_blake3_hex.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.build_recipe_digest_hex.as_bytes());
        bytes.push(0);
        bytes
    }

    pub fn verify_artifact_hash(&self, artifact_path: &Path) -> anyhow::Result<bool> {
        let computed = artifact_blake3_hex(artifact_path)?;
        Ok(computed.eq_ignore_ascii_case(&self.artifact_blake3_hex))
    }

    pub fn verify_recipe_digest(&self, repo_root: &Path) -> anyhow::Result<bool> {
        let computed = compute_build_recipe_digest(repo_root)?;
        Ok(computed.eq_ignore_ascii_case(&self.build_recipe_digest_hex))
    }

    pub fn verify_signatures(&self) -> ReleaseVerificationReport {
        let mut report = ReleaseVerificationReport::default();
        let bytes = self.attestation_signable_bytes();
        let mut seen_signers = HashSet::new();

        for sig in &self.signatures {
            if !release_signer_pubkey_matches_id(&sig.signer_id, &sig.signer_pubkey_hex) {
                report
                    .rejected
                    .push(format!("{}: unknown signer id or pubkey", sig.signer_id));
                continue;
            }
            if !seen_signers.insert(sig.signer_id.clone()) {
                report
                    .rejected
                    .push(format!("{}: duplicate signer", sig.signer_id));
                continue;
            }
            let Ok(raw) = hex::decode(sig.signature_hex.trim()) else {
                report
                    .rejected
                    .push(format!("{}: invalid signature hex", sig.signer_id));
                continue;
            };
            if !verify_pubkey_signature(&sig.signer_pubkey_hex, &bytes, &raw) {
                report
                    .rejected
                    .push(format!("{}: invalid signature", sig.signer_id));
                continue;
            }
            report.valid_signatures.push(sig.signer_id.clone());
        }
        report
    }

    pub fn is_official(&self) -> bool {
        self.is_official_with_threshold(self.threshold.required)
    }

    pub fn is_official_with_threshold(&self, required: u8) -> bool {
        let report = self.verify_signatures();
        report.valid_signatures.len() >= required as usize
    }

    pub fn full_verify(&self, artifact_path: &Path, repo_root: &Path) -> anyhow::Result<bool> {
        if !self.verify_artifact_hash(artifact_path)? {
            anyhow::bail!("artifact BLAKE3 mismatch");
        }
        if !self.verify_recipe_digest(repo_root)? {
            anyhow::bail!("build recipe digest mismatch");
        }
        if !self.is_official() {
            anyhow::bail!(
                "insufficient valid signatures ({}/{})",
                self.verify_signatures().valid_signatures.len(),
                self.threshold.required
            );
        }
        Ok(true)
    }
}

impl ReleaseSignature {
    pub fn signable_bytes(manifest: &ReleaseIntegrityManifest) -> Vec<u8> {
        manifest.attestation_signable_bytes()
    }
}

pub fn release_signer_keypair(signer_id: &str) -> anyhow::Result<Keypair> {
    key_from_seed(format!("release-signer:{signer_id}:v1").as_bytes())
}

pub fn release_signer_pubkey_hex(signer_id: &str) -> anyhow::Result<String> {
    Ok(hex::encode(
        release_signer_keypair(signer_id)?
            .public()
            .encode_protobuf(),
    ))
}

fn release_signer_pubkey_matches_id(signer_id: &str, pubkey_hex: &str) -> bool {
    if !RELEASE_SIGNER_IDS.iter().any(|(id, _)| *id == signer_id) {
        return false;
    }
    release_signer_pubkey_hex(signer_id)
        .ok()
        .is_some_and(|expected| expected.eq_ignore_ascii_case(pubkey_hex.trim()))
}

pub fn artifact_blake3_hex(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub fn compute_build_recipe_digest(repo_root: &Path) -> anyhow::Result<String> {
    let mut paths: Vec<PathBuf> = RECIPE_PATHS.iter().map(|p| repo_root.join(p)).collect();
    paths.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mycena-core-recipe-v1\0");
    for path in paths {
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy();
        let data = std::fs::read(&path).map_err(|e| anyhow::anyhow!("recipe file {rel}: {e}"))?;
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(&(data.len() as u64).to_le_bytes());
        hasher.update(&data);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn build_release_signature(
    manifest: &ReleaseIntegrityManifest,
    signer_id: &str,
    signer_key: &Keypair,
    signed_at_ms: u64,
) -> anyhow::Result<ReleaseSignature> {
    if !release_signer_pubkey_matches_id(
        signer_id,
        &hex::encode(signer_key.public().encode_protobuf()),
    ) {
        anyhow::bail!("signer key does not match signer_id {signer_id}");
    }
    let bytes = ReleaseSignature::signable_bytes(manifest);
    let sig = sign_account_message(signer_key, &bytes)?;
    Ok(ReleaseSignature {
        signer_id: signer_id.to_string(),
        signer_pubkey_hex: hex::encode(signer_key.public().encode_protobuf()),
        signed_at_ms,
        signature_hex: hex::encode(sig),
    })
}

pub fn merge_signatures(
    mut manifest: ReleaseIntegrityManifest,
    extra: ReleaseSignature,
) -> ReleaseIntegrityManifest {
    if manifest
        .signatures
        .iter()
        .any(|s| s.signer_id == extra.signer_id)
    {
        manifest
            .signatures
            .retain(|s| s.signer_id != extra.signer_id);
    }
    manifest.signatures.push(extra);
    manifest
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::io::Write;

    #[test]
    fn recipe_digest_stable() {
        let dir = tempfile::tempdir().unwrap();
        for rel in RECIPE_PATHS {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, format!("stub-{rel}")).unwrap();
        }
        let a = compute_build_recipe_digest(dir.path()).unwrap();
        let b = compute_build_recipe_digest(dir.path()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn three_of_five_makes_official() {
        let manifest = sample_manifest();
        let mut signed = manifest.clone();
        signed.signatures.clear();
        for id in [
            "release-signer-eu",
            "release-signer-ap",
            "release-signer-sa",
        ] {
            let key = release_signer_keypair(id).unwrap();
            let sig = build_release_signature(&signed, id, &key, 1_700_000_000_000).unwrap();
            signed = merge_signatures(signed, sig);
        }
        assert!(signed.is_official());
        assert_eq!(signed.verify_signatures().valid_signatures.len(), 3);
    }

    #[test]
    fn two_of_five_not_official() {
        let manifest = sample_manifest();
        let mut signed = manifest.clone();
        signed.signatures.clear();
        for id in ["release-signer-eu", "release-signer-ap"] {
            let key = release_signer_keypair(id).unwrap();
            let sig = build_release_signature(&signed, id, &key, 1_700_000_000_000).unwrap();
            signed = merge_signatures(signed, sig);
        }
        assert!(!signed.is_official());
    }

    #[test]
    fn wrong_artifact_hash_fails_verify() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("app.apk");
        std::fs::write(&artifact, b"real-bytes").unwrap();
        let manifest = sample_manifest();
        assert!(!manifest.verify_artifact_hash(&artifact).unwrap());
    }

    fn sample_manifest() -> ReleaseIntegrityManifest {
        ReleaseIntegrityManifest {
            format: RELEASE_INTEGRITY_FORMAT.into(),
            release_tag: "v0.1.62".into(),
            artifact_name: "mycena-v0.1.62-android.apk".into(),
            artifact_blake3_hex: blake3::hash(b"test-artifact").to_hex().to_string(),
            build_recipe_digest_hex: blake3::hash(b"test-recipe").to_hex().to_string(),
            recipe_url: Some(
                "https://github.com/mycena-project/Mycena/blob/main/docs/RELEASE_INTEGRITY.md"
                    .into(),
            ),
            threshold: ReleaseThreshold::default(),
            signatures: Vec::new(),
        }
    }

    #[test]
    fn artifact_hash_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello-release").unwrap();
        let hex = artifact_blake3_hex(&path).unwrap();
        assert_eq!(hex.len(), 64);
    }
}
