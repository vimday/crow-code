import sys

file_path = "crates/crow-brain/src/client.rs"
with open(file_path, "r") as f:
    content = f.read()

streaming_impl = """
    async fn _generate_streaming(
        &self,
        messages: &[crate::ChatMessage],
        temperature: Option<f64>,
        observer: &mut dyn crate::compiler::StreamObserver,
    ) -> Result<String, BrainError> {
        use eventsource_stream::Eventsource;
        use futures_util::StreamExt;

        let base = self.base_url.trim_end_matches('/');
        let url = format!("{}/chat/completions", base);

        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": api_messages,
            "stream": true
        });

        if let Some(effort) = &self.reasoning_effort {
            let model_lower = self.model.to_lowercase();
            let supports_reasoning = model_lower.starts_with("o1")
                || model_lower.starts_with("o3")
                || model_lower.starts_with("o4")
                || model_lower.starts_with("gpt-5");
            if supports_reasoning {
                body["reasoning_effort"] = serde_json::json!(effort);
            }
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
                        if let Some(choices) = data.get("choices").and_then(|c| c.as_array()) {
                            if let Some(choice) = choices.first() {
                                if let Some(delta) = choice.get("delta") {
                                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                        if !content.is_empty() {
                                            full_text.push_str(content);
                                            observer.on_chunk(content);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(BrainError::Transport(reqwest::Error::from(e)));
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

content = content.replace("    }\n}\n\n#[async_trait]\nimpl LlmClient for ReqwestLlmClient {", streaming_impl + "\n#[async_trait]\nimpl LlmClient for ReqwestLlmClient {")

trait_streaming_impl = """
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
}
"""

content = content.replace("    async fn generate_with_temperature(\n        &self,\n        messages: &[crate::ChatMessage],\n        temperature: f64,\n    ) -> Result<String, BrainError> {\n        self._generate(messages, Some(temperature)).await\n    }\n}", trait_streaming_impl)

with open(file_path, "w") as f:
    f.write(content)

print("Applied client patch")
