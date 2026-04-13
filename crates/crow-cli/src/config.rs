//! Shared CLI configuration resolved from environment variables.
//!
//! Single source of truth for all runtime knobs. Every command
//! (compile, dry-run, future Darwin loop) reads from this struct
//! instead of scattering `env::var` calls across orchestration code.

use std::env;
use std::path::PathBuf;

/// All environment-driven configuration for a crow session.
pub struct CliConfig {
    pub workspace: PathBuf,
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub max_tokens: u32,
    pub map_budget: usize,
    /// Explicit override for provider JSON mode capability.
    /// Set via `LLM_JSON_MODE=on|off`. `None` = auto-detect from URL.
    pub json_mode: Option<bool>,
}

impl CliConfig {
    /// Resolve configuration from environment.
    /// Fails fast with a clear message if required variables are missing
    /// or if configuration values are invalid.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let api_key = env::var("OPENAI_API_KEY")
            .or_else(|_| env::var("CROW_API_KEY"))
            .map_err(|_| "Missing API Key. Please set OPENAI_API_KEY or CROW_API_KEY.")?;

        // Strict parsing: only recognized values are accepted.
        let json_mode = match env::var("LLM_JSON_MODE") {
            Ok(v) => {
                let parsed = match v.to_lowercase().as_str() {
                    "on" | "true" | "1" | "yes" => true,
                    "off" | "false" | "0" | "no" => false,
                    other => {
                        return Err(format!(
                            "Invalid LLM_JSON_MODE='{}'. Expected: on|off|true|false|1|0|yes|no",
                            other
                        )
                        .into())
                    }
                };
                Some(parsed)
            }
            Err(_) => None,
        };

        let base_url = env::var("LLM_BASE_URL").ok();

        // When a custom base URL is set, require an explicit model to avoid
        // accidentally sending "gpt-4-turbo" to a non-OpenAI endpoint.
        let model = match (&base_url, env::var("LLM_MODEL")) {
            (_, Ok(m)) => m,
            (None, Err(_)) => "gpt-4-turbo".to_string(),
            (Some(url), Err(_)) => {
                return Err(format!(
                    "LLM_BASE_URL is set to '{}' but LLM_MODEL is not specified. \
                     Please set LLM_MODEL explicitly when using a custom provider.",
                    url
                )
                .into())
            }
        };

        Ok(Self {
            workspace: env::current_dir()?,
            api_key,
            model,
            base_url,
            max_tokens: env::var("LLM_MAX_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8192),
            map_budget: env::var("CROW_MAP_BUDGET")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500 * 1024),
            json_mode,
        })
    }

    /// Build an LLM client from this configuration.
    pub fn build_llm_client(&self) -> Result<crow_brain::ReqwestLlmClient, String> {
        crow_brain::ReqwestLlmClient::new(
            self.api_key.clone(),
            self.model.clone(),
            self.base_url.clone(),
            self.json_mode,
        )
        .map(|c| c.with_max_tokens(self.max_tokens))
    }

    /// Build a repo map from the workspace using the configured budget.
    pub fn build_repo_map(&self) -> Result<crow_intel::RepoMap, String> {
        self.build_repo_map_for(&self.workspace)
    }

    /// Build a repo map against an arbitrary root (e.g. a frozen sandbox).
    pub fn build_repo_map_for(
        &self,
        root: &std::path::Path,
    ) -> Result<crow_intel::RepoMap, String> {
        let walker = crow_intel::RepoWalker::new().with_max_bytes(self.map_budget);
        walker.build_repo_map(root)
    }
}
