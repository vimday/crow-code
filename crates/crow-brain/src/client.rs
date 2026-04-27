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

impl BrainError {
    /// Returns true if this error is likely transient and the request should be retried.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::ApiError { status, .. } => [429, 500, 502, 503, 529].contains(status),
            Self::Config(_) | Self::ParseError { .. } | Self::MissingField(_) => false,
        }
    }

    /// Returns true if this is an authentication/authorization error.
    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::ApiError { status, .. } if *status == 401 || *status == 403)
    }

    /// Returns the HTTP status code, if applicable.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            Self::ApiError { status, .. } => Some(*status),
            _ => None,
        }
    }
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
    pub reasoning_effort: Option<String>,
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
            json_mode: true,
            prompt_caching: false,
            reasoning_effort: None,
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
    reasoning_effort: Option<String>,
}

impl ReqwestLlmClient {
    /// Construct from a unified `LlmProviderConfig`.
    pub fn from_config(config: &LlmProviderConfig) -> Result<Self, BrainError> {
        let mut headers = header::HeaderMap::new();

        if let Some(ref key) = config.api_key {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| BrainError::Config(e.to_string()))?,
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
            json_mode: config.json_mode,
            prompt_caching: config.prompt_caching,
            reasoning_effort: config.reasoning_effort.clone(),
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
        let url = format!("{base}/chat/completions");

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

        // Only inject reasoning_effort for models known to support it.
        // Sending this to Ollama, DeepSeek, or older OpenAI models causes 400s.
        if let Some(effort) = &self.reasoning_effort {
            let model_lower = self.model.to_lowercase();
            let supports_reasoning = model_lower.starts_with("o1")
                || model_lower.starts_with("o3")
                || model_lower.starts_with("o4")
                || model_lower.starts_with("gpt-5");
            if supports_reasoning {
                body["reasoning_effort"] = json!(effort);
            }
        }

        if let Some(temp) = temperature {
            body["temperature"] = json!(temp);
        }

        if self.json_mode {
            body["tools"] = json!([{
                "type": "function",
                "function": {
                    "name": "agent_action",
                    "description": "Perform an agent action: read_files, recon, submit_plan, or delegate_task. The 'action' field discriminates the type.",
                    "parameters": openai_agent_action_schema(),
                    "strict": false
                }
            }]);
            body["tool_choice"] = json!("required");
        }

        let mut retries = 0;
        let max_retries = 5;
        let mut delay_ms = 1000;
        let mut raw_text = String::new();
        let mut final_status = 0;
        let mut last_error: Option<reqwest::Error> = None;

        loop {
            match self.client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    match resp.text().await {
                        Ok(text) => {
                            final_status = status.as_u16();
                            raw_text = text;

                            if status.is_success()
                                || ![429, 500, 502, 503, 529].contains(&final_status)
                            {
                                break; // Not a transient error, move on
                            } else {
                                println!(
                                    "    ⚠️ API returned transient error ({final_status}). Retrying..."
                                );
                            }
                        }
                        Err(e) => {
                            println!("    ⚠️ API stream text read failed (IncompleteMessage?): {e}. Retrying...");
                            final_status = status.as_u16();
                            last_error = Some(e);
                        }
                    }
                }
                Err(e) => {
                    println!("    ⚠️ API connection transport failed: {e}. Retrying...");
                    last_error = Some(e);
                }
            }

            if retries >= max_retries {
                if let Some(err) = last_error {
                    return Err(BrainError::Transport(err));
                } else {
                    return Err(BrainError::ApiError {
                        status: final_status,
                        body: format!(
                            "Network/transient errors maxed out. Last content: {raw_text}"
                        ),
                    });
                }
            }

            retries += 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            delay_ms *= 2;
        }

        if !(200..300).contains(&final_status) {
            return Err(BrainError::ApiError {
                status: final_status,
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

    async fn _generate_streaming(
        &self,
        messages: &[crate::ChatMessage],
        temperature: Option<f64>,
        observer: &mut dyn crate::compiler::StreamObserver,
    ) -> Result<String, BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/chat/completions");

        // Build message array with prompt caching support (matching non-streaming path)
        let api_messages: Vec<serde_json::Value> = if self.prompt_caching {
            let last_sys_idx = messages
                .iter()
                .rposition(|m| m.role == crate::ChatRole::System);
            messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    if m.role == crate::ChatRole::System {
                        let mut block = json!({"type": "text", "text": m.content});
                        if Some(i) == last_sys_idx {
                            block["cache_control"] = json!({"type": "ephemeral"});
                        }
                        json!({"role": "system", "content": [block]})
                    } else {
                        json!({"role": m.role, "content": m.content})
                    }
                })
                .collect()
        } else {
            messages
                .iter()
                .map(|m| json!({"role": m.role, "content": m.content}))
                .collect()
        };

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": api_messages,
            "stream": true
        });

        // Inject tools/tool_choice for structured output (CRITICAL: was missing before)
        if self.json_mode {
            body["tools"] = json!([{
                "type": "function",
                "function": {
                    "name": "agent_action",
                    "description": "Perform an agent action: read_files, recon, submit_plan, or delegate_task. The 'action' field discriminates the type.",
                    "parameters": openai_agent_action_schema(),
                    "strict": false
                }
            }]);
            body["tool_choice"] = json!("required");
        }

        if let Some(effort) = &self.reasoning_effort {
            let model_lower = self.model.to_lowercase();
            let supports_reasoning = model_lower.starts_with("o1")
                || model_lower.starts_with("o3")
                || model_lower.starts_with("o4")
                || model_lower.starts_with("gpt-5");
            if supports_reasoning {
                body["reasoning_effort"] = json!(effort);
            }
        }

        if let Some(temp) = temperature {
            body["temperature"] = json!(temp);
        }

        // Retry logic matching non-streaming path
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

        while let Some(event_res) = stream.next().await {
            match event_res {
                Ok(event) => {
                    let data_str = event.data;
                    if data_str == "[DONE]" {
                        break;
                    }
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&data_str) {
                        if let Some(choices) = data.get("choices").and_then(|c| c.as_array()) {
                            if let Some(choice) = choices.first() {
                                if let Some(delta) = choice.get("delta") {
                                    let mut chunk_str = "";
                                    if self.json_mode {
                                        if let Some(tcs) =
                                            delta.get("tool_calls").and_then(|t| t.as_array())
                                        {
                                            if let Some(tc) = tcs.first() {
                                                if let Some(fn_obj) = tc.get("function") {
                                                    if let Some(args) = fn_obj
                                                        .get("arguments")
                                                        .and_then(|a| a.as_str())
                                                    {
                                                        chunk_str = args;
                                                    }
                                                }
                                            }
                                        } else if let Some(content) =
                                            delta.get("content").and_then(|c| c.as_str())
                                        {
                                            chunk_str = content;
                                        }
                                    } else if let Some(content) =
                                        delta.get("content").and_then(|c| c.as_str())
                                    {
                                        chunk_str = content;
                                    }
                                    if !chunk_str.is_empty() {
                                        full_text.push_str(chunk_str);
                                        // Isolate observer from panics: a buggy
                                        // TUI handler must not kill the LLM stream.
                                        let chunk_owned = chunk_str.to_string();
                                        let obs_ref = &mut *observer;
                                        let _ = std::panic::catch_unwind(
                                            std::panic::AssertUnwindSafe(|| {
                                                obs_ref.on_chunk(&chunk_owned);
                                            }),
                                        );
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

    async fn generate_streaming(
        &self,
        messages: &[crate::ChatMessage],
        temperature: Option<f64>,
        observer: Option<&mut dyn crate::compiler::StreamObserver>,
    ) -> Result<String, BrainError> {
        if let Some(obs) = observer {
            self._generate_streaming(messages, temperature, obs).await
        } else {
            self._generate(messages, temperature).await
        }
    }

    async fn generate_streaming_with_tools(
        &self,
        messages: &[crate::ChatMessage],
        tools: &[serde_json::Value],
        mut observer: Option<&mut dyn crate::compiler::ToolStreamObserver>,
    ) -> Result<crate::AgentResponse, BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/chat/completions");

        // Build message array with proper tool message support
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let mut msg = if self.prompt_caching && m.role == crate::ChatRole::System {
                    let block = json!({"type": "text", "text": m.content});
                    json!({"role": "system", "content": [block]})
                } else {
                    json!({"role": m.role, "content": m.content})
                };

                // Add tool_call_id for tool result messages
                if let Some(ref tc_id) = m.tool_call_id {
                    msg["tool_call_id"] = json!(tc_id);
                }

                // Add tool_calls for assistant messages that requested them
                if let Some(ref tcs) = m.tool_calls {
                    let tc_array: Vec<serde_json::Value> = tcs
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments.to_string()
                                }
                            })
                        })
                        .collect();
                    msg["tool_calls"] = json!(tc_array);
                }

                msg
            })
            .collect();

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": api_messages,
            "stream": true
        });

        // Include tool definitions
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        if let Some(effort) = &self.reasoning_effort {
            let model_lower = self.model.to_lowercase();
            let supports_reasoning = model_lower.starts_with("o1")
                || model_lower.starts_with("o3")
                || model_lower.starts_with("o4")
                || model_lower.starts_with("gpt-5");
            if supports_reasoning {
                body["reasoning_effort"] = json!(effort);
            }
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

        // Tool call accumulation state
        // Map from index to (id, name, arguments_buffer)
        let mut tool_calls: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new();

        while let Some(event_res) = stream.next().await {
            match event_res {
                Ok(event) => {
                    let data_str = event.data;
                    if data_str == "[DONE]" {
                        break;
                    }
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&data_str) {
                        if let Some(choices) = data.get("choices").and_then(|c| c.as_array()) {
                            if let Some(choice) = choices.first() {
                                if let Some(delta) = choice.get("delta") {
                                    // Handle text content deltas
                                    if let Some(content) =
                                        delta.get("content").and_then(|c| c.as_str())
                                    {
                                        if !content.is_empty() {
                                            full_text.push_str(content);
                                            if let Some(ref mut obs) = observer {
                                                obs.on_text_chunk(content);
                                            }
                                        }
                                    }

                                    // Handle tool call deltas
                                    if let Some(tcs) =
                                        delta.get("tool_calls").and_then(|t| t.as_array())
                                    {
                                        for tc in tcs {
                                            let index = tc
                                                .get("index")
                                                .and_then(serde_json::Value::as_u64)
                                                .unwrap_or(0)
                                                as u32;

                                            // New tool call start
                                            if let Some(id) = tc.get("id").and_then(|i| i.as_str())
                                            {
                                                let name = tc
                                                    .get("function")
                                                    .and_then(|f| f.get("name"))
                                                    .and_then(|n| n.as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                tool_calls.insert(
                                                    index,
                                                    (id.to_string(), name.clone(), String::new()),
                                                );
                                                if let Some(ref mut obs) = observer {
                                                    obs.on_tool_call_start(id, &name);
                                                }
                                            }

                                            // Tool call arguments delta
                                            if let Some(args_chunk) = tc
                                                .get("function")
                                                .and_then(|f| f.get("arguments"))
                                                .and_then(|a| a.as_str())
                                            {
                                                if let Some(entry) = tool_calls.get_mut(&index) {
                                                    entry.2.push_str(args_chunk);
                                                    if let Some(ref mut obs) = observer {
                                                        obs.on_tool_call_args_chunk(
                                                            &entry.0, args_chunk,
                                                        );
                                                    }
                                                }
                                            }
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

        // Build the response
        let mut blocks = Vec::new();
        if !full_text.is_empty() {
            blocks.push(crate::AgentResponseBlock::Text(full_text));
        }

        // Sort tool calls by index and add to response
        let mut sorted_calls: Vec<(u32, (String, String, String))> =
            tool_calls.into_iter().collect();
        sorted_calls.sort_by_key(|(idx, _)| *idx);

        for (_, (id, name, args_str)) in sorted_calls {
            let arguments: serde_json::Value = serde_json::from_str(&args_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            blocks.push(crate::AgentResponseBlock::ToolCall(
                crate::ToolCallRequest {
                    id,
                    name,
                    arguments,
                },
            ));
        }

        if blocks.is_empty() {
            return Err(BrainError::MissingField(
                "Empty stream response with no tool calls".into(),
            ));
        }

        Ok(crate::AgentResponse { blocks })
    }
}

/// Build an OpenAI-compatible function parameter schema for AgentAction.
///
/// OpenAI requires `type: "object"` at the root of function parameters.
/// `schemars::schema_for!(AgentAction)` generates a `oneOf` root for tagged enums,
/// which OpenAI rejects with "schema must be a JSON Schema of 'type: \"object\"'".
///
/// We use a permissive schema (`additionalProperties: true`, `strict: false`)
/// so the model can freely use any of the action variants. The actual validation
/// happens at deserialization time via serde, not at the schema level.
fn openai_agent_action_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["read_files", "recon", "submit_plan", "delegate_task"],
                "description": "The action type to perform."
            },
            "paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "File paths for read_files action."
            },
            "rationale": {
                "type": "string",
                "description": "Reason for performing this action."
            },
            "tool": {
                "type": "string",
                "enum": ["list_dir", "search", "file_info", "word_count", "dir_tree"],
                "description": "Reconnaissance tool to use (for recon action)."
            },
            "pattern": {
                "type": "string",
                "description": "Search pattern (for recon search tool)."
            },
            "path": {
                "type": "string",
                "description": "Path for recon tools."
            },
            "glob": {
                "type": "string",
                "description": "Glob filter (for recon search tool)."
            },
            "max_depth": {
                "type": "integer",
                "description": "Max depth for dir_tree."
            },
            "plan": {
                "type": "object",
                "description": "The IntentPlan object (for submit_plan action).",
                "additionalProperties": true
            },
            "task": {
                "type": "string",
                "description": "Task description (for delegate_task action)."
            },
            "focus_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths to focus on (for delegate_task action)."
            }
        },
        "required": ["action"],
        "additionalProperties": true
    })
}
