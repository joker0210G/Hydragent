use anyhow::Context;
use std::path::PathBuf;
use wasmtime::*;
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;
use hydragent_types::ToolResult;
use crate::limits::ResourceLimits;

pub struct SandboxCtx {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

pub struct WasmTool {
    engine: Engine,
    module: Module,
    limits: ResourceLimits,
    preopened_dir: Option<PathBuf>,
}

impl WasmTool {
    pub fn load(
        engine: &Engine,
        wasm_path: &std::path::Path,
        limits: ResourceLimits,
        preopened_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let module = Module::from_file(engine, wasm_path)?;
        Ok(Self {
            engine: engine.clone(),
            module,
            limits,
            preopened_dir,
        })
    }

    pub async fn execute(&self, params_json: &str) -> anyhow::Result<ToolResult> {
        let engine = self.engine.clone();
        let module = self.module.clone();
        let limits = self.limits.clone();
        let preopened_dir = self.preopened_dir.clone();
        let params = params_json.to_string();

        let max_exec_ms = limits.max_exec_ms;
        let timeout_duration = std::time::Duration::from_millis(max_exec_ms);
        let result = tokio::time::timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                execute_wasm_sync(&engine, &module, &limits, preopened_dir, &params)
            })
        )
        .await;

        match result {
            Ok(Ok(Ok(res))) => Ok(res),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_panic_err)) => Err(anyhow::anyhow!("WASM execution panicked")),
            Err(_) => {
                Ok(ToolResult {
                    call_id: "".to_string(),
                    output_json: "".to_string(),
                    status: hydragent_types::ToolStatus::Timeout,
                    execution_ms: max_exec_ms as u32,
                    error_message: Some("Execution timed out".to_string()),
                })
            }
        }
    }
}

fn execute_wasm_sync(
    engine: &Engine,
    module: &Module,
    limits: &ResourceLimits,
    preopened_dir: Option<PathBuf>,
    params: &str,
) -> anyhow::Result<ToolResult> {
    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.inherit_stdout().inherit_stderr();

    if let Some(host_path) = preopened_dir {
        wasi_builder.preopened_dir(
            host_path,
            "/workspace",
            wasmtime_wasi::DirPerms::all(),
            wasmtime_wasi::FilePerms::all(),
        )?;
    }

    let wasi = wasi_builder.build_p1();

    let store_limits = StoreLimitsBuilder::new()
        .memory_size(limits.max_memory_bytes as usize)
        .instances(1)
        .build();

    let state = SandboxCtx {
        wasi,
        limits: store_limits,
    };

    let mut store = Store::new(engine, state);
    store.limiter(|s| &mut s.limits);
    store.set_fuel(limits.max_fuel)?;

    let mut linker: Linker<SandboxCtx> = Linker::new(engine);
    wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |s| &mut s.wasi)?;

    // G1 invariant: deny WASM network access. wasmtime 22.x
    // registers a `sock_*` subset in the preview1 linker. We
    // shadow each one with a trap-only Func of the matching
    // signature, derived from the *guest's* import list. This
    // is robust against future wasmtime versions that might
    // change the signature: we always match the actual import.
    //
    // Net effect: any WASM that imports a `sock_*` symbol still
    // links successfully (type-compatible Func) but traps the
    // moment the guest calls it, with a recognisable error
    // message. The host network stack is never reachable.
    linker.allow_shadowing(true);
    for name in &[
        "sock_accept",
        "sock_recv",
        "sock_send",
        "sock_shutdown",
    ] {
        let trap_label = format!(
            "sandbox: '{}' is disabled (no network access from WASM tools)",
            name
        );
        // Look up the import's FuncType in the guest module so
        // the shadowed Func exactly matches what the guest
        // expects — that is the only way the linker will accept
        // the override.
        let mut found_ty: Option<wasmtime::FuncType> = None;
        for imp in module.imports() {
            if imp.name().to_string() == *name
                && imp.module().to_string() == "wasi_snapshot_preview1"
            {
                if let wasmtime::ExternType::Func(f) = imp.ty() {
                    found_ty = Some(f.clone());
                    break;
                }
            }
        }
        if let Some(ty) = found_ty {
            let func = Func::new(
                &mut store,
                ty,
                move |_caller: Caller<'_, SandboxCtx>,
                      _params: &[Val],
                      _results: &mut [Val]|
                      -> Result<(), wasmtime::Error> {
                    Err(wasmtime::Error::msg(trap_label.clone()))
                },
            );
            linker.define(
                &mut store,
                "wasi_snapshot_preview1",
                name,
                func,
            )?;
        }
    }

    let instance = linker.instantiate(&mut store, module)?;

    let tool_execute: TypedFunc<(i32, i32), u64> =
        instance.get_typed_func(&mut store, "tool_execute")?;

    let memory = instance
        .get_memory(&mut store, "memory")
        .context("No memory export found in WASM tool")?;

    let alloc: TypedFunc<i32, i32> = instance.get_typed_func(&mut store, "alloc")?;

    let params_bytes = params.as_bytes();
    let ptr = alloc.call(&mut store, params_bytes.len() as i32)?;
    memory.write(&mut store, ptr as usize, params_bytes)?;

    let res_u64 = tool_execute.call(&mut store, (ptr, params_bytes.len() as i32))?;
    let res_ptr = (res_u64 >> 32) as usize;
    let res_len = (res_u64 & 0xFFFFFFFF) as usize;

    let mut result_bytes = vec![0u8; res_len];
    memory.read(&store, res_ptr, &mut result_bytes)?;

    let result_str = std::str::from_utf8(&result_bytes)?;
    let tool_result: ToolResult = serde_json::from_str(result_str)?;

    Ok(tool_result)
}
