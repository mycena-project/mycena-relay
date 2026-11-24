// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Run installed WASM mini-apps with bridge delegation into [`BridgeHost`].

use std::sync::Arc;

use mycena_miniapp::{
    invoke_wasm_run, issue_session, BridgeInvokeCallback, GuestRuntime, MiniAppBundle,
    WasmSandboxConfig, WasmSandboxError, WasmSandboxLimits, DEFAULT_WASM_EXECUTION_TIMEOUT_MS,
    DEFAULT_WASM_FUEL_PER_INVOCATION, DEFAULT_WASM_MAX_MEMORY_BYTES,
    DEFAULT_WASM_MAX_TABLE_ELEMENTS,
};
use serde_json::{json, Value};

use crate::bridge_host::BridgeHost;

/// Result of a WASM guest `run` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmGuestRunResult {
    pub status: i32,
    pub runtime: GuestRuntime,
}

/// Build wasmtime sandbox limits aligned with bridge call timeout.
pub fn default_wasm_limits() -> WasmSandboxLimits {
    WasmSandboxLimits {
        fuel_per_invocation: DEFAULT_WASM_FUEL_PER_INVOCATION,
        max_memory_bytes: DEFAULT_WASM_MAX_MEMORY_BYTES,
        execution_timeout_ms: DEFAULT_WASM_EXECUTION_TIMEOUT_MS,
        max_table_elements: DEFAULT_WASM_MAX_TABLE_ELEMENTS,
    }
}

fn bridge_callback(host: BridgeHost, session: String) -> BridgeInvokeCallback {
    Arc::new(move |method, args_json| {
        let mut args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
        if args.get("_session").is_none() {
            if let Value::Object(ref mut map) = args {
                map.insert("_session".into(), json!(session));
            } else {
                args = json!({ "_session": session, "payload": args });
            }
        }
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            match host.handle(method, &args).await {
                Ok(v) => serde_json::to_string(&v).map_err(|e| e.to_string()),
                Err(e) => Err(e),
            }
        })
    })
}

/// Invoke WASM guest logic for an installed app id.
pub async fn run_wasm_guest_for_app(
    app_id: &str,
    store: &mycena_miniapp::AppStore,
    host: BridgeHost,
) -> Result<WasmGuestRunResult, WasmSandboxError> {
    let bundle = store
        .load_installed_bundle(app_id)
        .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?
        .ok_or_else(|| WasmSandboxError::Runtime("bundle not installed".into()))?;
    run_wasm_guest(&bundle, host).await
}

/// Invoke WASM guest logic for an installed bundle using the shared bridge host.
pub async fn run_wasm_guest(
    bundle: &MiniAppBundle,
    host: BridgeHost,
) -> Result<WasmGuestRunResult, WasmSandboxError> {
    if bundle.guest_runtime() != GuestRuntime::Wasm {
        return Err(WasmSandboxError::Runtime(
            "bundle is not wasm runtime".into(),
        ));
    }
    let session = issue_session(host.app_id());
    let config = WasmSandboxConfig {
        limits: default_wasm_limits(),
        bridge: Some(bridge_callback(host, session)),
    };
    let status = invoke_wasm_run(bundle, config)?;
    Ok(WasmGuestRunResult {
        status,
        runtime: GuestRuntime::Wasm,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use mycena_miniapp::manifest::MiniAppManifest;
    use std::collections::HashMap;

    #[tokio::test]
    async fn wasm_guest_run_without_bridge() {
        let wat = r#"(module (func (export "run") (result i32) i32.const 9))"#;
        let wasm = wat::parse_str(wat).unwrap();
        let manifest = MiniAppManifest {
            id: "com.test.wasm".into(),
            name: "W".into(),
            description: "d".into(),
            version: "1.0.0".into(),
            developer: "X".into(),
            developer_peer_id: None,
            entry: "guest.wasm".into(),
            icon_base64: None,
            permissions: vec![],
            min_mycena_version: "0.1.0".into(),
            accepts_payments: false,
            payment_address: None,
            categories: vec![],
            runtime: "wasm".into(),
            bulletin_scopes: vec![],
            reproducible_build: None,
        };
        let bundle = MiniAppBundle {
            manifest,
            files: HashMap::from([("guest.wasm".into(), wasm)]),
        };
        let config = WasmSandboxConfig {
            limits: default_wasm_limits(),
            bridge: None,
        };
        let status = invoke_wasm_run(&bundle, config).unwrap();
        assert_eq!(status, 9);
    }
}
