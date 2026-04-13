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
        })
    }
}

#[async_trait]
impl LlmClient for ReqwestLlmClient {
    async fn generate(&self, messages: &[crate::ChatMessage]) -> Result<String, BrainError> {
        let url = format!("{}/chat/completions", self.base_url);

        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": m.content }))
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": api_messages
        });

        if self.json_mode {
            body["response_format"] = json!({ "type": "json_object" });
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
                raw: trimmed[..trimmed.len().min(500)].to_string(),
            })?;

        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                BrainError::MissingField(trimmed[..trimmed.len().min(500)].to_string())
            })?;

        Ok(content.to_string())
    }
}
