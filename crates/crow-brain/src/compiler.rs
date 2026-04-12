use crow_patch::IntentPlan;
use serde_json::Error as SerdeError;
use async_trait::async_trait;

#[derive(Debug)]
pub enum CompilerError {
    PromptFailed(String),
    MaxRetriesExceeded(Vec<SerdeError>),
}

/// A generic LLM driver interface to allow substituting e.g. OpenAI vs Claude vs Mock.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a prompt and a system prompt/JSON schema instruction to the LLM,
    /// and get strings back.
    async fn generate(&self, prompt: &str) -> Result<String, String>;
}

/// The Intelligence Compiler.
/// Translates natural language directives into strictly validated IntentPlans.
pub struct IntentCompiler {
    client: Box<dyn LlmClient>,
    max_retries: usize,
}

impl IntentCompiler {
    pub fn new(client: Box<dyn LlmClient>) -> Self {
        Self {
            client,
            max_retries: 3,
        }
    }

    pub fn with_max_retries(mut self, retries: usize) -> Self {
        self.max_retries = retries;
        self
    }

    /// Compiles a task directive into a strict `IntentPlan`.
    /// Employs a self-healing loop: if the LLM output violates the `IntentPlan`
    /// schema, it catches the parsing error and prompts the LLM to fix it,
    /// up to `max_retries` times.
    pub async fn compile(&self, base_task: &str) -> Result<IntentPlan, CompilerError> {
        let mut current_prompt = format!(
            "Task:\n{}\n\nOutput ONLY a valid JSON object matching the IntentPlan schema.",
            base_task
        );
        
        let mut errors = Vec::new();

        for _attempt in 0..=self.max_retries {
            let response = self.client.generate(&current_prompt).await.map_err(CompilerError::PromptFailed)?;
            
            // Try to parse the json directly (using a liberal extraction if the model wrapped it in markdown)
            let cleaned_json = extract_json_block(&response);

            match serde_json::from_str::<IntentPlan>(cleaned_json) {
                Ok(plan) => return Ok(plan),
                Err(e) => {
                    // Self-healing: capture the exact Serde error
                    current_prompt = format!(
                        "Your previous JSON output was invalid.\n\nError:\n{}\n\nPrevious Output:\n{}\n\nPlease fix the JSON to strictly conform to the schema.",
                        e, cleaned_json
                    );
                    errors.push(e);
                }
            }
        }

        Err(CompilerError::MaxRetriesExceeded(errors))
    }
}

/// Helper to strip ```json ... ``` wrappers from LLM output.
fn extract_json_block(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with("```json") {
        let after_fence = &trimmed["```json".len()..];
        if let Some(end) = after_fence.rfind("```") {
            return after_fence[..end].trim();
        }
    }
    // Fallback: strip generic markdown block if no language specifier
    if trimmed.starts_with("```") {
        let after_fence = &trimmed["```".len()..];
        if let Some(end) = after_fence.rfind("```") {
            return after_fence[..end].trim();
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockLlm {
        responses: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn generate(&self, _prompt: &str) -> Result<String, String> {
            let mut resps = self.responses.lock().unwrap();
            if resps.is_empty() {
                Err("No more mock responses".into())
            } else {
                Ok(resps.remove(0))
            }
        }
    }

    fn valid_plan_json() -> String {
        r#"{
            "base_snapshot_id": "snap-1",
            "rationale": "mock",
            "is_partial": false,
            "confidence": "High",
            "operations": [{
                "Create": {
                    "path": "test.txt",
                    "content": "hello",
                    "precondition": "MustNotExist"
                }
            }]
        }"#.into()
    }

    #[tokio::test]
    async fn compiler_succeeds_first_try() {
        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![valid_plan_json()])),
        });
        let compiler = IntentCompiler::new(client);

        let plan = compiler.compile("do something").await.expect("compile should succeed");
        assert_eq!(plan.rationale, "mock");
    }

    #[tokio::test]
    async fn compiler_self_heals_on_serde_error() {
        // First response misses a comma or is junk
        let bad_json = r#"{ "invalid": yes }"#.into();
        
        let client = Box::new(MockLlm {
            // First response is bad_json, forcing the loop to catch Err and retry
            // The second response is valid_plan_json, so the loop succeeds.
            responses: Arc::new(Mutex::new(vec![bad_json, valid_plan_json()])),
        });
        let compiler = IntentCompiler::new(client);

        let plan = compiler.compile("fix bug").await.expect("compile should heal and succeed");
        assert_eq!(plan.rationale, "mock");
    }

    #[tokio::test]
    async fn compiler_fails_after_max_retries() {
        let bad_json = String::from(r#"{ "still_bad": true }"#);
        
        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![
                bad_json.clone(), bad_json.clone(), bad_json.clone(), bad_json.clone()
            ])),
        });
        let compiler = IntentCompiler::new(client).with_max_retries(2);

        let err = compiler.compile("do things").await.unwrap_err();
        match err {
            CompilerError::MaxRetriesExceeded(errors) => {
                assert_eq!(errors.len(), 3); // initial + 2 retries
            }
            _ => panic!("Expected max retries error"),
        }
    }
}
