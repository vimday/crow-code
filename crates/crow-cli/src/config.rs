//! Shared CLI configuration resolved from project file and environment variables.
//!
//! Priority: Environment variables > `.crow/config.json` > defaults.

use crow_brain::{BrainError, LlmProviderConfig, ProviderKind};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// ─── Write Mode ─────────────────────────────────────────────────

/// Controls how the framework handles filesystem propagation from the execution sandbox
/// back to the live workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Never touch the real workspace. All changes stay in sandbox.
    /// Useful for dry-runs, CI pipelines, and demos.
    SandboxOnly,
    /// Apply only if EvidenceMatrix meets auto-apply threshold.
    /// This is the **default** — safe but productive.
    WorkspaceWrite,
    /// Apply without verification (requires explicit opt-in via
    /// `CROW_WRITE_MODE=danger` or config). Not recommended.
    DangerFullAccess,
}

impl WriteMode {
    fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "sandbox" | "sandbox-only" | "sandboxonly" => Some(WriteMode::SandboxOnly),
            "write" | "workspace-write" | "workspacewrite" | "default" => {
                Some(WriteMode::WorkspaceWrite)
            }
            "danger" | "full" | "danger-full-access" => Some(WriteMode::DangerFullAccess),
            _ => None,
        }
    }
}

impl std::fmt::Display for WriteMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteMode::SandboxOnly => write!(f, "sandbox-only"),
            WriteMode::WorkspaceWrite => write!(f, "workspace-write"),
            WriteMode::DangerFullAccess => write!(f, "danger-full-access"),
        }
    }
}

// ─── File-based configuration shapes ───────────────────────────

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    llm: Option<LlmConfigFile>,
    workspace: Option<WorkspaceConfigFile>,
    mcp_servers: Option<std::collections::HashMap<String, McpServerConfig>>,
}

/// Configuration for a remote Model Context Protocol (MCP) server integration.
#[derive(Debug, Deserialize, Clone)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LlmConfigFile {
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    max_tokens: Option<u32>,
    connect_timeout: Option<u64>,
    request_timeout: Option<u64>,
    json_mode: Option<bool>,
    prompt_caching: Option<bool>,
    reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct WorkspaceConfigFile {
    map_budget: Option<usize>,
    write_mode: Option<String>,
}

// ─── Runtime configuration ──────────────────────────────────────

/// All configuration for a crow session.
#[derive(Debug, Clone)]
pub struct CrowConfig {
    pub workspace: PathBuf,
    pub llm: LlmProviderConfig,
    pub map_budget: usize,
    pub write_mode: WriteMode,
    pub mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

impl CrowConfig {
    /// Resolve configuration from project file and environment.
    /// Fails fast with a clear message if configuration values are invalid.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_for(&env::current_dir()?)
    }

    /// Resolve configuration explicitly for a given workspace path.
    pub fn load_for(workspace_dir: &Path) -> anyhow::Result<Self> {
        let local_config_path = workspace_dir.join(".crow").join("config.json");

        // Skip global config entirely if testing
        let is_testing = std::env::var("CROW_TEST_ENV").is_ok() || cfg!(test);

        let home_dir = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        let global_config_path = home_dir.join(".crow").join("config.json");

        let mut file_cfg = ConfigFile::default();

        // 1. Load global config
        if !is_testing && global_config_path.exists() {
            let content = fs::read_to_string(&global_config_path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read global config at {}: {}",
                    global_config_path.display(),
                    e
                )
            })?;
            let cfg = serde_json::from_str::<ConfigFile>(&content).map_err(|e| {
                anyhow::anyhow!(
                    "Syntax error in global config at {}: {}",
                    global_config_path.display(),
                    e
                )
            })?;
            file_cfg = cfg;
        }

        // 2. Load local config and merge
        if local_config_path.exists() {
            let content = fs::read_to_string(&local_config_path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read local config at {}: {}",
                    local_config_path.display(),
                    e
                )
            })?;
            let cfg = serde_json::from_str::<ConfigFile>(&content).map_err(|e| {
                anyhow::anyhow!(
                    "Syntax error in local config at {}: {}",
                    local_config_path.display(),
                    e
                )
            })?;

            if let Some(llm) = cfg.llm {
                if let Some(ref mut global_llm) = file_cfg.llm {
                    if llm.provider.is_some() {
                        global_llm.provider = llm.provider.clone();
                    }
                    if llm.api_key.is_some() {
                        global_llm.api_key = llm.api_key.clone();
                    }
                    if llm.model.is_some() {
                        global_llm.model = llm.model.clone();
                    }
                    if llm.base_url.is_some() {
                        global_llm.base_url = llm.base_url.clone();
                    }
                    if llm.max_tokens.is_some() {
                        global_llm.max_tokens = llm.max_tokens;
                    }
                    if llm.connect_timeout.is_some() {
                        global_llm.connect_timeout = llm.connect_timeout;
                    }
                    if llm.request_timeout.is_some() {
                        global_llm.request_timeout = llm.request_timeout;
                    }
                    if llm.json_mode.is_some() {
                        global_llm.json_mode = llm.json_mode;
                    }
                    if llm.prompt_caching.is_some() {
                        global_llm.prompt_caching = llm.prompt_caching;
                    }
                    if llm.reasoning_effort.is_some() {
                        global_llm.reasoning_effort = llm.reasoning_effort.clone();
                    }
                } else {
                    file_cfg.llm = Some(llm);
                }
            }
            if let Some(ws) = cfg.workspace {
                if let Some(ref mut global_ws) = file_cfg.workspace {
                    if ws.map_budget.is_some() {
                        global_ws.map_budget = ws.map_budget;
                    }
                    if ws.write_mode.is_some() {
                        global_ws.write_mode = ws.write_mode.clone();
                    }
                } else {
                    file_cfg.workspace = Some(ws);
                }
            }
            if let Some(mcp) = cfg.mcp_servers {
                if let Some(ref mut global_mcp) = file_cfg.mcp_servers {
                    for (k, v) in mcp {
                        global_mcp.insert(k, v);
                    }
                } else {
                    file_cfg.mcp_servers = Some(mcp);
                }
            }
        }

        let file_llm = file_cfg.llm.unwrap_or_default();
        let file_ws = file_cfg.workspace.unwrap_or_default();

        // ── LLM Configuration ──

        // Resolve API key with provider-aware fallback chain
        let env_api_key = env::var("OPENAI_API_KEY")
            .or_else(|_| env::var("ANTHROPIC_API_KEY"))
            .or_else(|_| env::var("DEEPSEEK_API_KEY"))
            .or_else(|_| env::var("CROW_API_KEY"))
            .ok();
        let api_key = env_api_key.or(file_llm.api_key);

        let env_base_url = env::var("LLM_BASE_URL").ok();
        let base_url = env_base_url.or(file_llm.base_url);

        let env_model = env::var("LLM_MODEL").ok();
        let model = env_model.or(file_llm.model);

        // Strict parsing for JSON mode
        let json_mode = match env::var("LLM_JSON_MODE") {
            Ok(v) => match v.to_lowercase().as_str() {
                "on" | "true" | "1" | "yes" => true,
                "off" | "false" | "0" | "no" => false,
                other => anyhow::bail!(
                    "Invalid LLM_JSON_MODE='{}'. Expected: on|off|true|false|1|0|yes|no",
                    other
                ),
            },
            Err(_) => file_llm.json_mode.unwrap_or(true),
        };

        // Resolve ProviderKind
        let provider_str = env::var("LLM_PROVIDER").ok().or(file_llm.provider);

        let default_base_url = "https://api.openai.com/v1".to_string();

        let (provider_kind, final_base_url, final_model) = match provider_str.as_deref() {
            Some("openai") | Some("openaicompatible") | Some("openai-compatible") => (
                ProviderKind::OpenAICompatible,
                base_url.unwrap_or_else(|| default_base_url.clone()),
                model.unwrap_or_else(|| "gpt-4-turbo".to_string()),
            ),
            // Well-known providers with sensible defaults
            Some("anthropic") | Some("claude") => (
                ProviderKind::Custom("anthropic".to_string()),
                base_url.unwrap_or_else(|| "https://api.anthropic.com/v1".to_string()),
                model.unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
            ),
            Some("ollama") => (
                ProviderKind::Custom("ollama".to_string()),
                base_url.unwrap_or_else(|| "http://localhost:11434/v1".to_string()),
                model.unwrap_or_else(|| "llama3".to_string()),
            ),
            Some("deepseek") => (
                ProviderKind::Custom("deepseek".to_string()),
                base_url.unwrap_or_else(|| "https://api.deepseek.com/v1".to_string()),
                model.unwrap_or_else(|| "deepseek-coder".to_string()),
            ),
            Some(other) => {
                let url = base_url.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Custom provider '{}' requires an explicitly set LLM_BASE_URL.",
                        other
                    )
                })?;
                let m = model.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Custom provider '{}' requires an explicitly set LLM_MODEL.",
                        other
                    )
                })?;
                (ProviderKind::Custom(other.to_string()), url, m)
            }
            None => {
                // If base_url is specified but not provider, treat as custom.
                if let Some(url) = base_url {
                    let m = model.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "LLM_BASE_URL is set to '{}' but model is not specified. \
                         Please set LLM_MODEL explicitly when using a custom provider.",
                            url
                        )
                    })?;
                    (ProviderKind::Custom("custom".into()), url, m)
                } else {
                    (
                        ProviderKind::OpenAICompatible,
                        default_base_url,
                        model.unwrap_or_else(|| "gpt-4-turbo".to_string()),
                    )
                }
            }
        };

        // Provider-aware API key validation
        let requires_api_key = match &provider_kind {
            ProviderKind::OpenAICompatible => true,
            ProviderKind::Custom(name) => {
                let lower = name.to_lowercase();
                // Ollama doesn't require an API key
                lower != "ollama"
            }
        };

        if requires_api_key && api_key.is_none() {
            let hint = match &provider_kind {
                ProviderKind::Custom(name) if name == "anthropic" => {
                    "Set ANTHROPIC_API_KEY or CROW_API_KEY."
                }
                ProviderKind::Custom(name) if name == "deepseek" => {
                    "Set DEEPSEEK_API_KEY or CROW_API_KEY."
                }
                _ => "Set OPENAI_API_KEY or CROW_API_KEY.",
            };
            anyhow::bail!("Missing API Key for {:?}. {}", provider_kind, hint);
        }

        let max_tokens = env::var("LLM_MAX_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_llm.max_tokens)
            .unwrap_or(8192);

        let connect_timeout_secs = env::var("LLM_CONNECT_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_llm.connect_timeout)
            .unwrap_or(10);

        let request_timeout_secs = env::var("LLM_REQUEST_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_llm.request_timeout)
            .unwrap_or(300);

        // Prompt caching (Anthropic-style cache_control markers).
        // This must be explicitly enabled, as injecting structured content blocks
        // is NOT supported by standard OpenAI-compatible endpoints.
        let prompt_caching = env::var("CROW_PROMPT_CACHE")
            .ok()
            .map(|v| matches!(v.to_lowercase().as_str(), "on" | "true" | "1" | "yes"))
            .or(file_llm.prompt_caching)
            .unwrap_or(false);

        // Reasoning effort for extended-thinking models (o1, o3, gpt-5.x).
        // Only sent when explicitly configured; validated against known values.
        let valid_efforts: &[&str] = &["low", "medium", "high", "xhigh"];
        let reasoning_effort = env::var("CROW_REASONING_EFFORT")
            .ok()
            .or(file_llm.reasoning_effort)
            .map(|e| e.to_lowercase());

        if let Some(ref effort) = reasoning_effort {
            if !valid_efforts.contains(&effort.as_str()) {
                anyhow::bail!(
                    "Invalid reasoning_effort='{}'. Expected one of: {}",
                    effort,
                    valid_efforts.join(", ")
                );
            }
        }

        let llm = LlmProviderConfig {
            provider_kind,
            api_key,
            model: final_model,
            base_url: final_base_url,
            max_tokens,
            connect_timeout_secs,
            request_timeout_secs,
            json_mode,
            prompt_caching,
            reasoning_effort,
        };

        // Clamp map_budget so it can never exceed the conversation manager's
        // system-message allocation. This avoids wasted tree-sitter work
        // generating a map that would just be truncated at init time.
        let map_budget = env::var("CROW_MAP_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_ws.map_budget)
            .unwrap_or(64 * 1024)
            .min(crate::budget::MAX_SYSTEM_BYTES);

        // ── Write Mode ──
        let write_mode = match env::var("CROW_WRITE_MODE") {
            Ok(v) => WriteMode::from_str_opt(&v).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid CROW_WRITE_MODE='{}'. Expected: sandbox|write|danger",
                    v
                )
            })?,
            Err(_) => file_ws
                .write_mode
                .as_deref()
                .and_then(WriteMode::from_str_opt)
                .unwrap_or(WriteMode::WorkspaceWrite), // Safe default
        };

        Ok(Self {
            workspace: workspace_dir.to_path_buf(),
            llm,
            map_budget,
            write_mode,
            mcp_servers: file_cfg.mcp_servers.unwrap_or_default(),
        })
    }

    /// Build an LLM client from this configuration.
    /// Routes to the correct provider implementation (OpenAI, Anthropic, Ollama, DeepSeek).
    pub fn build_llm_client(
        &self,
    ) -> Result<std::sync::Arc<dyn crow_brain::LlmClient>, BrainError> {
        crow_brain::build_client(&self.llm)
    }

    /// Human-readable description of the configured provider.
    pub fn describe_provider(&self) -> String {
        crow_brain::describe_provider(&self.llm)
    }

    /// Build a repo map from the workspace using the configured budget.
    pub fn build_repo_map(&self) -> Result<crow_intel::RepoMap, String> {
        self.build_repo_map_for(&self.workspace)
    }

    /// Build a repo map against an arbitrary root (e.g. a frozen sandbox).
    pub fn build_repo_map_for(&self, root: &Path) -> Result<crow_intel::RepoMap, String> {
        let walker = crow_intel::RepoWalker::new().with_max_bytes(self.map_budget);
        walker.build_repo_map(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_provider_fails_without_url_and_model() {
        // Clear env vars that might interfere
        env::remove_var("LLM_PROVIDER");
        env::remove_var("LLM_BASE_URL");
        env::remove_var("LLM_MODEL");

        env::set_var("LLM_PROVIDER", "my_custom_provider");
        // No BASE_URL or MODEL set => should fail fast
        let result = CrowConfig::load();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("requires an explicitly set LLM_BASE_URL"));

        env::set_var("LLM_BASE_URL", "http://localhost:11434/v1");
        // No MODEL set => should fail fast
        let result = CrowConfig::load();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("requires an explicitly set LLM_MODEL"));

        env::set_var("LLM_MODEL", "llama3");
        // Now it should pass config load (though it may fail missing API key for OpenAICompatible,
        // but custom doesn't require API key by default)

        // Ensure no local .crow config throws off test inside mock environment
        // Since we are running in the main workspace, it might pick up an openai key.
        // Clean up environment variables for isolation in real test suites.
        env::remove_var("LLM_PROVIDER");
        env::remove_var("LLM_BASE_URL");
        env::remove_var("LLM_MODEL");
    }
}
