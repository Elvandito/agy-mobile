use crate::compat::{RuntimeSelection, DEFAULT_COMPAT_API_ENDPOINT};
use crate::CoreError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    Standard,
    Plan,
    ReadOnly,
}

impl AgentMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "STANDARD",
            Self::Plan => "PLAN",
            Self::ReadOnly => "READ-ONLY",
        }
    }
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::Standard
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    ConfirmDestructiveAndCommands,
    ConfirmAllWrites,
    ReadOnlyStrict,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self::ConfirmDestructiveAndCommands
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    DarkHardware,
    LightPaper,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::DarkHardware
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    pub display_name: String,
    pub supports_tools: bool,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgyConfig {
    pub selected_model: String,
    pub agent_mode: AgentMode,
    pub permission_policy: PermissionPolicy,
    pub runtime_selection: RuntimeSelection,
    pub theme_mode: ThemeMode,
    pub api_endpoint: String,
    pub oauth_client_id: Option<String>,
    pub active_workspace: Option<PathBuf>,
    pub active_saf_uri: Option<String>,
    pub max_output_tokens: u32,
}

impl Default for AgyConfig {
    fn default() -> Self {
        Self {
            selected_model: "gemini-2.5-flash".to_string(),
            agent_mode: AgentMode::Standard,
            permission_policy: PermissionPolicy::ConfirmDestructiveAndCommands,
            runtime_selection: RuntimeSelection::Auto,
            theme_mode: ThemeMode::DarkHardware,
            api_endpoint: DEFAULT_COMPAT_API_ENDPOINT.to_string(),
            oauth_client_id: None,
            active_workspace: None,
            active_saf_uri: None,
            max_output_tokens: 4096,
        }
    }
}

impl AgyConfig {
    pub fn load_or_default(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let cfg: Self = serde_json::from_str(&raw)?;
        Ok(cfg)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let serialized = serde_json::to_string_pretty(self)?;
        fs::write(&tmp, serialized.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn builtin_compat_models() -> Vec<ModelSpec> {
        vec![
            ModelSpec {
                id: "gemini-2.5-flash".to_string(),
                display_name: "Gemini 2.5 Flash (Fast, low latency)".to_string(),
                supports_tools: true,
                source: "google-api".to_string(),
            },
            ModelSpec {
                id: "gemini-2.5-pro".to_string(),
                display_name: "Gemini 2.5 Pro (Deep reasoning & code)".to_string(),
                supports_tools: true,
                source: "google-api".to_string(),
            },
            ModelSpec {
                id: "gemini-2.0-flash".to_string(),
                display_name: "Gemini 2.0 Flash".to_string(),
                supports_tools: true,
                source: "google-api".to_string(),
            },
        ]
    }
}
