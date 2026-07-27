//! Deterministic auto-mode orchestration planning.
//!
//! The auto planner turns one user task into a bounded Codex-style loop:
//! Explore → Plan → Execute → Review → Verify. It intentionally produces a
//! static plan so the TUI, runtime, and tests can reason about orchestration
//! without asking the model to invent process control at runtime.

use crate::registry::AgentTaskKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStrategy {
    Fast,
    Balanced,
    Thorough,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoRunConfig {
    pub max_parallel_agents: usize,
    pub max_agent_depth: usize,
    pub strategy: AutoStrategy,
}

impl Default for AutoRunConfig {
    fn default() -> Self {
        Self {
            max_parallel_agents: 4,
            max_agent_depth: 2,
            strategy: AutoStrategy::Balanced,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoPhaseKind {
    Explore,
    Plan,
    Execute,
    Review,
    Verify,
}

impl AutoPhaseKind {
    pub fn task_kind(self) -> AgentTaskKind {
        match self {
            Self::Explore => AgentTaskKind::Explore,
            Self::Plan => AgentTaskKind::Plan,
            Self::Execute => AgentTaskKind::Execute,
            Self::Review => AgentTaskKind::Review,
            Self::Verify => AgentTaskKind::Verify,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explore => "Explore",
            Self::Plan => "Plan",
            Self::Execute => "Execute",
            Self::Review => "Review",
            Self::Verify => "Verify",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoAgentSpec {
    pub name: String,
    pub role: String,
    pub phase: AutoPhaseKind,
    pub prompt: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoPlan {
    pub user_prompt: String,
    pub agents: Vec<AutoAgentSpec>,
    pub phase_order: Vec<AutoPhaseKind>,
}

pub fn build_auto_plan(prompt: &str, cfg: &AutoRunConfig) -> AutoPlan {
    let mut agents = vec![
        AutoAgentSpec {
            name: "explorer".to_string(),
            role: "explorer".to_string(),
            phase: AutoPhaseKind::Explore,
            prompt: format!(
                "Explore the codebase for this task. Return concrete files, existing patterns, and risks. Task:\n{prompt}"
            ),
            read_only: true,
        },
        AutoAgentSpec {
            name: "planner".to_string(),
            role: "planner".to_string(),
            phase: AutoPhaseKind::Plan,
            prompt: format!(
                "Create a concise implementation plan with files, tests, and verification commands. Task:\n{prompt}"
            ),
            read_only: true,
        },
        AutoAgentSpec {
            name: "executor".to_string(),
            role: "executor".to_string(),
            phase: AutoPhaseKind::Execute,
            prompt: format!(
                "Implement the approved plan safely. Reuse existing code and keep changes focused. Task:\n{prompt}"
            ),
            read_only: false,
        },
        AutoAgentSpec {
            name: "reviewer".to_string(),
            role: "reviewer".to_string(),
            phase: AutoPhaseKind::Review,
            prompt: format!(
                "Review the implementation for correctness, simplification, security, and test gaps. Task:\n{prompt}"
            ),
            read_only: true,
        },
        AutoAgentSpec {
            name: "verifier".to_string(),
            role: "reviewer".to_string(),
            phase: AutoPhaseKind::Verify,
            prompt: format!(
                "Verify the final implementation with targeted commands and summarize pass/fail evidence. Task:\n{prompt}"
            ),
            read_only: true,
        },
    ];

    match cfg.strategy {
        AutoStrategy::Fast => {
            agents.retain(|a| matches!(a.phase, AutoPhaseKind::Execute | AutoPhaseKind::Verify));
        }
        AutoStrategy::Balanced => {}
        AutoStrategy::Thorough => {
            agents.insert(
                1,
                AutoAgentSpec {
                    name: "architecture-scout".to_string(),
                    role: "architect".to_string(),
                    phase: AutoPhaseKind::Explore,
                    prompt: format!(
                        "Analyze architecture boundaries and propose the smallest high-leverage refactor path. Task:\n{prompt}"
                    ),
                    read_only: true,
                },
            );
        }
    }

    agents.truncate(cfg.max_parallel_agents.max(1));

    let mut phase_order = Vec::new();
    for phase in [
        AutoPhaseKind::Explore,
        AutoPhaseKind::Plan,
        AutoPhaseKind::Execute,
        AutoPhaseKind::Review,
        AutoPhaseKind::Verify,
    ] {
        if agents.iter().any(|agent| agent.phase == phase) {
            phase_order.push(phase);
        }
    }

    AutoPlan {
        user_prompt: prompt.to_string(),
        agents,
        phase_order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_plan_contains_world_class_loop() {
        let plan = build_auto_plan("refactor tui", &AutoRunConfig::default());
        let phases: Vec<_> = plan.agents.iter().map(|a| a.phase).collect();
        assert!(phases.contains(&AutoPhaseKind::Explore));
        assert!(phases.contains(&AutoPhaseKind::Plan));
        assert!(phases.contains(&AutoPhaseKind::Execute));
        assert!(phases.contains(&AutoPhaseKind::Review));
        assert_eq!(
            plan.phase_order,
            vec![
                AutoPhaseKind::Explore,
                AutoPhaseKind::Plan,
                AutoPhaseKind::Execute,
                AutoPhaseKind::Review
            ]
        );
    }

    #[test]
    fn thorough_plan_adds_architecture_scout() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 8,
            max_agent_depth: 2,
            strategy: AutoStrategy::Thorough,
        };
        let plan = build_auto_plan("refactor tui", &cfg);
        assert!(plan
            .agents
            .iter()
            .any(|agent| agent.name == "architecture-scout"));
        assert!(plan
            .agents
            .iter()
            .any(|agent| agent.phase == AutoPhaseKind::Verify));
    }

    #[test]
    fn fast_plan_is_capped_and_action_oriented() {
        let cfg = AutoRunConfig {
            max_parallel_agents: 2,
            max_agent_depth: 1,
            strategy: AutoStrategy::Fast,
        };
        let plan = build_auto_plan("fix bug", &cfg);
        assert!(plan.agents.len() <= 2);
        assert!(plan.agents.iter().any(|a| !a.read_only));
    }
}
