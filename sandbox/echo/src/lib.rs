use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output_json: String,
    pub status: ToolStatus,
    pub execution_ms: u32,
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Failure,
    Timeout,
}

#[derive(Debug, Deserialize)]
struct EchoParams {
    call_id: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct EchoOutput {
    message: String,
}

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> *mut u8 {
    let mut buf = Vec::with_capacity(size as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn tool_execute(ptr: *mut u8, len: i32) -> u64 {
    let params_slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let params_str = std::str::from_utf8(params_slice).unwrap_or("");

    let result = execute_inner(params_str);
    let result_json = serde_json::to_string(&result).unwrap();
    let result_bytes = result_json.into_bytes();
    let res_len = result_bytes.len() as u64;
    let res_ptr = result_bytes.as_ptr() as u64;
    std::mem::forget(result_bytes);

    (res_ptr << 32) | res_len
}

fn execute_inner(params_json: &str) -> ToolResult {
    let params: EchoParams = match serde_json::from_str(params_json) {
        Ok(p) => p,
        Err(e) => {
            return ToolResult {
                call_id: "unknown".to_string(),
                output_json: "".to_string(),
                status: ToolStatus::Failure,
                execution_ms: 0,
                error_message: Some(format!("Failed to parse params: {}", e)),
            };
        }
    };

    let output = EchoOutput {
        message: params.message,
    };

    ToolResult {
        call_id: params.call_id,
        output_json: serde_json::to_string(&output).unwrap(),
        status: ToolStatus::Success,
        execution_ms: 1,
        error_message: None,
    }
}
