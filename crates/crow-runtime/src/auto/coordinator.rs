use std::path::PathBuf;

use anyhow::Result;
use crow_brain::ChatMessage;

use super::artifact::{AgentArtifactBundle, ArtifactKind, ArtifactStore, ArtifactSummary};
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
        let mut graph = build_auto_graph(&request.prompt, &request.config, request.run_id.clone());
        let mut artifacts = ArtifactStore::default();

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
                run_node_without_worker(&mut graph, &node_id, &mut artifacts, observer);
            }
        }

        let success = graph
            .nodes
            .iter()
            .all(|node| matches!(node.state, AutoNodeState::Succeeded));

        let summary = if success {
            "auto run completed".to_string()
        } else {
            "auto run stopped before completion".to_string()
        };

        Ok(AutoRunOutcome {
            run_id: request.run_id,
            success,
            artifacts,
            summary,
        })
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

fn run_node_without_worker(
    graph: &mut AutoGraph,
    node_id: &AutoNodeId,
    artifacts: &mut ArtifactStore,
    observer: &mut dyn EventHandler,
) {
    let run_id = graph.run_id.clone();
    let handoff_context = artifacts.render_handoff_context();
    if let Some(node) = graph.node_mut(node_id) {
        let rendered = render_node_prompt(&node.spec, &handoff_context);
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
        let bundle = synthetic_artifact(&node.id, node.spec.phase, &node.spec.name, &rendered);
        if let Some(summary) = bundle.summaries.first() {
            observer.handle_event(AgentEvent::Orchestration(
                OrchestrationEvent::ArtifactProduced {
                    run_id: run_id.clone(),
                    node_id: node.id.0.clone(),
                    title: summary.title.clone(),
                    preview: summary.preview.clone(),
                },
            ));
        }
        artifacts.push(bundle);
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

fn synthetic_artifact(
    node_id: &AutoNodeId,
    phase: super::AutoPhaseKind,
    name: &str,
    rendered_prompt: &str,
) -> AgentArtifactBundle {
    let preview = rendered_prompt
        .split_whitespace()
        .take(32)
        .collect::<Vec<_>>()
        .join(" ");
    let kind = match phase {
        super::AutoPhaseKind::Explore => ArtifactKind::Exploration,
        super::AutoPhaseKind::Plan => ArtifactKind::Plan,
        super::AutoPhaseKind::Execute => ArtifactKind::Implementation,
        super::AutoPhaseKind::Review => ArtifactKind::Review,
        super::AutoPhaseKind::Verify => ArtifactKind::Verification,
    };

    AgentArtifactBundle {
        node_id: node_id.clone(),
        phase,
        final_text: rendered_prompt.to_string(),
        summaries: vec![ArtifactSummary {
            kind,
            title: name.to_string(),
            preview,
        }],
        files_read: Vec::new(),
        files_changed: Vec::new(),
        verification_commands: Vec::new(),
        success: true,
    }
}

#[cfg(test)]
mod tests {
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
        let outcome = match coordinator.run(request, &mut RecordingObserver).await {
            Ok(outcome) => outcome,
            Err(err) => panic!("coordinator run failed: {err}"),
        };

        assert!(outcome.success);
        assert_eq!(outcome.artifacts.all().len(), 5);
        assert!(outcome
            .artifacts
            .render_handoff_context()
            .contains("planner"));
    }

    struct RecordingObserver;

    impl crate::event::EventHandler for RecordingObserver {
        fn handle_event(&mut self, _event: crate::event::AgentEvent) {}
    }
}
