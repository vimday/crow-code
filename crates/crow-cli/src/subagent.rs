use crate::context::ConversationManager;
use crate::event::{AgentEvent, EventHandler};
use crow_brain::compiler::IntentCompiler;
use crow_patch::IntentPlan;
use std::path::Path;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Explorer,
    Coder,
    Generic,
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explorer => write!(f, "Explorer"),
            Self::Coder => write!(f, "Coder"),
            Self::Generic => write!(f, "Generic"),
        }
    }
}

pub struct SubagentWorker {
    pub id: String,
    pub role: AgentRole,
    compiler: IntentCompiler,
}

impl SubagentWorker {
    pub fn new(role: AgentRole, compiler: IntentCompiler) -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_micros();
        let id = format!("sub-{:08x}", ts as u32);
        Self { id, role, compiler }
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
            "You are a specialized Subagent Worker (Role: {role}, ID: {id}). You have been delegated the following bounded task by the Architect Orchestrator:\n\n\
            TASK: {task}\n\n\
            FOCUS PATHS: {focus_paths:?}\n\n\
            RATIONALE: {rationale}\n\n\
            Perform any necessary file reads or tool calls. When you have answers or a plan, emit a SubmitPlan action. \
            If you resolve the requested information without modifying code, emit an empty operations array and return your findings in the rationale.",
            role = self.role,
            id = self.id,
            task = task,
            focus_paths = focus_paths,
            rationale = rationale
        );

        let mut msgs = sys_msgs.clone();
        if let Some(first) = msgs.first_mut() {
            first.content = identity;
        }

        let mut sub_messages = ConversationManager::new(msgs);

        let mut observer = SubagentEventHandler {
            id: self.id.clone(),
            role: self.role,
            parent: parent_observer,
        };

        let file_state_store = std::sync::Arc::new(crate::file_state::FileStateStore::new());
        // Enforce a hard timeout matching the AGENTS.md branch-level 120s limit.
        // Prevents stalled LLM calls or infinite recon loops from hanging forever.
        const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(120);
        tokio::time::timeout(
            SUBAGENT_TIMEOUT,
            crate::epistemic::run_epistemic_loop(
                &self.compiler,
                &mut sub_messages,
                frozen_root,
                mcp_manager,
                &mut observer,
                file_state_store,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "Subagent [{id}] timed out after {timeout}s",
            id = self.id,
            timeout = SUBAGENT_TIMEOUT.as_secs()
        ))?
    }
}

pub struct SubagentEventHandler<'a> {
    id: String,
    role: AgentRole,
    parent: &'a mut dyn EventHandler,
}

impl EventHandler for SubagentEventHandler<'_> {
    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::StreamChunk(c) => self.parent.handle_event(AgentEvent::StreamChunk(c)),
            AgentEvent::Thinking(a, b) => self.parent.handle_event(AgentEvent::Thinking(a, b)),
            AgentEvent::ActionStart(msg) => self
                .parent
                .handle_event(AgentEvent::ActionStart(format!("[{}:{}] {}", self.role, self.id, msg))),
            AgentEvent::ActionComplete(msg) => self
                .parent
                .handle_event(AgentEvent::ActionComplete(format!("[{}:{}] {}", self.role, self.id, msg))),
            AgentEvent::ReadFiles(paths) => {
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ...", paths[0])
                };
                self.parent.handle_event(AgentEvent::Log(format!(
                    "  [{}:{}] 📖 Reading: {}",
                    self.role, self.id, display
                )));
            }
            AgentEvent::ReconStart(msg) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}:{}] 🔍 Recon: {}",
                self.role, self.id, msg
            ))),
            AgentEvent::DelegateStart(msg) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}:{}] 🤖 Delegating: {}",
                self.role, self.id, msg
            ))),
            AgentEvent::PlanSubmitted(_) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}:{}] 📋 Plan Submitted",
                self.role, self.id
            ))),
            AgentEvent::CruciblePreflight(msg) => self.parent.handle_event(AgentEvent::Log(
                format!("  [{}:{}] 🛡️ Preflight: {}", self.role, self.id, msg),
            )),
            AgentEvent::Log(msg) => self
                .parent
                .handle_event(AgentEvent::Log(format!("  [{}:{}] {}", self.role, self.id, msg))),
            AgentEvent::Error(msg) => self
                .parent
                .handle_event(AgentEvent::Error(format!("[{}:{}] {}", self.role, self.id, msg))),
            AgentEvent::Markdown(msg) => self.parent.handle_event(AgentEvent::Markdown(msg)),
            // Pass through new high-granularity events with subagent context
            AgentEvent::TokenUsage { .. } => self.parent.handle_event(event),
            AgentEvent::StateChanged { from, to } => self.parent.handle_event(AgentEvent::Log(
                format!("  [{}:{}] State: {} → {}", self.role, self.id, from, to),
            )),
            AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason,
            } => self.parent.handle_event(AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason: format!("[{}:{}] {}", self.role, self.id, reason),
            }),
            AgentEvent::Compacting { active } => {
                self.parent.handle_event(AgentEvent::Compacting { active })
            }
            AgentEvent::ToolProgress { tool_id, message } => {
                self.parent.handle_event(AgentEvent::ToolProgress {
                    tool_id,
                    message: format!("[{}:{}] {}", self.role, self.id, message),
                })
            }
            // Forward structured turn lifecycle events to parent as-is
            AgentEvent::Turn(ev) => self.parent.handle_event(AgentEvent::Turn(ev)),
        }
    }
}
