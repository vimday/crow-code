//! Multi-provider routing.
//!
//! Constructs the appropriate LLM client based on `ProviderKind` and
//! configuration. This is crow's answer to the "which model?" question:
//! a single configuration point that routes to OpenAI, Anthropic, Ollama,
//! DeepSeek, or any OpenAI-compatible provider.

use crate::client::{BrainError, LlmProviderConfig, ProviderKind, ReqwestLlmClient};
use crate::LlmClient;
use std::sync::Arc;

/// Well-known provider identifiers (case-insensitive matching).
const ANTHROPIC_ALIASES: &[&str] = &["anthropic", "claude"];
const OLLAMA_ALIASES: &[&str] = &["ollama"];
const DEEPSEEK_ALIASES: &[&str] = &["deepseek"];

/// Construct the correct LLM client for the given provider configuration.
///
/// Routing logic:
/// - `OpenAICompatible` → `ReqwestLlmClient` (chat/completions)
/// - `Custom("anthropic")` / `Custom("claude")` → `AnthropicClient` (messages API)
/// - `Custom("ollama")` → `ReqwestLlmClient` with Ollama defaults
/// - `Custom("deepseek")` → `ReqwestLlmClient` with DeepSeek defaults
/// - `Custom(_)` → `ReqwestLlmClient` (assume OpenAI-compatible)
pub fn build_client(config: &LlmProviderConfig) -> Result<Arc<dyn LlmClient>, BrainError> {
    match &config.provider_kind {
        ProviderKind::OpenAICompatible => {
            let client = ReqwestLlmClient::from_config(config)?;
            Ok(Arc::new(client))
        }
        ProviderKind::Custom(name) => {
            let lower = name.to_lowercase();

            if ANTHROPIC_ALIASES.iter().any(|a| *a == lower) {
                // Anthropic has a fundamentally different API format.
                let anthropic = crate::anthropic::AnthropicClient::from_config(config)?;
                Ok(Arc::new(anthropic))
            } else if OLLAMA_ALIASES.iter().any(|a| *a == lower) {
                // Ollama is OpenAI-compatible but typically runs on localhost.
                // Apply Ollama-specific defaults if not explicitly set.
                let mut ollama_config = config.clone();
                if ollama_config.base_url.is_empty()
                    || ollama_config.base_url == "https://api.openai.com/v1"
                {
                    ollama_config.base_url = "http://localhost:11434/v1".to_string();
                }
                // Ollama doesn't need an API key
                if ollama_config.api_key.is_none() {
                    ollama_config.api_key = Some("ollama".to_string());
                }
                // Longer timeout for local inference
                if ollama_config.request_timeout_secs == 300 {
                    ollama_config.request_timeout_secs = 600;
                }
                let client = ReqwestLlmClient::from_config(&ollama_config)?;
                Ok(Arc::new(client))
            } else if DEEPSEEK_ALIASES.iter().any(|a| *a == lower) {
                // DeepSeek is OpenAI-compatible with a different endpoint.
                let mut ds_config = config.clone();
                if ds_config.base_url.is_empty()
                    || ds_config.base_url == "https://api.openai.com/v1"
                {
                    ds_config.base_url = "https://api.deepseek.com/v1".to_string();
                }
                let client = ReqwestLlmClient::from_config(&ds_config)?;
                Ok(Arc::new(client))
            } else {
                // Unknown custom provider — try OpenAI-compatible format.
                let client = ReqwestLlmClient::from_config(config)?;
                Ok(Arc::new(client))
            }
        }
    }
}

/// Pretty-print the resolved provider configuration for the user.
pub fn describe_provider(config: &LlmProviderConfig) -> String {
    let kind = match &config.provider_kind {
        ProviderKind::OpenAICompatible => "OpenAI-compatible".to_string(),
        ProviderKind::Custom(name) => {
            let lower = name.to_lowercase();
            if ANTHROPIC_ALIASES.iter().any(|a| *a == lower) {
                "Anthropic (Claude Messages API)".to_string()
            } else if OLLAMA_ALIASES.iter().any(|a| *a == lower) {
                "Ollama (local inference)".to_string()
            } else if DEEPSEEK_ALIASES.iter().any(|a| *a == lower) {
                "DeepSeek".to_string()
            } else {
                format!("Custom ({name})")
            }
        }
    };

    format!(
        "{} | {} | {}",
        kind,
        config.model,
        if config.prompt_caching {
            "cache=on"
        } else {
            "cache=off"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> LlmProviderConfig {
        LlmProviderConfig {
            provider_kind: ProviderKind::OpenAICompatible,
            api_key: Some("test-key".into()),
            model: "gpt-4-turbo".into(),
            base_url: "https://api.openai.com/v1".into(),
            max_tokens: 8192,
            connect_timeout_secs: 10,
            request_timeout_secs: 300,
            json_mode: false,
            prompt_caching: false,
            reasoning_effort: None,
        }
    }

    #[test]
    fn routes_openai_compatible() {
        let config = base_config();
        let client = build_client(&config);
        assert!(client.is_ok());
    }

    #[test]
    fn routes_anthropic() {
        let mut config = base_config();
        config.provider_kind = ProviderKind::Custom("anthropic".into());
        config.base_url = "https://api.anthropic.com/v1".into();
        config.model = "claude-sonnet-4-20250514".into();
        let client = build_client(&config);
        assert!(client.is_ok());
    }

    #[test]
    fn routes_ollama_with_defaults() {
        let mut config = base_config();
        config.provider_kind = ProviderKind::Custom("ollama".into());
        config.model = "llama3".into();
        // base_url defaults should be applied
        let client = build_client(&config);
        assert!(client.is_ok());
    }

    #[test]
    fn routes_deepseek() {
        let mut config = base_config();
        config.provider_kind = ProviderKind::Custom("deepseek".into());
        config.model = "deepseek-coder".into();
        let client = build_client(&config);
        assert!(client.is_ok());
    }

    #[test]
    fn routes_unknown_custom_as_openai() {
        let mut config = base_config();
        config.provider_kind = ProviderKind::Custom("my-custom-provider".into());
        config.base_url = "https://my-api.example.com/v1".into();
        let client = build_client(&config);
        assert!(client.is_ok());
    }

    #[test]
    fn describe_anthropic() {
        let mut config = base_config();
        config.provider_kind = ProviderKind::Custom("claude".into());
        config.model = "claude-sonnet-4-20250514".into();
        let desc = describe_provider(&config);
        assert!(desc.contains("Anthropic"), "got: {desc}");
        assert!(desc.contains("claude-sonnet-4-20250514"), "got: {desc}");
    }

    #[test]
    fn describe_ollama() {
        let mut config = base_config();
        config.provider_kind = ProviderKind::Custom("ollama".into());
        config.model = "llama3".into();
        let desc = describe_provider(&config);
        assert!(desc.contains("Ollama"), "got: {desc}");
    }
}
