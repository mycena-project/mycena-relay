// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project

use anyhow::Context;
use clap::Subcommand;
use mycena_l0_facade::release_integrity::{
    artifact_blake3_hex, build_release_signature, compute_build_recipe_digest, merge_signatures,
    release_signer_keypair, ReleaseIntegrityManifest, ReleaseThreshold, RELEASE_INTEGRITY_FORMAT,
    RELEASE_SIGNER_IDS,
};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug, Clone)]
pub enum ReleaseCommands {
    /// BLAKE3 hex digest of a release artifact file.
    Hash { artifact: PathBuf },
    /// Build recipe digest for the repository root.
    RecipeDigest {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Create an unsigned manifest template from build-report.json or flags.
    Init {
        #[arg(long)]
        tag: String,
        #[arg(long)]
        artifact: PathBuf,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = 3)]
        required: u8,
        #[arg(long, default_value_t = 5)]
        total: u8,
        #[arg(short, long, default_value = "release-integrity.json")]
        out: PathBuf,
    },
    /// Add a release signer attestation to a manifest JSON file.
    Sign {
        manifest: PathBuf,
        #[arg(long = "signer-id")]
        signer_id: String,
    },
    /// Verify artifact hash, recipe digest, and M-of-N signatures.
    Verify {
        manifest: PathBuf,
        artifact: PathBuf,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}

pub fn run(cmd: ReleaseCommands) -> anyhow::Result<()> {
    match cmd {
        ReleaseCommands::Hash { artifact } => {
            println!("{}", artifact_blake3_hex(&artifact)?);
        }
        ReleaseCommands::RecipeDigest { repo } => {
            println!("{}", compute_build_recipe_digest(&repo)?);
        }
        ReleaseCommands::Init {
            tag,
            artifact,
            repo,
            required,
            total,
            out,
        } => {
            let artifact_name = artifact
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("artifact")
                .to_string();
            let manifest = ReleaseIntegrityManifest {
                format: RELEASE_INTEGRITY_FORMAT.into(),
                release_tag: tag,
                artifact_name,
                artifact_blake3_hex: artifact_blake3_hex(&artifact)?,
                build_recipe_digest_hex: compute_build_recipe_digest(&repo)?,
                recipe_url: Some(
                    "https://github.com/mycena-project/Mycena/blob/main/docs/RELEASE_INTEGRITY.md"
                        .into(),
                ),
                threshold: ReleaseThreshold { required, total },
                signatures: Vec::new(),
            };
            std::fs::write(&out, serde_json::to_string_pretty(&manifest)?)?;
            eprintln!("Wrote {out:?}");
            eprintln!("Sign with: mycena release sign {out:?} --signer-id <ID>");
            eprintln!("Signers: {}", list_signer_ids());
        }
        ReleaseCommands::Sign {
            manifest,
            signer_id,
        } => {
            let content = std::fs::read_to_string(&manifest)?;
            let base = ReleaseIntegrityManifest::parse_json(&content)?;
            let key = release_signer_keypair(&signer_id)?;
            let signed_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let sig = build_release_signature(&base, &signer_id, &key, signed_at_ms)?;
            let merged = merge_signatures(base, sig);
            std::fs::write(&manifest, serde_json::to_string_pretty(&merged)?)?;
            eprintln!(
                "signatures: {}/{} valid after sign",
                merged.verify_signatures().valid_signatures.len(),
                merged.threshold.required
            );
        }
        ReleaseCommands::Verify {
            manifest,
            artifact,
            repo,
        } => {
            verify_release_file(&manifest, &artifact, &repo)?;
            eprintln!("OFFICIAL: artifact matches manifest with sufficient signatures");
        }
    }
    Ok(())
}

fn verify_release_file(manifest_path: &Path, artifact: &Path, repo: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = ReleaseIntegrityManifest::parse_json(&content)?;
    manifest
        .full_verify(artifact, repo)
        .with_context(|| "release integrity verification failed".to_string())?;
    let report = manifest.verify_signatures();
    eprintln!(
        "valid signers ({}/{} required): {}",
        report.valid_signatures.len(),
        manifest.threshold.required,
        report.valid_signatures.join(", ")
    );
    if !report.rejected.is_empty() {
        eprintln!("rejected: {}", report.rejected.join("; "));
    }
    Ok(())
}

fn list_signer_ids() -> String {
    RELEASE_SIGNER_IDS
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>()
        .join(", ")
}
