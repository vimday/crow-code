import sys

file_path = "crates/crow-brain/src/anthropic.rs"
with open(file_path, "r") as f:
    content = f.read()

streaming_impl = """
    async fn _generate_streaming(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f64>,
        observer: &mut dyn crate::compiler::StreamObserver,
    ) -> Result<String, BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{}/messages", base);

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
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::System => unreachable!(),
            };

            if last_role == Some(role) {
                if let Some(last) = conversation.last_mut() {
                    if let Some(content) = last["content"].as_str() {
                        last["content"] = serde_json::json!(format!("{}\\n\\n{}", content, msg.content));
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

        let resp = self.client.post(&url).json(&body).send().await.map_err(BrainError::Transport)?;
        let status = resp.status();
        if !status.is_success() {
            let raw_text = resp.text().await.unwrap_or_default();
            return Err(BrainError::ApiError { status: status.as_u16(), body: raw_text });
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
                    return Err(BrainError::Config(format!("Stream error: {}", e)));
                }
            }
        }

        if full_text.is_empty() {
            return Err(BrainError::MissingField("Empty stream response".into()));
        }

        Ok(full_text)
    }
}
"""

content = content.replace("        Ok(content)\n    }\n}\n\n#[async_trait]\nimpl LlmClient for AnthropicClient {", "        Ok(content)\n    }\n" + streaming_impl + "\n#[async_trait]\nimpl LlmClient for AnthropicClient {")

trait_streaming_impl = """
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
"""

content = content.replace("    async fn generate_with_temperature(\n        &self,\n        messages: &[ChatMessage],\n        temperature: f64,\n    ) -> Result<String, BrainError> {\n        self._generate(messages, Some(temperature)).await\n    }\n}", trait_streaming_impl)

with open(file_path, "w") as f:
    f.write(content)

print("Applied anthropic patch")
