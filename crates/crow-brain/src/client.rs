use crate::LlmClient;
use async_trait::async_trait;
use reqwest::{Client, header};
use serde_json::json;

pub struct ReqwestLlmClient {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
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
            .build()
            .map_err(|e| e.to_string())?;

        Ok(Self {
            client,
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
        })
    }
}

#[async_trait]
impl LlmClient for ReqwestLlmClient {
    async fn generate(&self, prompt: &str) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url);
        
        let body = json!({
            "model": self.model,
            "messages": [
                {
                    "role": "system",
                    "content": "You are the Intelligence Compiler. You must strictly output JSON matching the requested schema."
                },
                {
                    "role": "user",
                    "content": prompt
                }
            ],
            // For OpenAI compatible endpoints supporting explicit JSON mode:
            // "response_format": { "type": "json_object" }
        });

        let resp = self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("API error {}: {}", status, text));
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        
        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| "Missing or invalid response content".to_string())?;

        Ok(content.to_string())
    }
}
