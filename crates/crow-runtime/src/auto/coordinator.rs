use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use crow_brain::ChatMessage;

use super::artifact::{
    bounded_preview, AgentArtifactBundle, ArtifactKind, ArtifactStore, ArtifactSummary,
};
use super::execution::{AutoExecutionLimiter, AutoNodeExecutionRequest, AutoNodeExecutor};
use super::graph::{build_auto_graph, AutoGraph, AutoNodeId, AutoNodeState};
use super::prompt::render_node_prompt;
use super::AutoRunConfig;
use crate::event::{AgentEvent, EventHandler, OrchestrationEvent};

#[derive(Debug, Clone)]
pub struct AutoRunRequest {
    pub run_id: String,
    pub prompt: String,
    pub config: AutoRunConfig,
    pub system_messages: Vec<ChatMessage>,
    pub workspace_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoRunOutcome {
    pub run_id: String,
    pub success: bool,
    pub artifacts: ArtifactStore,
    pub summary: String,
}

pub struct AutoRunCoordinator {
    pub max_parallel_agents: usize,
}

impl AutoRunCoordinator {
    pub fn new(max_parallel_agents: usize) -> Self {
        Self {
            max_parallel_agents: max_parallel_agents.max(1),
        }
    }

    pub async fn run(
        &self,
        request: AutoRunRequest,
        observer: &mut dyn EventHandler,
    ) -> Result<AutoRunOutcome> {
        self.run_with_executor(request, observer, &mut SyntheticAutoNodeExecutor)
            .await
    }

    pub async fn run_with_executor<E>(
        &self,
        request: AutoRunRequest,
        observer: &mut dyn EventHandler,
        executor: &mut E,
    ) -> Result<AutoRunOutcome>
    where
        E: AutoNodeExecutor,
    {
        let mut graph = build_auto_graph(&request.prompt, &request.config, request.run_id.clone());
        let mut artifacts = ArtifactStore::default();
        let limiter = Arc::new(AutoExecutionLimiter::new(self.max_parallel_agents));

        emit_run_started(&graph, observer);

        let mut stopped_by_error = false;
        while !graph.is_terminal() {
            if observer.is_cancelled() {
                cancel_open_nodes(&mut graph);
                break;
            }

            let ready = graph.ready_node_ids();
            if ready.is_empty() {
                break;
            }

            for node_id in ready.into_iter().take(self.max_parallel_agents) {
                if observer.is_cancelled() {
                    cancel_open_nodes(&mut graph);
                    break;
                }

                let handoff_context = artifacts.render_handoff_context();
                let Some(prepared) =
                    prepare_node(&mut graph, &node_id, &handoff_context, &request, observer)
                else {
                    continue;
                };

                let guard = match limiter.try_acquire() {
                    Ok(guard) => guard,
                    Err(err) => {
                        mark_node_failed(&mut graph, &node_id, &err.to_string(), observer);
                        stopped_by_error = true;
                        break;
                    }
                };

                let result = executor.execute_node(prepared, observer).await;
                drop(guard);

                match result {
                    Ok(bundle) => {
                        complete_node(&mut graph, &node_id, bundle, &mut artifacts, observer)
                    }
                    Err(err) => {
                        mark_node_failed(&mut graph, &node_id, &err.to_string(), observer);
                        stopped_by_error = true;
                        break;
                    }
                }
            }

            if stopped_by_error {
                cancel_open_nodes(&mut graph);
                break;
            }
        }

        let success = graph
            .nodes
            .iter()
            .all(|node| matches!(node.state, AutoNodeState::Succeeded));
        let summary = summarize_run(success, &artifacts);

        Ok(AutoRunOutcome {
            run_id: request.run_id,
            success,
            artifacts,
            summary,
        })
    }
}

fn emit_run_started(graph: &AutoGraph, observer: &mut dyn EventHandler) {
    observer.handle_event(AgentEvent::Orchestration(OrchestrationEvent::AutoStarted {
        run_id: graph.run_id.clone(),
        prompt: graph.user_prompt.clone(),
        agent_count: graph.nodes.len(),
    }));
    observer.handle_event(AgentEvent::Orchestration(OrchestrationEvent::GraphReady {
        run_id: graph.run_id.clone(),
        node_count: graph.nodes.len(),
    }));

    for node in &graph.nodes {
        observer.handle_event(AgentEvent::Orchestration(OrchestrationEvent::NodeQueued {
            run_id: graph.run_id.clone(),
            node_id: node.id.0.clone(),
            dependencies: node.dependencies.iter().map(|dep| dep.0.clone()).collect(),
        }));
    }
}

fn prepare_node(
    graph: &mut AutoGraph,
    node_id: &AutoNodeId,
    handoff_context: &str,
    request: &AutoRunRequest,
    observer: &mut dyn EventHandler,
) -> Option<AutoNodeExecutionRequest> {
    let run_id = graph.run_id.clone();
    let node = graph.node_mut(node_id)?;
    let rendered = render_node_prompt(&node.spec, handoff_context);
    node.state = AutoNodeState::Running;
    observer.handle_event(AgentEvent::Orchestration(OrchestrationEvent::NodeStarted {
        run_id: run_id.clone(),
        node_id: node.id.0.clone(),
        phase: node.spec.phase.as_str().to_string(),
    }));
    observer.handle_event(AgentEvent::Orchestration(
        OrchestrationEvent::AgentPreview {
            run_id: run_id.clone(),
            agent_id: node.id.0.clone(),
            preview: rendered.clone(),
        },
    ));

    Some(AutoNodeExecutionRequest {
        run_id,
        node_id: node.id.clone(),
        agent_name: node.spec.name.clone(),
        role: node.spec.role.clone(),
        phase: node.spec.phase,
        task: rendered,
        focus_paths: Vec::new(),
        handoff_context: handoff_context.to_string(),
        system_messages: request.system_messages.clone(),
    })
}

fn complete_node(
    graph: &mut AutoGraph,
    node_id: &AutoNodeId,
    bundle: AgentArtifactBundle,
    artifacts: &mut ArtifactStore,
    observer: &mut dyn EventHandler,
) {
    let run_id = graph.run_id.clone();
    for summary in &bundle.summaries {
        observer.handle_event(AgentEvent::Orchestration(
            OrchestrationEvent::ArtifactProduced {
                run_id: run_id.clone(),
                node_id: bundle.node_id.0.clone(),
                title: summary.title.clone(),
                preview: summary.preview.clone(),
            },
        ));
    }
    artifacts.push(bundle);
    if let Some(node) = graph.node_mut(node_id) {
        node.state = AutoNodeState::Succeeded;
        observer.handle_event(AgentEvent::Orchestration(
            OrchestrationEvent::NodeCompleted {
                run_id,
                node_id: node.id.0.clone(),
                success: true,
            },
        ));
    }
}

fn mark_node_failed(
    graph: &mut AutoGraph,
    node_id: &AutoNodeId,
    reason: &str,
    observer: &mut dyn EventHandler,
) {
    let run_id = graph.run_id.clone();
    if let Some(node) = graph.node_mut(node_id) {
        node.state = AutoNodeState::Failed(reason.to_string());
        observer.handle_event(AgentEvent::Orchestration(OrchestrationEvent::NodeFailed {
            run_id,
            node_id: node.id.0.clone(),
            error: reason.to_string(),
        }));
    }
}

fn cancel_open_nodes(graph: &mut AutoGraph) {
    for node in &mut graph.nodes {
        if matches!(
            node.state,
            AutoNodeState::Pending | AutoNodeState::Ready | AutoNodeState::Running
        ) {
            node.state = AutoNodeState::Cancelled;
        }
    }
}

fn summarize_run(success: bool, artifacts: &ArtifactStore) -> String {
    let summary = artifacts.run_summary();
    if success {
        format!(
            "auto run completed: {} artifacts, {} files changed, {} verification commands",
            summary.total_artifacts,
            summary.files_changed.len(),
            summary.verification_commands.len()
        )
    } else {
        format!(
            "auto run stopped before completion: {} succeeded, {} failed",
            summary.successful_artifacts, summary.failed_artifacts
        )
    }
}

struct SyntheticAutoNodeExecutor;

impl AutoNodeExecutor for SyntheticAutoNodeExecutor {
    fn execute_node<'a>(
        &'a mut self,
        request: AutoNodeExecutionRequest,
        _observer: &'a mut dyn EventHandler,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentArtifactBundle>> + Send + 'a>>
    {
        Box::pin(async move { Ok(synthetic_artifact(request)) })
    }
}

fn synthetic_artifact(request: AutoNodeExecutionRequest) -> AgentArtifactBundle {
    let kind = match request.phase {
        super::AutoPhaseKind::Explore => ArtifactKind::Exploration,
        super::AutoPhaseKind::Plan => ArtifactKind::Plan,
        super::AutoPhaseKind::Execute => ArtifactKind::Implementation,
        super::AutoPhaseKind::Review => ArtifactKind::Review,
        super::AutoPhaseKind::Verify => ArtifactKind::Verification,
    };

    AgentArtifactBundle {
        node_id: request.node_id,
        phase: request.phase,
        final_text: request.task.clone(),
        summaries: vec![ArtifactSummary {
            kind,
            title: request.agent_name,
            preview: bounded_preview(&request.task, 160),
        }],
        files_read: Vec::new(),
        files_changed: Vec::new(),
        verification_commands: Vec::new(),
        success: true,
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use anyhow::anyhow;

    use super::*;
    use crate::auto::graph::{build_auto_graph, AutoNodeState};
    use crate::auto::{AutoRunConfig, AutoStrategy};

    #[test]
    fn scheduler_starts_with_explore_nodes_only() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 2,
            max_agent_depth: 2,
            strategy: AutoStrategy::Thorough,
        };
        let graph = build_auto_graph("refactor", &cfg, "auto-test");
        let ready = graph.ready_node_ids();

        assert!(ready.iter().any(|id| id.0.starts_with("explorer")));
        assert!(ready
            .iter()
            .any(|id| id.0.starts_with("architecture-scout")));
        assert!(!ready.iter().any(|id| id.0.starts_with("planner")));
    }

    #[test]
    fn next_phase_waits_for_all_prior_phase_nodes() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 2,
            max_agent_depth: 2,
            strategy: AutoStrategy::Thorough,
        };
        let graph = build_auto_graph("refactor", &cfg, "auto-test");
        let Some(planner) = graph
            .nodes
            .iter()
            .find(|node| node.id.0.starts_with("planner"))
        else {
            panic!("expected planner node");
        };

        assert!(planner
            .dependencies
            .iter()
            .any(|id| id.0.starts_with("explorer")));
        assert!(planner
            .dependencies
            .iter()
            .any(|id| id.0.starts_with("architecture-scout")));
    }

    #[test]
    fn scheduler_unblocks_next_node_after_dependency_succeeds() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 1,
            max_agent_depth: 2,
            strategy: AutoStrategy::Balanced,
        };
        let mut graph = build_auto_graph("refactor", &cfg, "auto-test");
        let first = graph.ready_node_ids()[0].clone();
        let Some(first_node) = graph.node_mut(&first) else {
            panic!("expected first ready node to exist");
        };
        first_node.state = AutoNodeState::Succeeded;

        let ready = graph.ready_node_ids();

        assert!(ready.iter().any(|id| id.0.starts_with("planner")));
    }

    #[tokio::test]
    async fn coordinator_collects_artifact_for_each_node() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 2,
            max_agent_depth: 2,
            strategy: AutoStrategy::Balanced,
        };
        let coordinator = super::AutoRunCoordinator::new(2);
        let request = super::AutoRunRequest {
            run_id: "auto-test".into(),
            prompt: "refactor".into(),
            config: cfg,
            system_messages: Vec::new(),
            workspace_root: std::path::PathBuf::from("."),
        };
        let outcome = match coordinator
            .run(request, &mut RecordingObserver::new())
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => panic!("coordinator run failed: {err}"),
        };

        assert!(outcome.success);
        assert_eq!(outcome.artifacts.all().len(), 5);
        assert!(outcome.summary.contains("5 artifacts"));
        assert!(outcome
            .artifacts
            .render_handoff_context()
            .contains("planner"));
    }

    #[tokio::test]
    async fn coordinator_stops_on_executor_failure() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 1,
            max_agent_depth: 2,
            strategy: AutoStrategy::Balanced,
        };
        let coordinator = super::AutoRunCoordinator::new(1);
        let request = super::AutoRunRequest {
            run_id: "auto-test".into(),
            prompt: "refactor".into(),
            config: cfg,
            system_messages: Vec::new(),
            workspace_root: std::path::PathBuf::from("."),
        };
        let mut executor = FailingExecutor;
        let mut observer = RecordingObserver::new();
        let outcome = match coordinator
            .run_with_executor(request, &mut observer, &mut executor)
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => panic!("coordinator run failed unexpectedly: {err}"),
        };

        assert!(!outcome.success);
        assert!(outcome.summary.contains("stopped before completion"));
        assert!(observer.events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::Orchestration(OrchestrationEvent::NodeFailed { .. })
            )
        }));
    }

    struct RecordingObserver {
        events: Vec<AgentEvent>,
    }

    impl RecordingObserver {
        fn new() -> Self {
            Self { events: Vec::new() }
        }
    }

    impl crate::event::EventHandler for RecordingObserver {
        fn handle_event(&mut self, event: crate::event::AgentEvent) {
            self.events.push(event);
        }
    }

    struct FailingExecutor;

    impl AutoNodeExecutor for FailingExecutor {
        fn execute_node<'a>(
            &'a mut self,
            _request: AutoNodeExecutionRequest,
            _observer: &'a mut dyn EventHandler,
        ) -> Pin<Box<dyn Future<Output = Result<AgentArtifactBundle>> + Send + 'a>> {
            Box::pin(async { Err(anyhow!("planned failure")) })
        }
    }
}
