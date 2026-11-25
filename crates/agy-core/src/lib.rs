pub mod compat;
pub mod config;
pub mod memory;
pub mod session;

pub use compat::{
    assess_device_compatibility, AbiTarget, CompatibilityReport, DeviceInfo, ExecutionRuntime,
    OfficialCliStatus, RuntimeSelection,
};
pub use config::{AgentMode, AgyConfig, ModelSpec, PermissionPolicy, ThemeMode};
pub use memory::{MemoryBudget, MemoryPressureLevel, MemoryProfile};
pub use session::{
    ApprovalAction, ApprovalKind, PendingApproval, Role, SessionHeader, SessionStore, ToolCallRecord,
    TurnMessage,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Session '{0}' was not found")]
    SessionNotFound(String),
    #[error("Invalid configuration value: {0}")]
    InvalidConfig(String),
}
