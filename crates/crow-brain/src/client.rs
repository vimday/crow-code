use crate::LlmClient;
use async_trait::async_trait;
use reqwest::{header, Client};
use serde_json::json;

// ─── Provider Capabilities ──────────────────────────────────────────

/// Declares what a given LLM provider endpoint supports.
/// Resolved via explicit configuration (`LLM_JSON_MODE`) first;
/// falls back to URL heuristics only when no override is given.
#[derive(Debug, Clone)]
pub struct ProviderCaps {
    /// Whether the provider supports `response_format: { "type": "json_object" }`.
    pub supports_json_mode: bool,
}

impl ProviderCaps {
    /// Resolve capabilities from an explicit override or URL fallback.
    ///
    /// - `json_mode_override = Some(true/false)` → use that value directly.
    /// - `json_mode_override = None` → fall back to URL heuristic (conservative).
    pub fn resolve(json_mode_override: Option<bool>, base_url: &str) -> Self {
        let supports_json_mode =
            json_mode_override.unwrap_or_else(|| base_url.contains("openai.com"));
        Self { supports_json_mode }
    }

    /// Explicitly declare capabilities (for testing / direct construction).
    pub fn new(supports_json_mode: bool) -> Self {
        Self { supports_json_mode }
    }
}

// ─── Unified LLM Configuration ─────────────────────────────────────

/// All parameters needed to construct an LLM client.
/// Centralises API key, model, URL, timeouts, and capability flags
/// so they are no longer scattered across multiple call sites.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Bearer token. `None` for local/unauthenticated providers.
    pub api_key: Option<String>,
    /// Model identifier (e.g. `gpt-4-turbo`, `claude-3.5-sonnet`).
    pub model: String,
    /// Base URL for the chat completions endpoint.
    pub base_url: String,
    /// Maximum tokens the model may generate in a single response.
    pub max_tokens: u32,
    /// TCP connect timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Full request timeout in seconds (includes generation time).
    pub request_timeout_secs: u64,
    /// Explicit JSON mode override. `None` = auto-detect from URL.
    pub json_mode: Option<bool>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: "gpt-4-turbo".into(),
            base_url: "https://api.openai.com/v1".into(),
            max_tokens: 8192,
            connect_timeout_secs: 10,
            request_timeout_secs: 300,
            json_mode: None,
        }
    }
}

// ─── Client ─────────────────────────────────────────────────────────

pub struct ReqwestLlmClient {
    client: Client,
    model: String,
    base_url: String,
    max_tokens: u32,
    caps: ProviderCaps,
}

impl ReqwestLlmClient {
    /// Construct from a unified `LlmConfig`.
    pub fn from_config(config: &LlmConfig) -> Result<Self, String> {
        let mut headers = header::HeaderMap::new();

        // Only attach Authorization header when an API key is provided.
        // This allows local/unauthenticated providers to work without
        // requiring a dummy key.
        if let Some(ref key) = config.api_key {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Bearer {}", key))
                    .map_err(|e| e.to_string())?,
            );
        }

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(config.connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|e| e.to_string())?;

        let caps = ProviderCaps::resolve(config.json_mode, &config.base_url);

        Ok(Self {
            client,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            max_tokens: config.max_tokens,
            caps,
        })
    }
}

#[async_trait]
impl LlmClient for ReqwestLlmClient {
    async fn generate(&self, messages: &[crate::ChatMessage]) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url);

        // Map ChatMessage structs directly into the API messages array.
        // The caller (IntentCompiler) is responsible for providing the
        // system prompt — the client is a pure transport layer.
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": m.content }))
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": api_messages
        });

        if self.caps.supports_json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        let status = resp.status();
        let raw_text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?;

        if !status.is_success() {
            return Err(format!("API error {}: {}", status, raw_text));
        }

        let trimmed = raw_text.trim();
        let data: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
            format!(
                "Failed to parse API response as JSON: {} — raw: {}",
                e,
                &trimmed[..trimmed.len().min(500)]
            )
        })?;

        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                format!(
                    "Missing content in response: {}",
                    &trimmed[..trimmed.len().min(500)]
                )
            })?;

        Ok(content.to_string())
    }
}
