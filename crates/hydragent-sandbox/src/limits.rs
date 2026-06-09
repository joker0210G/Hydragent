#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Fuel units allocated for CPU execution. 1 billion units ≈ 1 second of CPU.
    pub max_fuel: u64,
    /// Maximum linear memory bytes.
    pub max_memory_bytes: u64,
    /// Maximum wall-clock time limit.
    pub max_exec_ms: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_fuel: 1_000_000_000,            // ~1 s CPU time
            max_memory_bytes: 64 * 1024 * 1024, // 64 MB RAM
            max_exec_ms: 5000,                  // 5 s wall-clock time limit
        }
    }
}

impl ResourceLimits {
    pub fn strict() -> Self {
        Self {
            max_fuel: 100_000_000,             // ~100 ms CPU time
            max_memory_bytes: 16 * 1024 * 1024, // 16 MB RAM
            max_exec_ms: 2000,                  // 2 s wall-clock time limit
        }
    }

    pub fn trusted() -> Self {
        Self::default()
    }
}
