//! Anthropic Messages API client.
//!
//! Anthropic's Claude uses a different API format from OpenAI:
//! - Endpoint: `/v1/messages` (not `/chat/completions`)
//! - Auth header: `x-api-key` (not `Authorization: Bearer`)
//! - Body: `{ model, max_tokens, system, messages }` (system is a top-level field)
//! - Response: `{ content: [{ type: "text", text: "..." }] }` (not choices array)
//!
//! This client handles these differences transparently behind the `LlmClient` trait.

use crate::{AgentResponse, AgentResponseBlock, ChatMessage, ChatRole, LlmClient, ToolCallRequest};
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

    /// Build the Anthropic conversation array from `ChatMessage`s.
    ///
    /// When `native_tools` is true, assistant messages with `tool_calls` are
    /// emitted as content-block arrays (text + tool_use blocks) and tool-result
    /// messages use `tool_result` content blocks instead of plain text.
    fn build_conversation(
        &self,
        messages: &[ChatMessage],
        native_tools: bool,
    ) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
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
            if self.prompt_caching && i == system_messages.len() - 1 {
                block["cache_control"] = json!({"type": "ephemeral"});
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

            if native_tools {
                // ── Native tool calling format ─────────────────────
                match msg.role {
                    ChatRole::Assistant => {
                        // Build content blocks: text + tool_use blocks
                        let mut content_blocks: Vec<serde_json::Value> = Vec::new();
                        if !msg.content.is_empty() {
                            content_blocks.push(json!({
                                "type": "text",
                                "text": msg.content
                            }));
                        }
                        if let Some(ref tcs) = msg.tool_calls {
                            for tc in tcs {
                                content_blocks.push(json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.name,
                                    "input": tc.arguments
                                }));
                            }
                        }
                        if content_blocks.is_empty() {
                            content_blocks.push(json!({"type": "text", "text": ""}));
                        }
                        // Anthropic requires alternation; merge if same role
                        if last_role == Some("assistant") {
                            if let Some(last) = conversation.last_mut() {
                                if let Some(arr) = last["content"].as_array_mut() {
                                    arr.extend(content_blocks);
                                }
                            }
                        } else {
                            conversation.push(json!({
                                "role": "assistant",
                                "content": content_blocks
                            }));
                        }
                        last_role = Some("assistant");
                    }
                    ChatRole::Tool => {
                        // tool_result content block
                        let tool_result = json!({
                            "type": "tool_result",
                            "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                            "content": msg.content
                        });
                        if last_role == Some("user") {
                            if let Some(last) = conversation.last_mut() {
                                if let Some(arr) = last["content"].as_array_mut() {
                                    arr.push(tool_result);
                                } else {
                                    // Previous user message was a plain string;
                                    // upgrade to content-block array.
                                    let prev_text =
                                        last["content"].as_str().unwrap_or("").to_string();
                                    last["content"] = json!([
                                        {"type": "text", "text": prev_text},
                                        tool_result
                                    ]);
                                }
                            }
                        } else {
                            conversation.push(json!({
                                "role": "user",
                                "content": [tool_result]
                            }));
                        }
                        last_role = Some("user");
                    }
                    ChatRole::User => {
                        if last_role == Some("user") {
                            if let Some(last) = conversation.last_mut() {
                                if let Some(arr) = last["content"].as_array_mut() {
                                    arr.push(json!({"type": "text", "text": msg.content}));
                                } else if let Some(prev) = last["content"].as_str() {
                                    last["content"] = json!(format!("{}\n\n{}", prev, msg.content));
                                }
                            }
                        } else {
                            conversation.push(json!({
                                "role": "user",
                                "content": msg.content
                            }));
                        }
                        last_role = Some("user");
                    }
                    ChatRole::System => unreachable!(),
                }
            } else {
                // ── Legacy text-only format ────────────────────────
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
        }

        if conversation.is_empty() {
            conversation.push(json!({
                "role": "user",
                "content": "Please proceed with the task."
            }));
        }

        (system_parts, conversation)
    }

    async fn _generate(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f64>,
    ) -> Result<String, BrainError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/messages");

        let (system_parts, conversation) = self.build_conversation(messages, false);

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
    ) -> Result<(String, crate::usage::TokenUsage), BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/messages");

        let (system_parts, conversation) = self.build_conversation(messages, false);

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
        let mut usage = crate::usage::TokenUsage::default();

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
                            } else if ty == "message_start" {
                                // Initial usage from message_start event
                                if let Some(u) = data.get("message").and_then(|m| m.get("usage")) {
                                    usage.prompt_tokens = u
                                        .get("input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
                                    usage.completion_tokens = u
                                        .get("output_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
                                    usage.cache_creation_input_tokens = u
                                        .get("cache_creation_input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
                                    usage.cache_read_input_tokens = u
                                        .get("cache_read_input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
                                }
                            } else if ty == "message_delta" {
                                // Final output token count from message_delta event
                                if let Some(u) = data.get("usage") {
                                    usage.completion_tokens = u
                                        .get("output_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0)
                                        as u32;
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

        Ok((full_text, usage))
    }
    /// Convert OpenAI-format tool definitions to Anthropic format.
    ///
    /// OpenAI: `{ type: "function", function: { name, description, parameters } }`
    /// Anthropic: `{ name, description, input_schema }`
    fn convert_tools_to_anthropic(tools: &[serde_json::Value]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                let name = func.get("name")?.as_str()?;
                let description = func
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("");
                let input_schema = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

                Some(json!({
                    "name": name,
                    "description": description,
                    "input_schema": input_schema
                }))
            })
            .collect()
    }

    async fn _generate_streaming_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        mut observer: Option<&mut dyn crate::compiler::ToolStreamObserver>,
    ) -> Result<AgentResponse, BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/messages");

        let (system_parts, conversation) = self.build_conversation(messages, true);

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": conversation,
            "stream": true
        });

        if !system_parts.is_empty() {
            body["system"] = json!(system_parts);
        }

        // Convert and attach tools
        if !tools.is_empty() {
            let anthropic_tools = Self::convert_tools_to_anthropic(tools);
            body["tools"] = json!(anthropic_tools);
        }

        // Retry loop
        let mut retries = 0;
        let max_retries = 3;
        let mut delay_ms: u64 = 1000;

        let resp = loop {
            match self.client.post(&url).json(&body).send().await {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        break r;
                    }
                    let code = status.as_u16();
                    if [429, 500, 502, 503, 529].contains(&code) && retries < max_retries {
                        retries += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2;
                        continue;
                    }
                    let raw_text = r.text().await.unwrap_or_default();
                    return Err(BrainError::ApiError {
                        status: code,
                        body: raw_text,
                    });
                }
                Err(e) => {
                    if retries < max_retries {
                        retries += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2;
                        continue;
                    }
                    return Err(BrainError::Transport(e));
                }
            }
        };

        let mut stream = resp.bytes_stream().eventsource();
        let mut full_text = String::new();
        let mut usage = crate::usage::TokenUsage::default();

        // Tool call accumulation: indexed by content_block index
        // Each entry: (id, name, arguments_json_buffer)
        let mut tool_calls: std::collections::HashMap<u64, (String, String, String)> =
            std::collections::HashMap::new();
        // Track the current content block index for delta routing
        let mut current_block_index: u64 = 0;

        while let Some(event_res) = stream.next().await {
            match event_res {
                Ok(event) => {
                    let data_str = event.data;
                    if data_str == "[DONE]" {
                        break;
                    }

                    let data = match serde_json::from_str::<serde_json::Value>(&data_str) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                    let event_type = match data.get("type").and_then(|t| t.as_str()) {
                        Some(t) => t,
                        None => continue,
                    };

                    match event_type {
                        "content_block_start" => {
                            // Track the block index
                            if let Some(idx) = data.get("index").and_then(serde_json::Value::as_u64)
                            {
                                current_block_index = idx;
                            }
                            // Check if it's a tool_use block
                            if let Some(content_block) = data.get("content_block") {
                                if content_block.get("type").and_then(|t| t.as_str())
                                    == Some("tool_use")
                                {
                                    let id = content_block
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let name = content_block
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string();

                                    if let Some(ref mut obs) = observer {
                                        obs.on_tool_call_start(&id, &name);
                                    }

                                    tool_calls
                                        .insert(current_block_index, (id, name, String::new()));
                                }
                            }
                        }
                        "content_block_delta" => {
                            if let Some(idx) = data.get("index").and_then(serde_json::Value::as_u64)
                            {
                                current_block_index = idx;
                            }
                            if let Some(delta) = data.get("delta") {
                                let delta_type =
                                    delta.get("type").and_then(|t| t.as_str()).unwrap_or("");

                                match delta_type {
                                    "text_delta" => {
                                        if let Some(text) =
                                            delta.get("text").and_then(|t| t.as_str())
                                        {
                                            if !text.is_empty() {
                                                full_text.push_str(text);
                                                if let Some(ref mut obs) = observer {
                                                    obs.on_text_chunk(text);
                                                }
                                            }
                                        }
                                    }
                                    "input_json_delta" => {
                                        if let Some(partial_json) =
                                            delta.get("partial_json").and_then(|p| p.as_str())
                                        {
                                            if let Some(entry) =
                                                tool_calls.get_mut(&current_block_index)
                                            {
                                                entry.2.push_str(partial_json);
                                                if let Some(ref mut obs) = observer {
                                                    obs.on_tool_call_args_chunk(
                                                        &entry.0,
                                                        partial_json,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    _ => {} // ignore thinking_delta, etc.
                                }
                            }
                        }
                        "message_start" => {
                            // Capture initial usage from message_start event
                            if let Some(u) = data.get("message").and_then(|m| m.get("usage")) {
                                usage.prompt_tokens =
                                    u.get("input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0) as u32;
                                usage.completion_tokens =
                                    u.get("output_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0) as u32;
                                usage.cache_creation_input_tokens =
                                    u.get("cache_creation_input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0) as u32;
                                usage.cache_read_input_tokens =
                                    u.get("cache_read_input_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0) as u32;
                            }
                        }
                        "message_delta" => {
                            // Final output token count from message_delta event
                            if let Some(u) = data.get("usage") {
                                usage.completion_tokens =
                                    u.get("output_tokens")
                                        .and_then(serde_json::Value::as_u64)
                                        .unwrap_or(0) as u32;
                            }
                        }
                        "message_stop" => break,
                        _ => {} // content_block_stop, ping, etc.
                    }
                }
                Err(e) => {
                    return Err(BrainError::Config(format!("Stream error: {e}")));
                }
            }
        }

        // Build the response blocks
        let mut blocks = Vec::new();
        if !full_text.is_empty() {
            blocks.push(AgentResponseBlock::Text(full_text));
        }

        // Sort tool calls by block index and add to response
        let mut sorted_calls: Vec<(u64, (String, String, String))> =
            tool_calls.into_iter().collect();
        sorted_calls.sort_by_key(|(idx, _)| *idx);

        for (_, (id, name, args_str)) in sorted_calls {
            let arguments: serde_json::Value = serde_json::from_str(&args_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(AgentResponseBlock::ToolCall(ToolCallRequest {
                id,
                name,
                arguments,
            }));
        }

        if blocks.is_empty() {
            return Err(BrainError::MissingField(
                "Empty stream response with no tool calls".into(),
            ));
        }

        Ok(AgentResponse {
            blocks,
            usage: Some(usage),
        })
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
            let (text, _usage) = self._generate_streaming(messages, temperature, obs).await?;
            Ok(text)
        } else {
            self._generate(messages, temperature).await
        }
    }

    async fn generate_streaming_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        observer: Option<&mut dyn crate::compiler::ToolStreamObserver>,
    ) -> Result<AgentResponse, BrainError> {
        self._generate_streaming_with_tools(messages, tools, observer)
            .await
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
