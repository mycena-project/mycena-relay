// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Mini-app platform: manifests, `.mxa` bundles, local store, and JS bridge source.
#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod bridge_limits;
pub mod bridge_session;
pub mod bundle;
pub mod bundle_scan;
pub mod bundle_signature;
pub mod bundled_listings;
pub mod capability_token;
pub mod csp;
pub mod install_policy;
pub mod manifest;
pub mod policy;
pub mod prefs;
pub mod reproducible_build;
pub mod reputation;
pub mod revocation;
pub mod safe_mode;
pub mod storage_quota;
pub mod store;

pub mod runtime;
#[cfg(feature = "wasm-runtime")]
pub mod wasm_runtime;
#[cfg(not(feature = "wasm-runtime"))]
mod wasm_stubs;

pub use bridge_session::{issue_session, revoke_session, validate_session};
pub use bundle::MiniAppBundle;
pub use bundle_scan::scan_bundle;
pub use capability_token::{
    issue_capability, mac_key_from_db_path, parse_permission_name, permission_for_method,
    validate_capability, DEFAULT_CAP_TTL_MS,
};
pub use csp::{meta_tag as csp_meta_tag, script_src_attr};
pub use install_policy::{InstallPreview, InstallTrust, InstallTrustLevel};
pub use manifest::{MiniAppManifest, Permission};
pub use policy::MiniAppPolicy;
pub use reproducible_build::{content_attestation_hash, ReproducibleBuild};
pub use reputation::{forced_safe_mode, load as load_reputation, suggest_safe_mode, AppReputation};
pub use revocation::{
    ingest_gossip_revocation, is_revoked, revoke_app, RevocationGossip, RevocationSnapshot,
};
pub use runtime::{
    invoke_wasm_run, GuestRuntime, WasmSandboxConfig, WasmSandboxError, WasmSandboxLimits,
    DEFAULT_WASM_ENTRY, DEFAULT_WASM_EXECUTION_TIMEOUT_MS, DEFAULT_WASM_FUEL_PER_INVOCATION,
    DEFAULT_WASM_MAX_MEMORY_BYTES, DEFAULT_WASM_MAX_TABLE_ELEMENTS,
};
#[cfg(feature = "wasm-runtime")]
pub use runtime::{is_valid_wasm_magic, BridgeInvokeCallback, WasmSandbox};
pub use store::{listing_from_manifest_and_bundle, AppSource, AppStore, AppStoreListing};

/// JavaScript bridge injected into the mini-app host (`window.mycena`).
pub fn bridge_api_js_source() -> &'static str {
    include_str!("bridge_api.js")
}
