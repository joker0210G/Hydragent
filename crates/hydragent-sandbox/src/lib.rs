pub mod engine;
pub mod limits;
pub mod wasm_tool;

pub use engine::create_sandbox_engine;
pub use limits::ResourceLimits;
pub use wasm_tool::WasmTool;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use hydragent_types::ToolStatus;

    fn get_wasm_path(filename: &str) -> PathBuf {
        let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        // Workspace root is parent of crates/hydragent-sandbox
        let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
        workspace_root.join("sandbox/tools").join(filename)
    }

    #[tokio::test]
    async fn test_echo_tool_execution() {
        let engine = create_sandbox_engine().unwrap();
        let wasm_path = get_wasm_path("echo.wasm");
        let limits = ResourceLimits::default();
        
        let tool = WasmTool::load(&engine, &wasm_path, limits, None).unwrap();
        let params = r#"{"call_id":"test-1","message":"Hello from WASM"}"#;
        let result = tool.execute(params).await.unwrap();
        
        assert_eq!(result.call_id, "test-1");
        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output_json.contains("Hello from WASM"));
    }

    #[tokio::test]
    async fn test_file_read_tool_sandbox() {
        let engine = create_sandbox_engine().unwrap();
        let wasm_path = get_wasm_path("file_read.wasm");
        let limits = ResourceLimits::default();
        
        let temp_dir = std::env::temp_dir().join("hydra_test_wasi");
        fs::create_dir_all(&temp_dir).unwrap();
        
        let test_file = temp_dir.join("hello.txt");
        fs::write(&test_file, "WASI Sandbox works!").unwrap();

        let tool = WasmTool::load(&engine, &wasm_path, limits, Some(temp_dir.clone())).unwrap();
        
        let params = r#"{"call_id":"test-2","path":"hello.txt"}"#;
        let result = tool.execute(params).await.unwrap();
        
        assert_eq!(result.status, ToolStatus::Success);
        assert!(result.output_json.contains("WASI Sandbox works!"));
        
        let _ = fs::remove_file(test_file);
        let _ = fs::remove_dir(temp_dir);
    }

    #[tokio::test]
    async fn test_timeout_limit() {
        let engine = create_sandbox_engine().unwrap();
        let wasm_path = get_wasm_path("echo.wasm");
        
        let mut limits = ResourceLimits::default();
        limits.max_exec_ms = 0;
        
        let tool = WasmTool::load(&engine, &wasm_path, limits, None).unwrap();
        let params = r#"{"call_id":"test-3","message":"Hello"}"#;
        let result = tool.execute(params).await.unwrap();
        
        assert_eq!(result.status, ToolStatus::Timeout);
        assert!(result.error_message.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn test_memory_limit() {
        let engine = create_sandbox_engine().unwrap();
        let wasm_path = get_wasm_path("echo.wasm");
        
        let mut limits = ResourceLimits::default();
        limits.max_memory_bytes = 65536; // 64 KB
        
        let tool = WasmTool::load(&engine, &wasm_path, limits, None).unwrap();
        let params = r#"{"call_id":"test-4","message":"Hello"}"#;
        let result = tool.execute(params).await;
        
        assert!(result.is_err());
    }
}
