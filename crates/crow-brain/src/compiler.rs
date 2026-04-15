use async_trait::async_trait;
use serde::Serialize;
use serde_json::Error as SerdeError;

// ─── Chat Message Protocol ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// A structured chat message with role-content separation.
/// Replaces raw string concatenation for LLM context management.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

// ─── Compiler Types ─────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CompilerError {
    #[error("Prompt failed: {0}")]
    PromptFailed(#[from] crate::client::BrainError),
    #[error("Max retries exceeded: {0:?}")]
    MaxRetriesExceeded(Vec<SerdeError>),
}

/// A generic LLM driver interface to allow substituting e.g. OpenAI vs Claude vs Mock.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a structured conversation to the LLM and get the assistant's response.
    async fn generate(&self, messages: &[ChatMessage])
        -> Result<String, crate::client::BrainError>;

    /// Generate with an explicit temperature for diversity (used by MCTS).
    /// Default implementation ignores temperature and delegates to `generate()`.
    async fn generate_with_temperature(
        &self,
        messages: &[ChatMessage],
        _temperature: f64,
    ) -> Result<String, crate::client::BrainError> {
        self.generate(messages).await
    }
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

    /// Compiles a task directive into a strict `AgentAction`.
    /// Employs a self-healing loop: if the LLM output violates the schema,
    /// it catches the parsing error and prompts the LLM to fix it.
    pub async fn compile_action(
        &self,
        messages: &[ChatMessage],
    ) -> Result<crow_patch::AgentAction, CompilerError> {
        let schema_guide = crate::schema::intent_plan_schema();
        let mut conversation: Vec<ChatMessage> = Vec::new();

        // Always inject the schema guide into the system prompt.
        // - When json_mode=true (native tool calling): the transport sends
        //   the formal tool schema; this prompt guide is belt-and-suspenders.
        // - When json_mode=false (plain chat): this is the model's ONLY
        //   contract for producing valid AgentAction JSON.
        conversation.push(ChatMessage::system(format!(
            "You are the Intelligence Compiler. Output ONLY valid JSON matching the AgentAction schema.\n\n{}",
            schema_guide
        )));

        conversation.extend(messages.iter().cloned());

        let mut errors = Vec::new();

        for _attempt in 0..=self.max_retries {
            let response = self
                .client
                .generate(&conversation)
                .await
                .map_err(CompilerError::PromptFailed)?;

            let cleaned_json = extract_json_block(&response);

            match serde_json::from_str::<crow_patch::AgentAction>(cleaned_json) {
                Ok(action) => {
                    // Semantic validation: enforce constraints serde can't check.
                    if let Err(reason) = action.validate() {
                        conversation.push(ChatMessage::assistant(response.clone()));
                        conversation.push(ChatMessage::user(format!(
                            "[SYSTEM: PREVIOUS ATTEMPT FAILED]\nYour JSON was syntactically valid but semantically invalid.\nReason: {}\n\nPlease fix and resubmit.",
                            reason
                        )));
                        // Use a synthetic serde error for the error list
                        errors.push(
                            serde_json::from_str::<()>(&format!("\"validation: {}\"", reason))
                                .unwrap_err(),
                        );
                        continue;
                    }
                    return Ok(action);
                }
                Err(e) => {
                    // Self-healing: append the failed attempt and error as
                    // assistant + user messages for the next retry.
                    conversation.push(ChatMessage::assistant(response.clone()));
                    conversation.push(ChatMessage::user(format!(
                        "[SYSTEM: PREVIOUS ATTEMPT FAILED]\nYour previous JSON output was invalid.\nError:\n{}\n\nPlease fix the JSON to strictly conform to the schema.",
                        e
                    )));
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
    if let Some(after_fence) = trimmed.strip_prefix("```json") {
        if let Some(end) = after_fence.rfind("```") {
            return after_fence[..end].trim();
        }
    }
    // Fallback: strip generic markdown block if no language specifier
    if let Some(after_fence) = trimmed.strip_prefix("```") {
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
        async fn generate(
            &self,
            _messages: &[ChatMessage],
        ) -> Result<String, crate::client::BrainError> {
            let mut resps = self.responses.lock().unwrap();
            if resps.is_empty() {
                Err(crate::client::BrainError::Config(
                    "No more mock responses".into(),
                ))
            } else {
                Ok(resps.remove(0))
            }
        }
    }

    fn valid_plan_json() -> String {
        r#"{
            "action": "submit_plan",
            "plan": {
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
            }
        }"#
        .into()
    }

    fn task_messages() -> Vec<ChatMessage> {
        vec![ChatMessage::user("do something")]
    }

    #[tokio::test]
    async fn compiler_succeeds_first_try() {
        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![valid_plan_json()])),
        });
        let compiler = IntentCompiler::new(client);

        let action = compiler
            .compile_action(&task_messages())
            .await
            .expect("compile should succeed");
        if let crow_patch::AgentAction::SubmitPlan { plan } = action {
            assert_eq!(plan.rationale, "mock");
        } else {
            panic!("Expected SubmitPlan");
        }
    }

    #[tokio::test]
    async fn compiler_self_heals_on_serde_error() {
        let bad_json = r#"{ "invalid": yes }"#.into();

        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![bad_json, valid_plan_json()])),
        });
        let compiler = IntentCompiler::new(client);

        let action = compiler
            .compile_action(&task_messages())
            .await
            .expect("compile should heal and succeed");
        if let crow_patch::AgentAction::SubmitPlan { plan } = action {
            assert_eq!(plan.rationale, "mock");
        } else {
            panic!("Expected SubmitPlan");
        }
    }

    #[tokio::test]
    async fn compiler_fails_after_max_retries() {
        let bad_json = String::from(r#"{ "still_bad": true }"#);

        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![
                bad_json.clone(),
                bad_json.clone(),
                bad_json.clone(),
                bad_json.clone(),
            ])),
        });
        let compiler = IntentCompiler::new(client).with_max_retries(2);

        let err = compiler.compile_action(&task_messages()).await.unwrap_err();
        match err {
            CompilerError::MaxRetriesExceeded(errors) => {
                assert_eq!(errors.len(), 3); // initial + 2 retries
            }
            _ => panic!("Expected max retries error"),
        }
    }

    // ─── extract_json_block tests ───────────────────────────────────

    #[test]
    fn extract_json_block_strips_markdown_fence() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_block(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_block_strips_generic_fence() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json_block(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_block_handles_leading_whitespace() {
        let input = "\n\n   {\"key\": \"value\"}   \n\n";
        assert_eq!(extract_json_block(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_block_passes_raw_json_through() {
        let input = r#"{"key": "value"}"#;
        assert_eq!(extract_json_block(input), input);
    }

    // ─── Provider-realism test ──────────────────────────────────────

    #[tokio::test]
    async fn compiler_handles_provider_leading_newline_in_content() {
        let with_newline = format!("\n{}", valid_plan_json());

        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![with_newline])),
        });
        let compiler = IntentCompiler::new(client);

        let action = compiler
            .compile_action(&task_messages())
            .await
            .expect("Leading newline in content should not break parsing");
        if let crow_patch::AgentAction::SubmitPlan { plan } = action {
            assert_eq!(plan.rationale, "mock");
        } else {
            panic!("Expected SubmitPlan");
        }
    }

    #[tokio::test]
    async fn compiler_handles_markdown_wrapped_response() {
        let wrapped = format!("```json\n{}\n```", valid_plan_json());

        let client = Box::new(MockLlm {
            responses: Arc::new(Mutex::new(vec![wrapped])),
        });
        let compiler = IntentCompiler::new(client);

        let action = compiler
            .compile_action(&task_messages())
            .await
            .expect("Markdown-wrapped JSON should be extracted and parsed");
        if let crow_patch::AgentAction::SubmitPlan { plan } = action {
            assert_eq!(plan.rationale, "mock");
        } else {
            panic!("Expected SubmitPlan");
        }
    }
}
