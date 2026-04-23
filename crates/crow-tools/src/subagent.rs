use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;

pub struct SubagentTool;

#[async_trait::async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &'static str {
        "subagent"
    }

    fn description(&self) -> &'static str {
        "Delegate a bounded, complex sub-task to a specialized subagent. The subagent acts as an \
         independent worker with its own tool access, workspace visibility, and context. Use this \
         when a task requires extensive exploration, planning, or a divergent line of reasoning \
         that would clutter your main context. The subagent will return a detailed summary of its \
         findings or actions once complete."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Clear, specific instructions on what the subagent should accomplish or investigate."
                },
                "role": {
                    "type": "string",
                    "description": "The persona/role for the subagent (e.g., 'Explorer', 'Coder', 'Reviewer', 'Architect'). Default is 'Generic'."
                },
                "focus_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of file paths the subagent should focus on."
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            task: String,
            role: Option<String>,
            focus_paths: Option<Vec<String>>,
        }
        let parsed: Args = serde_json::from_value(args)?;

        let delegator = match &ctx.subagent_delegator {
            Some(d) => d,
            None => return Ok(ToolOutput::error("Subagent delegation is not available in this context.")),
        };

        let role = parsed.role.unwrap_or_else(|| "Generic".to_string());
        let paths = parsed.focus_paths.unwrap_or_default();

        match delegator.delegate(parsed.task, role, paths).await {
            Ok(result) => Ok(ToolOutput::success(format!("Subagent completed successfully.\n\nResults:\n{result}"))),
            Err(e) => Ok(ToolOutput::error(format!("Subagent failed or aborted: {e}"))),
        }
    }
}
