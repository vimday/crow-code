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
    Architect,
    Executor,
    Reviewer,
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explorer => write!(f, "Explorer"),
            Self::Coder => write!(f, "Coder"),
            Self::Generic => write!(f, "Generic"),
            Self::Architect => write!(f, "Architect"),
            Self::Executor => write!(f, "Executor"),
            Self::Reviewer => write!(f, "Reviewer"),
        }
    }
}

pub struct SubagentWorker {
    pub id: String,
    pub role: AgentRole,
    compiler: IntentCompiler,
    task_registry: crate::registry::TaskRegistry,
    tool_registry: std::sync::Arc<crow_tools::ToolRegistry>,
    permissions: std::sync::Arc<crow_tools::PermissionEnforcer>,
}

impl SubagentWorker {
    pub fn new(
        role: AgentRole, 
        compiler: IntentCompiler, 
        task_registry: crate::registry::TaskRegistry,
        tool_registry: std::sync::Arc<crow_tools::ToolRegistry>,
        permissions: std::sync::Arc<crow_tools::PermissionEnforcer>,
    ) -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_micros();
        let id = format!("sub-{:08x}", ts as u32);
        Self { id, role, compiler, task_registry, tool_registry, permissions }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        task: &str,
        focus_paths: &[crow_patch::WorkspacePath],
        rationale: &str,
        sys_msgs: Vec<crow_brain::ChatMessage>,
        workspace_root: &Path,
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

        let task_desc = format!("[{}] {}", self.role, task);
        parent_observer.handle_event(crate::event::AgentEvent::DelegateStart(self.id.clone(), task_desc.clone()));

        let mut observer = SubagentEventHandler {
            id: self.id.clone(),
            role: self.role,
            parent: parent_observer,
        };

        let file_state_store = std::sync::Arc::new(crate::file_state::FileStateStore::new());
        // Enforce a hard timeout matching the AGENTS.md branch-level 120s limit.
        // Prevents stalled LLM calls or infinite recon loops from hanging forever.
        const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(120);

        let task_def = crate::registry::AgentTask {
            id: self.id.clone(),
            name: format!("Subagent-{}", self.role),
            description: task.to_string(),
            status: crate::registry::TaskStatus::Running,
            output: None,
        };
        self.task_registry.register(task_def);
        
        let execution_result = tokio::time::timeout(
            SUBAGENT_TIMEOUT,
            crate::epistemic::run_epistemic_loop(
                &self.compiler,
                &mut sub_messages,
                workspace_root,
                mcp_manager,
                &mut observer,
                file_state_store,
                std::sync::Arc::clone(&self.tool_registry),
                std::sync::Arc::clone(&self.permissions),
            ),
        )
        .await;
        
        let success = matches!(&execution_result, Ok(Ok(_)));
        observer.parent.handle_event(crate::event::AgentEvent::DelegateComplete(self.id.clone(), success));

        match execution_result {
            Ok(Ok(plan)) => {
                self.task_registry.update_status(&self.id, crate::registry::TaskStatus::Completed);
                Ok(plan)
            }
            Ok(Err(e)) => {
                self.task_registry.update_status(&self.id, crate::registry::TaskStatus::Failed(e.to_string()));
                Err(e)
            }
            Err(_) => {
                let err_msg = format!("Subagent [{id}] timed out after {timeout}s", id = self.id, timeout = SUBAGENT_TIMEOUT.as_secs());
                self.task_registry.update_status(&self.id, crate::registry::TaskStatus::Failed(err_msg.clone()));
                Err(anyhow::anyhow!(err_msg))
            }
        }
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
            AgentEvent::DelegateStart(id, msg) => self.parent.handle_event(AgentEvent::DelegateStart(id, format!(
                "[{}:{}] {}",
                self.role, self.id, msg
            ))),
            AgentEvent::DelegateComplete(id, success) => self.parent.handle_event(AgentEvent::DelegateComplete(id, success)),
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
