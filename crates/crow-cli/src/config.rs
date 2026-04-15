//! Shared CLI configuration resolved from project file and environment variables.
//!
//! Priority: Environment variables > `.crow/config.json` > defaults.

use crow_brain::{BrainError, LlmProviderConfig, ProviderKind, ReqwestLlmClient};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// ─── File-based configuration shapes ───────────────────────────

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    llm: Option<LlmConfigFile>,
    workspace: Option<WorkspaceConfigFile>,
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
}

#[derive(Debug, Deserialize, Default)]
struct WorkspaceConfigFile {
    map_budget: Option<usize>,
}

// ─── Runtime configuration ──────────────────────────────────────

/// All configuration for a crow session.
#[derive(Debug)]
pub struct CrowConfig {
    pub workspace: PathBuf,
    pub llm: LlmProviderConfig,
    pub map_budget: usize,
}

impl CrowConfig {
    /// Resolve configuration from project file and environment.
    /// Fails fast with a clear message if configuration values are invalid.
    pub fn load() -> anyhow::Result<Self> {
        let workspace_dir = env::current_dir()?;
        let config_path = workspace_dir.join(".crow").join("config.json");

        let file_cfg = if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            serde_json::from_str::<ConfigFile>(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse .crow/config.json: {}", e))?
        } else {
            ConfigFile::default()
        };

        let file_llm = file_cfg.llm.unwrap_or_default();
        let file_ws = file_cfg.workspace.unwrap_or_default();

        // ── LLM Configuration ──

        let env_api_key = env::var("OPENAI_API_KEY")
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
            Err(_) => file_llm.json_mode.unwrap_or(false),
        };

        // Resolve ProviderKind
        let provider_str = env::var("LLM_PROVIDER").ok().or(file_llm.provider);

        let default_base_url = "https://api.openai.com/v1".to_string();

        let (provider_kind, final_base_url, final_model) = match provider_str.as_deref() {
            Some("openai") | Some("openaicompatible") => (
                ProviderKind::OpenAICompatible,
                base_url.unwrap_or_else(|| default_base_url.clone()),
                model.unwrap_or_else(|| "gpt-4-turbo".to_string()),
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

        if matches!(provider_kind, ProviderKind::OpenAICompatible) && api_key.is_none() {
            anyhow::bail!(
                "Missing API Key for {:?}. Please set OPENAI_API_KEY or CROW_API_KEY. \
                 (API key is only optional when using a custom provider with explicitly set base URL.)",
                 provider_kind
            );
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
        // Default: enabled for non-OpenAI providers (they benefit most).
        let prompt_caching = match env::var("CROW_PROMPT_CACHE") {
            Ok(v) => matches!(v.to_lowercase().as_str(), "on" | "true" | "1" | "yes"),
            Err(_) => {
                // Auto-enable for Anthropic-compatible endpoints
                // (base_url contains "anthropic" or provider is custom)
                !matches!(provider_kind, ProviderKind::OpenAICompatible)
            }
        };

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
        };

        // Clamp map_budget so it can never exceed the conversation manager's
        // system-message allocation. This avoids wasted tree-sitter work
        // generating a map that would just be truncated at init time.
        let map_budget = env::var("CROW_MAP_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_ws.map_budget)
            .unwrap_or(500 * 1024)
            .min(crate::budget::MAX_SYSTEM_BYTES);

        Ok(Self {
            workspace: workspace_dir,
            llm,
            map_budget,
        })
    }

    /// Build an LLM client from this configuration.
    pub fn build_llm_client(&self) -> Result<ReqwestLlmClient, BrainError> {
        ReqwestLlmClient::from_config(&self.llm)
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
