//! Subagent worker — spawns an isolated agent context for delegated tasks.
//!
//! # Architecture
//!
//! The `SubagentWorker` is now powered by `run_agent_loop` (native tool-call
//! state machine) instead of the legacy `run_epistemic_loop`. This means
//! subagents get:
//!
//! - Full `TurnContext` semantics (immutable per-turn config snapshot)
//! - Native tool calling via the provider's protocol (not custom JSON)
//! - `ToolOrchestrator` pipeline (approval → lock → timeout → truncation)
//! - Per-tool RwLock parallelism (read-only tools run concurrently)
//! - Backoff with jitter on transient LLM errors
//! - The same 120s hard timeout from AGENTS.md
//!
//! # Delegation Depth
//!
//! Recursive delegation is bounded at 3 levels by the epistemic loop's
//! `delegation_count` check. The 120s `tokio::time::timeout` wrapping
//! the entire execution prevents stalled LLM calls from hanging forever.

use crate::context::ConversationManager;
use crate::event::{AgentEvent, EventHandler};
use crate::role::AgentRole;
use crate::turn_context::TurnContext;
use crow_brain::compiler::IntentCompiler;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub struct SubagentWorker {
    pub id: String,
    pub role: AgentRole,
    compiler: IntentCompiler,
    task_registry: crate::registry::TaskRegistry,
    tool_registry: Arc<crow_tools::ToolRegistry>,
    permissions: Arc<crow_tools::PermissionEnforcer>,
    /// Inherited from parent TurnContext so subagents use the same model.
    parent_model: String,
    /// Inherited from parent TurnContext so subagents use the same provider.
    parent_provider: String,
    /// Parent cancellation token — derived child token propagates ESC to subagents.
    parent_cancel: Option<tokio_util::sync::CancellationToken>,
}

impl SubagentWorker {
    pub fn new(
        role: AgentRole,
        compiler: IntentCompiler,
        task_registry: crate::registry::TaskRegistry,
        tool_registry: Arc<crow_tools::ToolRegistry>,
        permissions: Arc<crow_tools::PermissionEnforcer>,
    ) -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_micros();
        let id = format!("sub-{:08x}", ts as u32);
        Self {
            id,
            role,
            compiler,
            task_registry,
            tool_registry,
            permissions,
            parent_model: String::new(),
            parent_provider: String::new(),
            parent_cancel: None,
        }
    }

    /// Inherit model/provider from parent TurnContext (Codex pattern).
    /// Subagents should use the same LLM as their parent for consistency.
    #[must_use]
    pub fn with_parent_context(mut self, ctx: &TurnContext) -> Self {
        self.parent_model = ctx.model.clone();
        self.parent_provider = ctx.provider.clone();
        self.parent_cancel = Some(ctx.child_cancel_token());
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        task: &str,
        focus_paths: &[crow_patch::WorkspacePath],
        rationale: &str,
        sys_msgs: Vec<crow_brain::ChatMessage>,
        workspace_root: &Path,
        _mcp_manager: Option<&crate::mcp::McpManager>,
        parent_observer: &mut dyn EventHandler,
    ) -> anyhow::Result<crow_patch::IntentPlan> {
        let identity = format!(
            "You are a specialized Subagent Worker (Role: {role}, ID: {id}). You have been delegated the following bounded task by the Architect Orchestrator:\n\n\
            TASK: {task}\n\n\
            FOCUS PATHS: {focus_paths:?}\n\n\
            RATIONALE: {rationale}\n\n\
            {role_instructions}\
            Use your available tools to complete the task. When done, respond with a clear summary of what you accomplished.",
            role = self.role.name,
            id = self.id,
            task = task,
            focus_paths = focus_paths,
            rationale = rationale,
            role_instructions = if self.role.system_prompt_suffix.is_empty() {
                String::new()
            } else {
                format!("{}\n\n", self.role.system_prompt_suffix)
            }
        );

        // Build system messages with subagent-specific identity override
        let mut msgs = sys_msgs.clone();
        if let Some(first) = msgs.first_mut() {
            first.content = identity;
        } else {
            msgs.push(crow_brain::ChatMessage::system(format!(
                "You are a specialized Subagent Worker (Role: {}, ID: {}).",
                self.role.name, self.id
            )));
        }

        let mut sub_messages = ConversationManager::new(msgs);

        // ── Git context injection for subagents (Codex pattern) ──────
        // Subagents inherit git context from the workspace to maintain
        // branch/status awareness across the delegation hierarchy.
        if let Some(git_ctx) = crate::git_context::GitContext::detect(workspace_root) {
            let rendered = git_ctx.render();
            if !rendered.is_empty() {
                sub_messages.push_user(format!(
                    "[CONTEXT] Current workspace git state:\n{rendered}"
                ));
            }
        }

        sub_messages.push_user(format!("Task:\n{task}"));

        let task_desc = format!("[{}] {}", self.role.name, task);
        parent_observer.handle_event(crate::event::AgentEvent::DelegateStart(
            self.id.clone(),
            task_desc,
        ));

        let mut observer = SubagentEventHandler {
            id: self.id.clone(),
            role: self.role.clone(),
            parent: parent_observer,
        };

        // Register task in the task registry
        let task_def = crate::registry::AgentTask {
            id: self.id.clone(),
            name: format!("Subagent-{}", self.role.name),
            description: task.to_string(),
            status: crate::registry::TaskStatus::Running,
            output: None,
        };
        self.task_registry.register(task_def);

        // Build TurnContext for the subagent (Codex pattern: immutable per-turn snapshot)
        let file_state = Arc::new(crow_tools::FileStateStore::new());
        let background_manager = Arc::new(crow_tools::BackgroundProcessManager::new());

        // Inherit model/provider from parent, fallback to "subagent"
        let model = if self.parent_model.is_empty() {
            "subagent".to_string()
        } else {
            self.parent_model.clone()
        };
        let provider = if self.parent_provider.is_empty() {
            "subagent".to_string()
        } else {
            self.parent_provider.clone()
        };

        let mut builder = TurnContext::builder()
            .model(model)
            .provider(provider)
            .compiler(Arc::new(self.compiler.clone()))
            .workspace_root(workspace_root.to_path_buf())
            .tool_registry(Arc::clone(&self.tool_registry))
            .permissions(Arc::clone(&self.permissions))
            .file_state(file_state)
            .background_manager(background_manager)
            .max_steps(self.role.max_steps)
            .role(self.role.clone());

        // Propagate parent cancellation token (Codex pattern: ESC cancels child agents)
        if let Some(parent_token) = &self.parent_cancel {
            builder = builder.cancel_token(parent_token.child_token());
        }

        let turn_ctx = builder
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build subagent TurnContext: {e}"))?;

        // Enforce the 120s hard timeout from AGENTS.md
        const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(120);

        let execution_result = tokio::time::timeout(
            SUBAGENT_TIMEOUT,
            crate::agent_loop::run_agent_loop(&turn_ctx, &mut sub_messages, &mut observer),
        )
        .await;

        let success = matches!(&execution_result, Ok(Ok(_)));
        observer
            .parent
            .handle_event(crate::event::AgentEvent::DelegateComplete(
                self.id.clone(),
                success,
            ));

        match execution_result {
            Ok(Ok(result)) => {
                self.task_registry
                    .update_status(&self.id, crate::registry::TaskStatus::Completed);

                // Convert AgentLoopResult into an IntentPlan-like representation.
                // Subagents in native tool-call mode don't produce IntentPlan;
                // instead they write changes directly and return a text summary.
                // We surface this as a "no-op plan" with the summary as rationale,
                // allowing the orchestrator to collect findings.
                Ok(crow_patch::IntentPlan {
                    base_snapshot_id: crow_patch::SnapshotId("subagent".into()),
                    rationale: result.final_text,
                    is_partial: false,
                    confidence: crow_patch::Confidence::None,
                    requires_mcts: false,
                    operations: vec![],
                })
            }
            Ok(Err(e)) => {
                self.task_registry
                    .update_status(&self.id, crate::registry::TaskStatus::Failed(e.to_string()));
                Err(e)
            }
            Err(_) => {
                let err_msg = format!(
                    "Subagent [{id}] timed out after {timeout}s",
                    id = self.id,
                    timeout = SUBAGENT_TIMEOUT.as_secs()
                );
                self.task_registry.update_status(
                    &self.id,
                    crate::registry::TaskStatus::Failed(err_msg.clone()),
                );
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
            AgentEvent::ActionStart(msg) => self.parent.handle_event(AgentEvent::ActionStart(
                format!("[{}:{}] {}", self.role.name, self.id, msg),
            )),
            AgentEvent::ActionComplete(msg) => self.parent.handle_event(
                AgentEvent::ActionComplete(format!("[{}:{}] {}", self.role.name, self.id, msg)),
            ),
            AgentEvent::ReadFiles(paths) => {
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ...", paths[0])
                };
                self.parent.handle_event(AgentEvent::Log(format!(
                    "  [{}:{}] 📖 Reading: {}",
                    self.role.name, self.id, display
                )));
            }
            AgentEvent::ReconStart(msg) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}:{}] 🔍 Recon: {}",
                self.role.name, self.id, msg
            ))),
            AgentEvent::DelegateStart(id, msg) => self.parent.handle_event(
                AgentEvent::DelegateStart(id, format!("[{}:{}] {}", self.role.name, self.id, msg)),
            ),
            AgentEvent::DelegateComplete(id, success) => self
                .parent
                .handle_event(AgentEvent::DelegateComplete(id, success)),
            AgentEvent::PlanSubmitted(_) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}:{}] 📋 Plan Submitted",
                self.role.name, self.id
            ))),
            AgentEvent::CruciblePreflight(msg) => self.parent.handle_event(AgentEvent::Log(
                format!("  [{}:{}] 🛡️ Preflight: {}", self.role.name, self.id, msg),
            )),
            AgentEvent::Log(msg) => self.parent.handle_event(AgentEvent::Log(format!(
                "  [{}:{}] {}",
                self.role.name, self.id, msg
            ))),
            AgentEvent::Error(msg) => self.parent.handle_event(AgentEvent::Error(format!(
                "[{}:{}] {}",
                self.role.name, self.id, msg
            ))),
            AgentEvent::Markdown(msg) => self.parent.handle_event(AgentEvent::Markdown(msg)),
            // Pass through new high-granularity events with subagent context
            AgentEvent::TokenUsage { .. } => self.parent.handle_event(event),
            AgentEvent::StateChanged { from, to } => self.parent.handle_event(AgentEvent::Log(
                format!("  [{}:{}] State: {} → {}", self.role.name, self.id, from, to),
            )),
            AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason,
            } => self.parent.handle_event(AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason: format!("[{}:{}] {}", self.role.name, self.id, reason),
            }),
            AgentEvent::Compacting { active } => {
                self.parent.handle_event(AgentEvent::Compacting { active })
            }
            AgentEvent::ToolProgress { tool_id, message } => {
                self.parent.handle_event(AgentEvent::ToolProgress {
                    tool_id,
                    message: format!("[{}:{}] {}", self.role.name, self.id, message),
                })
            }
            // Forward structured tool-call lifecycle events with subagent context
            AgentEvent::ToolCallStarted {
                call_id,
                tool_name,
                is_read_only,
            } => self.parent.handle_event(AgentEvent::ToolCallStarted {
                call_id,
                tool_name: format!("[{}:{}] {}", self.role.name, self.id, tool_name),
                is_read_only,
            }),
            AgentEvent::ToolCallCompleted {
                call_id,
                tool_name,
                duration_ms,
                output_bytes,
                is_error,
                retry_count,
                from_cache,
                preview,
            } => self.parent.handle_event(AgentEvent::ToolCallCompleted {
                call_id,
                tool_name: format!("[{}:{}] {}", self.role.name, self.id, tool_name),
                duration_ms,
                output_bytes,
                is_error,
                retry_count,
                from_cache,
                preview,
            }),
            // Forward structured turn lifecycle events to parent as-is
            AgentEvent::Turn(ev) => self.parent.handle_event(AgentEvent::Turn(ev)),
            // Forward phased errors with subagent context prefix
            AgentEvent::PhasedError {
                phase,
                error,
                is_recoverable,
            } => self.parent.handle_event(AgentEvent::PhasedError {
                phase,
                error: format!("[{}:{}] {}", self.role.name, self.id, error),
                is_recoverable,
            }),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.parent.is_cancelled()
    }
}
