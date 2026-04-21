use crate::context::ConversationManager;
use crate::event::{AgentEvent, EventHandler};
use crow_brain::compiler::IntentCompiler;
use crow_patch::IntentPlan;
use std::path::Path;

pub struct SubagentWorker {
    pub id: String,
    compiler: IntentCompiler,
}

impl SubagentWorker {
    pub fn new(compiler: IntentCompiler) -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_micros();
        let id = format!("sub-{:08x}", ts as u32);
        Self { id, compiler }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        task: &str,
        focus_paths: &[crow_patch::WorkspacePath],
        rationale: &str,
        sys_msgs: Vec<crow_brain::ChatMessage>,
        frozen_root: &Path,
        mcp_manager: Option<&crate::mcp::McpManager>,
        parent_observer: &mut dyn EventHandler,
    ) -> anyhow::Result<IntentPlan> {
        let identity = format!(
            "You are a specialized Subagent Worker (ID: {}). You have been delegated the following bounded task by the Architect Orchestrator:\n\n\
            TASK: {}\n\n\
            FOCUS PATHS: {:?}\n\n\
            RATIONALE: {}\n\n\
            Perform any necessary file reads or tool calls. When you have answers or a plan, emit a SubmitPlan action. \
            If you resolve the requested information without modifying code, emit an empty operations array and return your findings in the rationale.",
            self.id, task, focus_paths, rationale
        );

        let mut msgs = sys_msgs.clone();
        if let Some(first) = msgs.first_mut() {
            first.content = identity;
        }

        let mut sub_messages = ConversationManager::new(msgs);

        let mut observer = SubagentEventHandler {
            id: self.id.clone(),
            parent: parent_observer,
        };

        let file_state_store = std::sync::Arc::new(crate::file_state::FileStateStore::new());
        crate::epistemic::run_epistemic_loop(
            &self.compiler,
            &mut sub_messages,
            frozen_root,
            mcp_manager,
            &mut observer,
            file_state_store,
        )
        .await
    }
}

pub struct SubagentEventHandler<'a> {
    id: String,
    parent: &'a mut dyn EventHandler,
}

impl EventHandler for SubagentEventHandler<'_> {
    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::StreamChunk(c) => self.parent.handle_event(AgentEvent::StreamChunk(c)),
            AgentEvent::Thinking(a, b) => self.parent.handle_event(AgentEvent::Thinking(a, b)),
            AgentEvent::ActionStart(msg) => self
                .parent
                .handle_event(AgentEvent::ActionStart(format!("[{}] {}", self.id, msg))),
            AgentEvent::ActionComplete(msg) => self
                .parent
                .handle_event(AgentEvent::ActionComplete(format!("[{}] {}", self.id, msg))),
            AgentEvent::ReadFiles(paths) => {
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ...", paths[0])
                };
                self.parent.handle_event(AgentEvent::Log(format!(
                    "  [{}] 📖 Reading: {}",
                    self.id, display
                )));
            }
            AgentEvent::ReconStart(msg) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}] 🔍 Recon: {}",
                self.id, msg
            ))),
            AgentEvent::DelegateStart(msg) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}] 🤖 Delegating: {}",
                self.id, msg
            ))),
            AgentEvent::PlanSubmitted(_) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}] 📋 Plan Submitted",
                self.id
            ))),
            AgentEvent::CruciblePreflight(msg) => self.parent.handle_event(AgentEvent::Log(
                format!("  [{}] 🛡️ Preflight: {}", self.id, msg),
            )),
            AgentEvent::Log(msg) => self
                .parent
                .handle_event(AgentEvent::Log(format!("  [{}] {}", self.id, msg))),
            AgentEvent::Error(msg) => self
                .parent
                .handle_event(AgentEvent::Error(format!("[{}] {}", self.id, msg))),
            AgentEvent::Markdown(msg) => self.parent.handle_event(AgentEvent::Markdown(msg)),
            // Pass through new high-granularity events with subagent context
            AgentEvent::TokenUsage { .. } => self.parent.handle_event(event),
            AgentEvent::StateChanged { from, to } => self.parent.handle_event(AgentEvent::Log(
                format!("  [{}] State: {} → {}", self.id, from, to),
            )),
            AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason,
            } => self.parent.handle_event(AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason: format!("[{}] {}", self.id, reason),
            }),
            AgentEvent::Compacting { active } => {
                self.parent.handle_event(AgentEvent::Compacting { active })
            }
            AgentEvent::ToolProgress { tool_id, message } => {
                self.parent.handle_event(AgentEvent::ToolProgress {
                    tool_id,
                    message: format!("[{}] {}", self.id, message),
                })
            }
            // Forward structured turn lifecycle events to parent as-is
            AgentEvent::Turn(ev) => self.parent.handle_event(AgentEvent::Turn(ev)),
        }
    }
}
