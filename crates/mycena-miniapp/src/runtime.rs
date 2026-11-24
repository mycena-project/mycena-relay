// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Guest runtime selection: WebView (legacy) vs wasmtime WASM sandbox.

use super::bundle::MiniAppBundle;
use super::manifest::MiniAppManifest;

#[cfg(feature = "wasm-runtime")]
pub use super::wasm_runtime::{
    is_valid_wasm_magic, BridgeInvokeCallback, WasmSandbox, WasmSandboxConfig, WasmSandboxError,
    WasmSandboxLimits, DEFAULT_WASM_ENTRY, DEFAULT_WASM_EXECUTION_TIMEOUT_MS,
    DEFAULT_WASM_FUEL_PER_INVOCATION, DEFAULT_WASM_MAX_MEMORY_BYTES,
    DEFAULT_WASM_MAX_TABLE_ELEMENTS,
};

#[cfg(not(feature = "wasm-runtime"))]
pub use super::wasm_stubs::{
    WasmSandboxConfig, WasmSandboxError, WasmSandboxLimits, DEFAULT_WASM_ENTRY,
    DEFAULT_WASM_EXECUTION_TIMEOUT_MS, DEFAULT_WASM_FUEL_PER_INVOCATION,
    DEFAULT_WASM_MAX_MEMORY_BYTES, DEFAULT_WASM_MAX_TABLE_ELEMENTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestRuntime {
    /// HTML/JS in platform WebView (compatibility fallback).
    WebView,
    /// Logic in wasmtime sandbox; optional declarative UI in WebView.
    Wasm,
}

impl GuestRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WebView => "webview",
            Self::Wasm => "wasm",
        }
    }
}

impl MiniAppManifest {
    pub fn guest_runtime(&self) -> GuestRuntime {
        if self.runtime == "wasm" {
            GuestRuntime::Wasm
        } else {
            GuestRuntime::WebView
        }
    }
}

/// Execute WASM guest `run` export from an installed bundle.
#[cfg(feature = "wasm-runtime")]
pub fn invoke_wasm_run(
    bundle: &MiniAppBundle,
    config: WasmSandboxConfig,
) -> Result<i32, WasmSandboxError> {
    let wasm = bundle
        .wasm_bytes()
        .ok_or_else(|| WasmSandboxError::InvalidModule("no wasm entry".into()))?;
    let sandbox = WasmSandbox::from_bytes(wasm, config)?;
    sandbox.invoke_run()
}

#[cfg(not(feature = "wasm-runtime"))]
pub fn invoke_wasm_run(
    _bundle: &MiniAppBundle,
    _config: WasmSandboxConfig,
) -> Result<i32, WasmSandboxError> {
    Err(WasmSandboxError::Runtime(
        "wasm-runtime feature not enabled".into(),
    ))
}

impl MiniAppBundle {
    pub fn guest_runtime(&self) -> GuestRuntime {
        self.manifest.guest_runtime()
    }

    pub fn wasm_bytes(&self) -> Option<&[u8]> {
        if self.manifest.guest_runtime() != GuestRuntime::Wasm {
            return None;
        }
        let entry = self.manifest.entry.as_str();
        let bytes = self.get_file(entry)?;
        if is_wasm_magic(bytes) {
            Some(bytes)
        } else {
            None
        }
    }

    pub fn requires_webview_shell(&self) -> bool {
        match self.guest_runtime() {
            GuestRuntime::WebView => true,
            GuestRuntime::Wasm => {
                self.get_file("ui/index.html").is_some()
                    || self
                        .manifest
                        .hybrid_ui_entry()
                        .is_some_and(|p| self.get_file(p).is_some())
            }
        }
    }
}

fn is_wasm_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[..4] == [0x00, 0x61, 0x73, 0x6d]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::manifest::MiniAppManifest;
    use std::collections::HashMap;

    fn wasm_manifest() -> MiniAppManifest {
        MiniAppManifest {
            id: "com.test.wasm".into(),
            name: "W".into(),
            description: "d".into(),
            version: "1.0.0".into(),
            developer: "X".into(),
            developer_peer_id: None,
            entry: DEFAULT_WASM_ENTRY.into(),
            icon_base64: None,
            permissions: vec![],
            min_mycena_version: "0.1.0".into(),
            accepts_payments: false,
            payment_address: None,
            categories: vec![],
            runtime: "wasm".into(),
            bulletin_scopes: vec![],
            reproducible_build: None,
        }
    }

    #[test]
    fn webview_bundle_requires_webview_shell() {
        let manifest = MiniAppManifest {
            runtime: "webview".into(),
            entry: "index.html".into(),
            id: "com.test.app".into(),
            name: "T".into(),
            description: "d".into(),
            version: "1".into(),
            developer: "X".into(),
            developer_peer_id: None,
            icon_base64: None,
            permissions: vec![],
            min_mycena_version: "0.1.0".into(),
            accepts_payments: false,
            payment_address: None,
            categories: vec![],
            bulletin_scopes: vec![],
            reproducible_build: None,
        };
        let bundle = MiniAppBundle {
            manifest,
            files: HashMap::new(),
        };
        assert!(bundle.requires_webview_shell());
    }

    #[cfg(feature = "wasm-runtime")]
    #[test]
    fn wasm_run_invocation() {
        let wat = r#"(module (func (export "run") (result i32) i32.const 7))"#;
        let wasm = wat::parse_str(wat).unwrap();
        let manifest = wasm_manifest();
        manifest.validate().unwrap();
        let bundle = MiniAppBundle {
            manifest,
            files: HashMap::from([(DEFAULT_WASM_ENTRY.into(), wasm)]),
        };
        let status = invoke_wasm_run(&bundle, WasmSandboxConfig::default()).unwrap();
        assert_eq!(status, 7);
    }
}
