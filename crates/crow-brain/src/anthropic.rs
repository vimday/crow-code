//! Anthropic Messages API client.
//!
//! Anthropic's Claude uses a different API format from OpenAI:
//! - Endpoint: `/v1/messages` (not `/chat/completions`)
//! - Auth header: `x-api-key` (not `Authorization: Bearer`)
//! - Body: `{ model, max_tokens, system, messages }` (system is a top-level field)
//! - Response: `{ content: [{ type: "text", text: "..." }] }` (not choices array)
//!
//! This client handles these differences transparently behind the `LlmClient` trait.

use crate::{ChatMessage, ChatRole, LlmClient};
use async_trait::async_trait;
use reqwest::{header, Client};
use serde_json::json;

use crate::client::BrainError;

pub struct AnthropicClient {
    client: Client,
    model: String,
    base_url: String,
    max_tokens: u32,
    prompt_caching: bool,
}

impl AnthropicClient {
    pub fn from_config(config: &crate::client::LlmProviderConfig) -> Result<Self, BrainError> {
        let mut headers = header::HeaderMap::new();

        // Anthropic uses x-api-key, not Authorization Bearer
        if let Some(ref key) = config.api_key {
            headers.insert(
                "x-api-key",
                header::HeaderValue::from_str(key)
                    .map_err(|e| BrainError::Config(e.to_string()))?,
            );
        }

        // Required version header
        headers.insert(
            "anthropic-version",
            header::HeaderValue::from_static("2023-06-01"),
        );

        // Enable prompt caching beta header if configured
        if config.prompt_caching {
            headers.insert(
                "anthropic-beta",
                header::HeaderValue::from_static("prompt-caching-2024-07-31"),
            );
        }

        let client = Client::builder()
            // Avoid OS proxy auto-discovery here. On some sandboxed macOS
            // environments the system proxy lookup path can panic inside
            // `system-configuration`, which would take down both tests and
            // runtime client construction.
            .no_proxy()
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
            prompt_caching: config.prompt_caching,
        })
    }

    async fn _generate(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f64>,
    ) -> Result<String, BrainError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/messages");

        // Anthropic separates system messages from the conversation.
        // All system messages are concatenated into a single top-level `system` field.
        let mut system_parts: Vec<serde_json::Value> = Vec::new();
        let mut conversation: Vec<serde_json::Value> = Vec::new();

        let system_messages: Vec<&ChatMessage> = messages
            .iter()
            .filter(|m| m.role == ChatRole::System)
            .collect();

        for (i, msg) in system_messages.iter().enumerate() {
            let mut block = json!({
                "type": "text",
                "text": msg.content
            });
            // Cache breakpoint on the last system message
            if self.prompt_caching && i == system_messages.len() - 1 {
                block["cache_control"] = json!({"type": "ephemeral"});
            }
            system_parts.push(block);
        }

        // Non-system messages become the conversation array.
        // Anthropic requires alternating user/assistant messages.
        // We merge consecutive same-role messages.
        let mut last_role: Option<&str> = None;
        for msg in messages {
            if msg.role == ChatRole::System {
                continue;
            }

            let role = match msg.role {
                ChatRole::User | ChatRole::Tool => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::System => unreachable!(),
            };

            // For tool results, format with the tool call ID
            let content = if msg.role == ChatRole::Tool {
                if let Some(ref tc_id) = msg.tool_call_id {
                    format!("[Tool Result ({tc_id})]\n{}", msg.content)
                } else {
                    msg.content.clone()
                }
            } else {
                msg.content.clone()
            };

            if last_role == Some(role) {
                // Merge with previous message
                if let Some(last) = conversation.last_mut() {
                    if let Some(prev_content) = last["content"].as_str() {
                        last["content"] = json!(format!("{}\n\n{}", prev_content, content));
                    }
                }
            } else {
                conversation.push(json!({
                    "role": role,
                    "content": content
                }));
                last_role = Some(role);
            }
        }

        // Anthropic requires at least one user message
        if conversation.is_empty() {
            conversation.push(json!({
                "role": "user",
                "content": "Please proceed with the task."
            }));
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": conversation
        });

        // Set system as structured content blocks (enables caching)
        if !system_parts.is_empty() {
            body["system"] = json!(system_parts);
        }

        if let Some(temp) = temperature {
            body["temperature"] = json!(temp);
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

        let data: serde_json::Value =
            serde_json::from_str(raw_text.trim()).map_err(|e| BrainError::ParseError {
                err: e,
                raw: crow_patch::safe_truncate(raw_text.trim(), 500).to_string(),
            })?;

        // Anthropic response format: { content: [{ type: "text", text: "..." }] }
        let content = data["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|block| block["text"].as_str())
            .unwrap_or("")
            .to_string();

        if content.is_empty() {
            return Err(BrainError::MissingField(
                crow_patch::safe_truncate(raw_text.trim(), 500).to_string(),
            ));
        }

        Ok(content)
    }

    async fn _generate_streaming(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f64>,
        observer: &mut dyn crate::compiler::StreamObserver,
    ) -> Result<String, BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/messages");

        let mut system_parts: Vec<serde_json::Value> = Vec::new();
        let mut conversation: Vec<serde_json::Value> = Vec::new();

        let system_messages: Vec<&ChatMessage> = messages
            .iter()
            .filter(|m| m.role == ChatRole::System)
            .collect();

        for (i, msg) in system_messages.iter().enumerate() {
            let mut block = serde_json::json!({
                "type": "text",
                "text": msg.content
            });
            if self.prompt_caching && i == system_messages.len() - 1 {
                block["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }
            system_parts.push(block);
        }

        let mut last_role: Option<&str> = None;
        for msg in messages {
            if msg.role == ChatRole::System {
                continue;
            }

            let role = match msg.role {
                ChatRole::User | ChatRole::Tool => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::System => unreachable!(),
            };

            if last_role == Some(role) {
                if let Some(last) = conversation.last_mut() {
                    if let Some(content) = last["content"].as_str() {
                        last["content"] =
                            serde_json::json!(format!("{}\n\n{}", content, msg.content));
                    }
                }
            } else {
                conversation.push(serde_json::json!({
                    "role": role,
                    "content": msg.content
                }));
                last_role = Some(role);
            }
        }

        if conversation.is_empty() {
            conversation.push(serde_json::json!({
                "role": "user",
                "content": "Please proceed with the task."
            }));
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": conversation,
            "stream": true
        });

        if !system_parts.is_empty() {
            body["system"] = serde_json::json!(system_parts);
        }

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(BrainError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let raw_text = resp.text().await.unwrap_or_default();
            return Err(BrainError::ApiError {
                status: status.as_u16(),
                body: raw_text,
            });
        }

        let mut stream = resp.bytes_stream().eventsource();
        let mut full_text = String::new();

        while let Some(event_res) = stream.next().await {
            match event_res {
                Ok(event) => {
                    let data_str = event.data;
                    if data_str == "[DONE]" {
                        break;
                    }
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&data_str) {
                        // Anthropic SSE events:
                        // type: "content_block_delta" -> delta: { "type": "text_delta", "text": "..." }
                        if let Some(ty) = data.get("type").and_then(|t| t.as_str()) {
                            if ty == "content_block_delta" {
                                if let Some(delta) = data.get("delta") {
                                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            full_text.push_str(text);
                                            observer.on_chunk(text);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(BrainError::Config(format!("Stream error: {e}")));
                }
            }
        }

        if full_text.is_empty() {
            return Err(BrainError::MissingField("Empty stream response".into()));
        }

        Ok(full_text)
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn generate(&self, messages: &[ChatMessage]) -> Result<String, BrainError> {
        self._generate(messages, None).await
    }

    async fn generate_with_temperature(
        &self,
        messages: &[ChatMessage],
        temperature: f64,
    ) -> Result<String, BrainError> {
        self._generate(messages, Some(temperature)).await
    }

    async fn generate_streaming(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f64>,
        observer: Option<&mut dyn crate::compiler::StreamObserver>,
    ) -> Result<String, BrainError> {
        if let Some(obs) = observer {
            self._generate_streaming(messages, temperature, obs).await
        } else {
            self._generate(messages, temperature).await
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::client::LlmProviderConfig;

    #[test]
    fn anthropic_client_constructs_from_config() {
        let config = LlmProviderConfig {
            provider_kind: crate::client::ProviderKind::Custom("anthropic".into()),
            api_key: Some("test-key".into()),
            model: "claude-sonnet-4-20250514".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            max_tokens: 8192,
            connect_timeout_secs: 10,
            request_timeout_secs: 300,
            json_mode: false,
            prompt_caching: true,
            reasoning_effort: None,
        };

        let client = AnthropicClient::from_config(&config);
        assert!(client.is_ok());
        let c = client.unwrap();
        assert_eq!(c.model, "claude-sonnet-4-20250514");
        assert_eq!(c.max_tokens, 8192);
        assert!(c.prompt_caching);
    }
}
