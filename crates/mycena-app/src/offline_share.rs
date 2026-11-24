// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Phone-to-phone offline distribution helpers (APK + mesh bootstrap sidecar).
//!
//! Shared between desktop [`share_server`](../../mycena-desktop/src/share_server.rs)
//! and Android hotspot HTTP hosts. Port **8765** is the cross-platform convention.

use mycena_l0_facade::data::now_ms;
use serde::{Deserialize, Serialize};

pub const SHARE_PORT: u16 = 8765;
pub const APK_PATH: &str = "/mycena.apk";
pub const BOOTSTRAP_SIDECAR_PATH: &str = "/mesh/bootstrap.myc";
pub const BOOTSTRAP_JSON_PATH: &str = "/mesh/bootstrap.json";

/// Sidecar downloaded alongside the APK so a fresh install can join the sharer's mesh
/// without manual onboarding (imported from Downloads on first launch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OfflineShareBootstrap {
    pub version: u8,
    /// Primary dial target (`mycena://invite/v3#…` or circuit multiaddr).
    pub invite_uri: String,
    pub sharer_peer_id: String,
    /// Landing page the recipient opened (for diagnostics).
    pub install_url: String,
    pub created_at_ms: u64,
}

impl OfflineShareBootstrap {
    pub fn new(invite_uri: String, sharer_peer_id: String, install_url: String) -> Self {
        Self {
            version: 1,
            invite_uri,
            sharer_peer_id,
            install_url,
            created_at_ms: now_ms(),
        }
    }

    pub fn to_sidecar_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn from_sidecar_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Auto display name for sideload recipients (no manual onboarding).
    pub fn guest_display_name(&self) -> String {
        let suffix = self
            .sharer_peer_id
            .chars()
            .skip(6)
            .take(8)
            .collect::<String>();
        if suffix.is_empty() {
            "guest".to_string()
        } else {
            format!("guest-{suffix}")
        }
    }
}

/// Minimal install landing HTML (Android hotspot + desktop share server).
pub fn build_install_landing_html(apk_path: &str, bootstrap_path: &str, subtitle: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8"/>
  <meta name="viewport" content="width=device-width, initial-scale=1"/>
  <title>Install Mycena</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 2rem; text-align: center; }}
    a.btn {{ display: inline-block; margin: 0.75rem; padding: 1rem 1.5rem;
             background: #2e7d32; color: #fff; text-decoration: none; border-radius: 8px; }}
    a.secondary {{ background: #1565c0; }}
    p.hint {{ color: #555; font-size: 0.95rem; max-width: 28rem; margin: 1rem auto; }}
  </style>
</head>
<body>
  <h1>Mycena</h1>
  <p>{subtitle}</p>
  <p class="hint">No Play Store or internet required. Join Wi‑Fi from the other QR first if prompted.</p>
  <a class="btn" href="{apk_path}">Install Mycena</a>
  <a class="btn secondary" href="{bootstrap_path}" download="mycena-bootstrap.myc">Save mesh join file</a>
  <script>
    // Auto-fetch bootstrap sidecar so first app launch can import mesh credentials from Downloads.
    fetch("{bootstrap_path}").then(r => r.blob()).then(b => {{
      const a = document.createElement("a");
      a.href = URL.createObjectURL(b);
      a.download = "mycena-bootstrap.myc";
      a.click();
    }}).catch(() => {{}});
  </script>
</body>
</html>"#
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_sidecar_roundtrip() {
        let b = OfflineShareBootstrap::new(
            "mycena://invite/v1#12D3KooWTest".into(),
            "12D3KooWTestPeerIdHereXXXXXXXXXXXX".into(),
            "http://192.168.43.1:8765/".into(),
        );
        let bytes = b.to_sidecar_bytes().unwrap();
        let back = OfflineShareBootstrap::from_sidecar_bytes(&bytes).unwrap();
        assert_eq!(b, back);
        assert!(back.guest_display_name().starts_with("guest-"));
    }

    #[test]
    fn landing_html_includes_paths() {
        let html = build_install_landing_html(APK_PATH, BOOTSTRAP_SIDECAR_PATH, "test");
        assert!(html.contains(APK_PATH));
        assert!(html.contains(BOOTSTRAP_SIDECAR_PATH));
    }
}
