// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! wasmtime-based mini-app sandbox: Fuel metering, memory caps, execution timeouts.

use std::sync::Arc;
use std::time::Duration;

use wasmtime::{Caller, Engine, Instance, Linker, Module, ResourceLimiter, Store};

/// Default deterministic instruction budget per guest invocation.
pub const DEFAULT_WASM_FUEL_PER_INVOCATION: u64 = 10_000_000;
/// Default linear memory cap for guest modules (16 MiB).
pub const DEFAULT_WASM_MAX_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// Default wasm table element cap.
pub const DEFAULT_WASM_MAX_TABLE_ELEMENTS: usize = 4096;
/// Default wall-clock execution budget (matches bridge call timeout).
pub const DEFAULT_WASM_EXECUTION_TIMEOUT_MS: u64 = 5_000;
/// Standard WASM entry file for `runtime: "wasm"` bundles.
pub const DEFAULT_WASM_ENTRY: &str = "guest.wasm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmSandboxLimits {
    pub fuel_per_invocation: u64,
    pub max_memory_bytes: usize,
    pub execution_timeout_ms: u64,
    pub max_table_elements: usize,
}

impl Default for WasmSandboxLimits {
    fn default() -> Self {
        Self {
            fuel_per_invocation: DEFAULT_WASM_FUEL_PER_INVOCATION,
            max_memory_bytes: DEFAULT_WASM_MAX_MEMORY_BYTES,
            execution_timeout_ms: DEFAULT_WASM_EXECUTION_TIMEOUT_MS,
            max_table_elements: DEFAULT_WASM_MAX_TABLE_ELEMENTS,
        }
    }
}

/// Host bridge callback: `(method, args_json) -> response_json`.
pub type BridgeInvokeCallback = Arc<dyn Fn(&str, &str) -> Result<String, String> + Send + Sync>;

#[derive(Clone, Default)]
pub struct WasmSandboxConfig {
    pub limits: WasmSandboxLimits,
    pub bridge: Option<BridgeInvokeCallback>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmSandboxError {
    InvalidModule(String),
    FuelExhausted,
    ExecutionTimeout,
    MemoryLimitExceeded,
    ExportMissing(String),
    Bridge(String),
    Runtime(String),
}

impl std::fmt::Display for WasmSandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidModule(m) => write!(f, "invalid wasm module: {m}"),
            Self::FuelExhausted => write!(f, "wasm fuel exhausted"),
            Self::ExecutionTimeout => write!(f, "wasm execution timeout"),
            Self::MemoryLimitExceeded => write!(f, "wasm memory limit exceeded"),
            Self::ExportMissing(e) => write!(f, "missing export {e}"),
            Self::Bridge(b) => write!(f, "wasm bridge: {b}"),
            Self::Runtime(r) => write!(f, "wasm runtime: {r}"),
        }
    }
}

struct HostState {
    bridge: Option<BridgeInvokeCallback>,
}

struct LimitState {
    max_memory_bytes: usize,
    max_table_elements: usize,
}

impl ResourceLimiter for LimitState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= self.max_memory_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= self.max_table_elements)
    }
}

struct StoreState {
    host: HostState,
    limiter: LimitState,
}

struct GuestInstance {
    store: Store<StoreState>,
    instance: Instance,
}

/// Loaded wasmtime module ready for sandboxed invocation.
pub struct WasmSandbox {
    engine: Engine,
    module: Module,
    config: WasmSandboxConfig,
}

impl WasmSandbox {
    pub fn from_bytes(
        wasm_bytes: &[u8],
        config: WasmSandboxConfig,
    ) -> Result<Self, WasmSandboxError> {
        if !is_valid_wasm_magic(wasm_bytes) {
            return Err(WasmSandboxError::InvalidModule(
                "missing \\0asm magic".into(),
            ));
        }
        let mut engine_config = wasmtime::Config::new();
        engine_config.consume_fuel(true);
        if config.limits.execution_timeout_ms > 0 {
            engine_config.epoch_interruption(true);
        }
        let engine =
            Engine::new(&engine_config).map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| WasmSandboxError::InvalidModule(e.to_string()))?;
        Ok(Self {
            engine,
            module,
            config,
        })
    }

    pub fn limits(&self) -> WasmSandboxLimits {
        self.config.limits
    }

    /// Invoke exported `run` (or `_start` fallback). Returns guest status code.
    pub fn invoke_run(&self) -> Result<i32, WasmSandboxError> {
        self.with_guest(|mut guest| {
            let func = find_export_func(&mut guest.store, &guest.instance, &["run", "_start"])?;
            let typed = func
                .typed::<(), i32>(&guest.store)
                .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
            typed.call(&mut guest.store, ()).map_err(map_trap)
        })
    }

    fn with_guest<R>(
        &self,
        f: impl FnOnce(GuestInstance) -> Result<R, WasmSandboxError>,
    ) -> Result<R, WasmSandboxError> {
        let timeout_ms = self.config.limits.execution_timeout_ms;
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        let watcher = (timeout_ms > 0).then(|| {
            let engine = self.engine.clone();
            std::thread::spawn(move || {
                match cancel_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        engine.increment_epoch();
                    }
                }
            })
        });
        let result = (|| {
            let guest = self.instantiate_guest()?;
            f(guest)
        })();
        drop(cancel_tx);
        if let Some(w) = watcher {
            let _ = w.join();
        }
        result
    }

    fn instantiate_guest(&self) -> Result<GuestInstance, WasmSandboxError> {
        let mut store = Store::new(
            &self.engine,
            StoreState {
                host: HostState {
                    bridge: self.config.bridge.clone(),
                },
                limiter: LimitState {
                    max_memory_bytes: self.config.limits.max_memory_bytes,
                    max_table_elements: self.config.limits.max_table_elements,
                },
            },
        );
        store
            .set_fuel(self.config.limits.fuel_per_invocation)
            .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
        store.limiter(|state| &mut state.limiter);
        if self.config.limits.execution_timeout_ms > 0 {
            store.set_epoch_deadline(1);
        }
        let mut linker = Linker::new(&self.engine);
        linker
            .func_wrap(
                "mycena",
                "bridge_call",
                |mut caller: Caller<'_, StoreState>,
                 method_ptr: i32,
                 method_len: i32,
                 args_ptr: i32,
                 args_len: i32,
                 out_ptr: i32,
                 out_cap: i32|
                 -> i32 {
                    bridge_call_host(
                        &mut caller,
                        method_ptr,
                        method_len,
                        args_ptr,
                        args_len,
                        out_ptr,
                        out_cap,
                    )
                },
            )
            .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
        Ok(GuestInstance { store, instance })
    }
}

fn find_export_func(
    store: &mut Store<StoreState>,
    instance: &Instance,
    names: &[&str],
) -> Result<wasmtime::Func, WasmSandboxError> {
    for name in names {
        if let Some(func) = instance.get_func(&mut *store, name) {
            return Ok(func);
        }
    }
    Err(WasmSandboxError::ExportMissing(names.join(" or ")))
}

fn bridge_call_host(
    caller: &mut Caller<'_, StoreState>,
    method_ptr: i32,
    method_len: i32,
    args_ptr: i32,
    args_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> i32 {
    let bridge = caller.data().host.bridge.clone();
    let Some(bridge) = bridge else {
        return -1;
    };
    let Some(memory) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
        return -2;
    };
    let method = match read_guest_str(caller, &memory, method_ptr, method_len) {
        Ok(s) => s,
        Err(_) => return -3,
    };
    let args = match read_guest_str(caller, &memory, args_ptr, args_len) {
        Ok(s) => s,
        Err(_) => return -4,
    };
    let response = match bridge(method.as_str(), args.as_str()) {
        Ok(r) => r,
        Err(_) => return -5,
    };
    match write_guest_str(caller, &memory, out_ptr, out_cap, &response) {
        Ok(written) => i32::try_from(written).unwrap_or(-6),
        Err(_) => -7,
    }
}

fn read_guest_str(
    caller: &mut Caller<'_, StoreState>,
    memory: &wasmtime::Memory,
    ptr: i32,
    len: i32,
) -> Result<String, WasmSandboxError> {
    if ptr < 0 || len < 0 {
        return Err(WasmSandboxError::Runtime("negative guest pointer".into()));
    }
    let mut buf = vec![0u8; len as usize];
    memory
        .read(caller, ptr as usize, &mut buf)
        .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| WasmSandboxError::Runtime(e.to_string()))
}

fn write_guest_str(
    caller: &mut Caller<'_, StoreState>,
    memory: &wasmtime::Memory,
    ptr: i32,
    cap: i32,
    data: &str,
) -> Result<usize, WasmSandboxError> {
    if ptr < 0 || cap < 0 {
        return Err(WasmSandboxError::Runtime(
            "negative guest out buffer".into(),
        ));
    }
    let bytes = data.as_bytes();
    if bytes.len() > cap as usize {
        return Err(WasmSandboxError::Runtime(
            "guest out buffer too small".into(),
        ));
    }
    memory
        .write(caller, ptr as usize, bytes)
        .map_err(|e| WasmSandboxError::Runtime(e.to_string()))?;
    Ok(bytes.len())
}

fn map_trap(err: wasmtime::Error) -> WasmSandboxError {
    let detailed = format!("{err:#}").to_lowercase();
    if detailed.contains("fuel") {
        WasmSandboxError::FuelExhausted
    } else if detailed.contains("epoch") || detailed.contains("interrupt") {
        WasmSandboxError::ExecutionTimeout
    } else if detailed.contains("memory") {
        WasmSandboxError::MemoryLimitExceeded
    } else {
        WasmSandboxError::Runtime(err.to_string())
    }
}

pub fn is_valid_wasm_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[..4] == [0x00, 0x61, 0x73, 0x6d]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const RUN_WAT: &str = r#"
        (module
          (func (export "run") (result i32)
            i32.const 42
          )
        )
    "#;

    const LOOP_WAT: &str = r#"
        (module
          (func (export "run") (result i32)
            (loop (br 0))
            unreachable
          )
        )
    "#;

    const MEMORY_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (result i32)
            (if (result i32) (i32.lt_s (memory.grow (i32.const 512)) (i32.const 0))
              (then (unreachable))
              (else (i32.const 0))
            )
          )
        )
    "#;

    fn compile_wat(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("wat")
    }

    fn sandbox_from_wat(wat: &str) -> WasmSandbox {
        let bytes = compile_wat(wat);
        WasmSandbox::from_bytes(&bytes, WasmSandboxConfig::default()).expect("sandbox")
    }

    #[test]
    fn run_export_returns_status() {
        let sandbox = sandbox_from_wat(RUN_WAT);
        let started = std::time::Instant::now();
        assert_eq!(sandbox.invoke_run().expect("run"), 42);
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "invoke_run must not join the full wall-clock timeout"
        );
    }

    #[test]
    fn fuel_direct_without_epoch() {
        let mut engine_config = wasmtime::Config::new();
        engine_config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&engine_config).expect("engine");
        let bytes = compile_wat(LOOP_WAT);
        let module = wasmtime::Module::new(&engine, &bytes).expect("module");
        let mut store = wasmtime::Store::new(&engine, ());
        store.set_fuel(50_000).expect("fuel");
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instance");
        let func = instance.get_func(&mut store, "run").expect("run export");
        let typed = func.typed::<(), i32>(&store).expect("typed");
        let err = typed.call(&mut store, ()).expect_err("should exhaust fuel");
        assert!(
            matches!(map_trap(err), WasmSandboxError::FuelExhausted),
            "expected fuel exhaustion"
        );
    }

    #[test]
    fn fuel_exhaustion_on_infinite_loop() {
        let mut config = WasmSandboxConfig::default();
        config.limits.fuel_per_invocation = 10_000;
        config.limits.execution_timeout_ms = 0;
        let bytes = compile_wat(LOOP_WAT);
        let sandbox = WasmSandbox::from_bytes(&bytes, config).expect("sandbox");
        assert!(matches!(
            sandbox.invoke_run(),
            Err(WasmSandboxError::FuelExhausted)
        ));
    }

    #[test]
    fn memory_growth_limited() {
        let mut config = WasmSandboxConfig::default();
        config.limits.max_memory_bytes = 64 * 1024;
        config.limits.execution_timeout_ms = 2_000;
        let bytes = compile_wat(MEMORY_WAT);
        let sandbox = WasmSandbox::from_bytes(&bytes, config).expect("sandbox");
        assert!(matches!(
            sandbox.invoke_run(),
            Err(WasmSandboxError::MemoryLimitExceeded) | Err(WasmSandboxError::Runtime(_))
        ));
    }
}
