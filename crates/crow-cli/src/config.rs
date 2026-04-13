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
}

impl CliConfig {
    /// Resolve configuration from environment.
    /// Fails fast with a clear message if required variables are missing.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let api_key = env::var("OPENAI_API_KEY")
            .or_else(|_| env::var("CROW_API_KEY"))
            .map_err(|_| "Missing API Key. Please set OPENAI_API_KEY or CROW_API_KEY.")?;

        Ok(Self {
            workspace: env::current_dir()?,
            api_key,
            model: env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4-turbo".to_string()),
            base_url: env::var("LLM_BASE_URL").ok(),
            max_tokens: env::var("LLM_MAX_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8192),
            map_budget: env::var("CROW_MAP_BUDGET")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500 * 1024),
        })
    }

    /// Build an LLM client from this configuration.
    pub fn build_llm_client(&self) -> Result<crow_brain::ReqwestLlmClient, String> {
        crow_brain::ReqwestLlmClient::new(
            self.api_key.clone(),
            self.model.clone(),
            self.base_url.clone(),
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
