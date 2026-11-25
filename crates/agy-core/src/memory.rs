use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProfile {
    LowRam1Gb,
    Balanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressureLevel {
    Normal,
    RunningModerate,
    RunningLow,
    RunningCritical,
    BackgroundTrim,
}

impl MemoryPressureLevel {
    // Maps Android ComponentCallbacks2 TRIM_MEMORY_* constants to portable levels.
    pub fn from_android_trim_level(level: i32) -> Self {
        match level {
            15 => Self::RunningCritical, // TRIM_MEMORY_RUNNING_CRITICAL
            10 => Self::RunningLow,      // TRIM_MEMORY_RUNNING_LOW
            5 => Self::RunningModerate,  // TRIM_MEMORY_RUNNING_MODERATE
            l if l >= 40 => Self::BackgroundTrim,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MemoryBudget {
    pub profile: MemoryProfile,
    pub max_terminal_lines: usize,
    pub max_terminal_bytes: usize,
    pub stream_read_chunk_bytes: usize,
    pub max_file_preview_bytes: usize,
    pub max_search_hits: usize,
    pub max_in_memory_turns: usize,
    pub tokio_worker_threads: usize,
    pub tokio_stack_bytes: usize,
}

impl MemoryBudget {
    pub fn for_device_ram_mb(total_ram_mb: u64) -> Self {
        if total_ram_mb <= 1536 {
            Self {
                profile: MemoryProfile::LowRam1Gb,
                max_terminal_lines: 600,
                max_terminal_bytes: 96 * 1024,
                stream_read_chunk_bytes: 8 * 1024,
                max_file_preview_bytes: 64 * 1024,
                max_search_hits: 100,
                max_in_memory_turns: 24,
                tokio_worker_threads: 2,
                tokio_stack_bytes: 512 * 1024,
            }
        } else {
            Self {
                profile: MemoryProfile::Balanced,
                max_terminal_lines: 1500,
                max_terminal_bytes: 256 * 1024,
                stream_read_chunk_bytes: 16 * 1024,
                max_file_preview_bytes: 192 * 1024,
                max_search_hits: 250,
                max_in_memory_turns: 64,
                tokio_worker_threads: 2,
                tokio_stack_bytes: 768 * 1024,
            }
        }
    }

    pub fn reduced_for_pressure(&self, pressure: MemoryPressureLevel) -> (usize, usize) {
        match pressure {
            MemoryPressureLevel::RunningCritical | MemoryPressureLevel::BackgroundTrim => (
                (self.max_terminal_lines / 4).max(120),
                (self.max_terminal_bytes / 4).max(24 * 1024),
            ),
            MemoryPressureLevel::RunningLow => (
                (self.max_terminal_lines / 2).max(240),
                (self.max_terminal_bytes / 2).max(48 * 1024),
            ),
            MemoryPressureLevel::RunningModerate => (
                (self.max_terminal_lines * 3 / 4).max(320),
                (self.max_terminal_bytes * 3 / 4).max(64 * 1024),
            ),
            MemoryPressureLevel::Normal => (self.max_terminal_lines, self.max_terminal_bytes),
        }
    }
}
