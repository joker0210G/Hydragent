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
