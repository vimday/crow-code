use crate::LlmClient;
use async_trait::async_trait;
use reqwest::{header, Client};
use serde_json::json;

// ─── Errors ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BrainError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("API error {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("Failed to parse API response as JSON: {err} — raw: {raw}")]
    ParseError { err: serde_json::Error, raw: String },
    #[error("Missing expected field in response: {0}")]
    MissingField(String),
}

// ─── Provider Capabilities & Configuration ─────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAICompatible,
    Custom(String), // A custom identifier
}

/// All parameters needed to construct an LLM client.
/// Centralises API key, model, URL, timeouts, and explicit capability flags.
#[derive(Debug, Clone)]
pub struct LlmProviderConfig {
    pub provider_kind: ProviderKind,
    pub api_key: Option<String>,
    pub model: String,
    pub base_url: String,
    pub max_tokens: u32,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    /// Whether this provider requires and supports strict JSON object output.
    pub json_mode: bool,
    /// Whether to inject Anthropic-style `cache_control` markers on system
    /// messages for prompt caching. When enabled, system messages use
    /// structured content blocks and the last system message gets a
    /// `cache_control: {"type": "ephemeral"}` breakpoint.
    pub prompt_caching: bool,
}

impl Default for LlmProviderConfig {
    fn default() -> Self {
        Self {
            provider_kind: ProviderKind::OpenAICompatible,
            api_key: None,
            model: "gpt-4-turbo".into(),
            base_url: "https://api.openai.com/v1".into(),
            max_tokens: 8192,
            connect_timeout_secs: 10,
            request_timeout_secs: 300,
            json_mode: false,
            prompt_caching: false,
        }
    }
}

// ─── Client ─────────────────────────────────────────────────────────

pub struct ReqwestLlmClient {
    client: Client,
    model: String,
    base_url: String,
    max_tokens: u32,
    json_mode: bool,
    prompt_caching: bool,
}

impl ReqwestLlmClient {
    /// Construct from a unified `LlmProviderConfig`.
    pub fn from_config(config: &LlmProviderConfig) -> Result<Self, BrainError> {
        let mut headers = header::HeaderMap::new();

        if let Some(ref key) = config.api_key {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Bearer {}", key))
                    .map_err(|e| BrainError::Config(e.to_string()))?,
            );
        }

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(config.connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
            .build()
            .map_err(|e| BrainError::Config(e.to_string()))?;

        Ok(Self {
            client,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            max_tokens: config.max_tokens,
            json_mode: config.json_mode,
            prompt_caching: config.prompt_caching,
        })
    }
}

fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    crow_patch::safe_truncate(s, max_bytes)
}

impl ReqwestLlmClient {
    /// Core generation logic shared by both `generate()` and `generate_with_temperature()`.
    async fn _generate(
        &self,
        messages: &[crate::ChatMessage],
        temperature: Option<f64>,
    ) -> Result<String, BrainError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{}/chat/completions", base);

        // Build message array, optionally with Anthropic-style prompt caching.
        // When prompt_caching is enabled, system messages use structured content
        // blocks and the LAST system message gets a cache_control breakpoint.
        let api_messages: Vec<serde_json::Value> = if self.prompt_caching {
            // Find the index of the last system message for the cache breakpoint.
            let last_sys_idx = messages
                .iter()
                .rposition(|m| m.role == crate::ChatRole::System);

            messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    if m.role == crate::ChatRole::System {
                        // System messages use structured content blocks.
                        let mut block = json!({
                            "type": "text",
                            "text": m.content
                        });
                        // Only the last system message gets the cache breakpoint.
                        if Some(i) == last_sys_idx {
                            block["cache_control"] = json!({"type": "ephemeral"});
                        }
                        json!({ "role": "system", "content": [block] })
                    } else {
                        json!({ "role": m.role, "content": m.content })
                    }
                })
                .collect()
        } else {
            messages
                .iter()
                .map(|m| json!({ "role": m.role, "content": m.content }))
                .collect()
        };

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": api_messages
        });

        if let Some(temp) = temperature {
            body["temperature"] = json!(temp);
        }

        if self.json_mode {
            let schema = schemars::schema_for!(crow_patch::AgentAction);
            body["tools"] = json!([{
                "type": "function",
                "function": {
                    "name": "agent_action",
                    "description": "Perform an action in the repository.",
                    "parameters": schema
                }
            }]);
            body["tool_choice"] = json!({
                "type": "function",
                "function": { "name": "agent_action" }
            });
        }

        let resp = self.client.post(&url).json(&body).send().await?;

        let status = resp.status();
        let raw_text = resp.text().await?;

        if !status.is_success() {
            return Err(BrainError::ApiError {
                status: status.as_u16(),
                body: raw_text,
            });
        }

        let trimmed = raw_text.trim();

        let data: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|e| BrainError::ParseError {
                err: e,
                raw: safe_truncate(trimmed, 500).to_string(),
            })?;

        let message = &data["choices"][0]["message"];

        let content = if self.json_mode {
            if let Some(tool_calls) = message["tool_calls"].as_array() {
                if let Some(call) = tool_calls.first() {
                    call["function"]["arguments"]
                        .as_str()
                        .unwrap_or("")
                        .to_string()
                } else {
                    message["content"].as_str().unwrap_or("").to_string()
                }
            } else {
                // Fallback in case the model ignored tool_choice
                message["content"].as_str().unwrap_or("").to_string()
            }
        } else {
            message["content"].as_str().unwrap_or("").to_string()
        };

        if content.is_empty() {
            return Err(BrainError::MissingField(
                safe_truncate(trimmed, 500).to_string(),
            ));
        }

        Ok(content)
    }
}

#[async_trait]
impl LlmClient for ReqwestLlmClient {
    async fn generate(&self, messages: &[crate::ChatMessage]) -> Result<String, BrainError> {
        self._generate(messages, None).await
    }

    async fn generate_with_temperature(
        &self,
        messages: &[crate::ChatMessage],
        temperature: f64,
    ) -> Result<String, BrainError> {
        self._generate(messages, Some(temperature)).await
    }
}
