use crate::LlmClient;
use async_trait::async_trait;
use reqwest::{Client, header};
use serde_json::json;

pub struct ReqwestLlmClient {
    client: Client,
    #[allow(dead_code)]
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
}

impl ReqwestLlmClient {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Result<Self, String> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|e| e.to_string())?,
        );

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;

        Ok(Self {
            client,
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            max_tokens: 2048,
        })
    }

    pub fn with_max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = max;
        self
    }
}

#[async_trait]
impl LlmClient for ReqwestLlmClient {
    async fn generate(&self, prompt: &str) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url);
        
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [
                {
                    "role": "system",
                    "content": "You are the Intelligence Compiler. You must output ONLY valid JSON matching the requested schema. No markdown, no explanation, just pure JSON."
                },
                {
                    "role": "user",
                    "content": prompt
                }
            ]
        });

        // Only inject response_format for providers known to support it
        if self.base_url.contains("openai.com") {
            body["response_format"] = json!({ "type": "json_object" });
        }

        let resp = self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        let status = resp.status();
        let raw_text = resp.text().await.map_err(|e| format!("Failed to read response body: {}", e))?;

        if !status.is_success() {
            return Err(format!("API error {}: {}", status, raw_text));
        }

        let trimmed = raw_text.trim();
        let data: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| format!("Failed to parse API response as JSON: {} — raw: {}", e, &trimmed[..trimmed.len().min(500)]))?;
        
        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| format!("Missing content in response: {}", &trimmed[..trimmed.len().min(500)]))?;

        Ok(content.to_string())
    }
}
