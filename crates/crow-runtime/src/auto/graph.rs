use super::{build_auto_plan, AutoAgentSpec, AutoPhaseKind, AutoRunConfig};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AutoNodeId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoNodeState {
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed(String),
    Skipped(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoFailurePolicy {
    StopRun,
    ContinueReadOnly,
    RequireUserApproval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoNode {
    pub id: AutoNodeId,
    pub spec: AutoAgentSpec,
    pub dependencies: Vec<AutoNodeId>,
    pub state: AutoNodeState,
    pub failure_policy: AutoFailurePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoGraph {
    pub run_id: String,
    pub user_prompt: String,
    pub nodes: Vec<AutoNode>,
    pub phase_order: Vec<AutoPhaseKind>,
}

impl AutoGraph {
    pub fn ready_node_ids(&self) -> Vec<AutoNodeId> {
        self.nodes
            .iter()
            .filter(|node| node.state == AutoNodeState::Ready)
            .map(|node| node.id.clone())
            .collect()
    }

    pub fn mark_new_ready(&mut self) -> Vec<AutoNodeId> {
        let ready_ids: Vec<_> = self
            .nodes
            .iter()
            .filter(|node| node.state == AutoNodeState::Pending)
            .filter(|node| {
                node.dependencies.iter().all(|dep| {
                    self.nodes.iter().any(|candidate| {
                        candidate.id == *dep && candidate.state == AutoNodeState::Succeeded
                    })
                })
            })
            .map(|node| node.id.clone())
            .collect();

        for id in &ready_ids {
            if let Some(node) = self.node_mut(id) {
                node.state = AutoNodeState::Ready;
            }
        }

        ready_ids
    }

    pub fn node_mut(&mut self, id: &AutoNodeId) -> Option<&mut AutoNode> {
        self.nodes.iter_mut().find(|node| node.id == *id)
    }

    pub fn is_terminal(&self) -> bool {
        self.nodes.iter().all(|node| {
            matches!(
                node.state,
                AutoNodeState::Succeeded
                    | AutoNodeState::Failed(_)
                    | AutoNodeState::Skipped(_)
                    | AutoNodeState::Cancelled
            )
        })
    }
}

pub fn build_auto_graph(prompt: &str, cfg: &AutoRunConfig, run_id: impl Into<String>) -> AutoGraph {
    let plan = build_auto_plan(prompt, cfg);
    let mut prior_phase_tail: Vec<AutoNodeId> = Vec::new();
    let mut current_phase: Option<AutoPhaseKind> = None;
    let mut current_phase_ids: Vec<AutoNodeId> = Vec::new();
    let mut nodes = Vec::new();

    for (idx, spec) in plan.agents.iter().cloned().enumerate() {
        if current_phase.is_some_and(|phase| phase != spec.phase) {
            prior_phase_tail = current_phase_ids;
            current_phase_ids = Vec::new();
        }
        current_phase = Some(spec.phase);

        let id = AutoNodeId(format!("{}-{}", spec.name, idx + 1));
        let dependencies = prior_phase_tail.clone();
        nodes.push(AutoNode {
            id: id.clone(),
            spec,
            dependencies,
            state: AutoNodeState::Pending,
            failure_policy: AutoFailurePolicy::StopRun,
        });
        current_phase_ids.push(id);
    }

    AutoGraph {
        run_id: run_id.into(),
        user_prompt: plan.user_prompt,
        nodes,
        phase_order: plan.phase_order,
    }
}
