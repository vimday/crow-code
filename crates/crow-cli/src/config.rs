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
    pub llm: crow_brain::LlmConfig,
    pub map_budget: usize,
}

impl CliConfig {
    /// Resolve configuration from environment.
    /// Fails fast with a clear message if configuration values are invalid.
    pub fn from_env() -> anyhow::Result<Self> {
        // API key: optional when a custom base URL is set (allows local providers).
        let api_key = env::var("OPENAI_API_KEY")
            .or_else(|_| env::var("CROW_API_KEY"))
            .ok();

        // Strict parsing for JSON mode: unrecognized values fail fast.
        let json_mode = match env::var("LLM_JSON_MODE") {
            Ok(v) => {
                let parsed = match v.to_lowercase().as_str() {
                    "on" | "true" | "1" | "yes" => true,
                    "off" | "false" | "0" | "no" => false,
                    other => {
                        anyhow::bail!(
                            "Invalid LLM_JSON_MODE='{}'. Expected: on|off|true|false|1|0|yes|no",
                            other
                        )
                    }
                };
                Some(parsed)
            }
            Err(_) => None,
        };

        let base_url = env::var("LLM_BASE_URL").ok();

        // When a custom base URL is set, require an explicit model to avoid
        // accidentally sending "gpt-4-turbo" to a non-OpenAI endpoint.
        // API key is NOT required for custom providers (local/unauthenticated).
        let model = match (&base_url, env::var("LLM_MODEL")) {
            (_, Ok(m)) => m,
            (None, Err(_)) => "gpt-4-turbo".to_string(),
            (Some(url), Err(_)) => {
                anyhow::bail!(
                    "LLM_BASE_URL is set to '{}' but LLM_MODEL is not specified. \
                     Please set LLM_MODEL explicitly when using a custom provider.",
                    url
                )
            }
        };

        // When using the default OpenAI endpoint, API key is still required.
        if base_url.is_none() && api_key.is_none() {
            anyhow::bail!(
                "Missing API Key. Please set OPENAI_API_KEY or CROW_API_KEY. \
                 (API key is only optional when LLM_BASE_URL points to a local/unauthenticated provider.)"
            );
        }

        let llm = crow_brain::LlmConfig {
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            max_tokens: env::var("LLM_MAX_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8192),
            connect_timeout_secs: env::var("LLM_CONNECT_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            request_timeout_secs: env::var("LLM_REQUEST_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            json_mode,
        };

        Ok(Self {
            workspace: env::current_dir()?,
            llm,
            map_budget: env::var("CROW_MAP_BUDGET")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500 * 1024),
        })
    }

    /// Build an LLM client from this configuration.
    pub fn build_llm_client(&self) -> Result<crow_brain::ReqwestLlmClient, String> {
        crow_brain::ReqwestLlmClient::from_config(&self.llm)
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
