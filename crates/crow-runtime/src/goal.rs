//! Goal system for long-running, multi-turn agent tasks.
//!
//! A [`Goal`] represents a high-level objective that may span many agent turns.
//! The [`GoalManager`] tracks the active goal and decides whether to issue
//! continuation prompts after each turn completes.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// GoalStatus
// ---------------------------------------------------------------------------

/// Terminal or in-progress status for a [`Goal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    InProgress,
    Completed,
    Failed,
    BudgetExhausted,
}

impl GoalStatus {
    /// Returns `true` when the goal has reached a terminal state and no
    /// further continuations should be issued.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::InProgress)
    }
}

// ---------------------------------------------------------------------------
// Goal
// ---------------------------------------------------------------------------

/// A single long-running objective together with budget / continuation
/// bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    /// The high-level objective the agent is trying to accomplish.
    pub objective: String,
    /// Current lifecycle status.
    pub status: GoalStatus,
    /// Optional hard token budget for the entire goal.
    pub token_budget: Option<u64>,
    /// Cumulative tokens consumed across all continuations.
    pub accumulated_tokens: u64,
    /// How many continuation turns have been executed so far.
    pub continuation_count: u32,
    /// Upper bound on the number of continuations before the goal is
    /// auto-terminated.
    pub max_continuations: u32,
    /// ISO-8601 timestamp of when the goal was created.
    pub created_at: String,
}

/// Fraction of the token budget at which we start emitting warnings.
const BUDGET_WARNING_THRESHOLD: f64 = 0.80;

impl Goal {
    /// Create a new in-progress goal.
    pub fn new(objective: String, token_budget: Option<u64>) -> Self {
        Self {
            objective,
            status: GoalStatus::InProgress,
            token_budget,
            accumulated_tokens: 0,
            continuation_count: 0,
            max_continuations: 50,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Whether the goal is still eligible for another continuation turn.
    pub fn should_continue(&self) -> bool {
        self.status == GoalStatus::InProgress
            && self.continuation_count < self.max_continuations
    }

    /// Record token usage from a completed turn. Returns `true` when the
    /// budget has been exhausted (and sets the status accordingly).
    pub fn record_tokens(&mut self, tokens: u64) -> bool {
        self.accumulated_tokens += tokens;
        if let Some(budget) = self.token_budget {
            if self.accumulated_tokens >= budget {
                self.status = GoalStatus::BudgetExhausted;
                return true;
            }
        }
        false
    }

    /// Mark the goal as successfully completed.
    pub fn complete(&mut self) {
        self.status = GoalStatus::Completed;
    }

    /// Mark the goal as failed.
    pub fn fail(&mut self) {
        self.status = GoalStatus::Failed;
    }

    /// Build a continuation prompt that reminds the agent of the original
    /// objective and current progress.
    pub fn continuation_prompt(&self) -> String {
        let mut prompt = format!(
            "[GOAL CONTINUATION — turn {continuation}/{max}]\n\
             Objective: {objective}\n\
             Tokens used so far: {accumulated}",
            continuation = self.continuation_count,
            max = self.max_continuations,
            objective = self.objective,
            accumulated = self.accumulated_tokens,
        );

        if let Some(budget) = self.token_budget {
            use std::fmt::Write;
            let remaining = budget.saturating_sub(self.accumulated_tokens);
            let _ = write!(
                prompt,
                " / {budget} (remaining: {remaining})",
            );
        }

        prompt.push_str(
            "\n\nContinue working towards the objective. \
             If the goal is complete, indicate so clearly.",
        );

        prompt
    }

    /// Return a warning string when token usage exceeds
    /// [`BUDGET_WARNING_THRESHOLD`] of the budget. Returns `None` when there
    /// is no budget or usage is still below the threshold.
    pub fn budget_warning_prompt(&self) -> Option<String> {
        let budget = self.token_budget?;
        #[allow(clippy::cast_precision_loss)]
        let usage_ratio = self.accumulated_tokens as f64 / budget as f64;
        if usage_ratio >= BUDGET_WARNING_THRESHOLD {
            let pct = (usage_ratio * 100.0) as u64;
            let remaining = budget.saturating_sub(self.accumulated_tokens);
            Some(format!(
                "[BUDGET WARNING] You have used {pct}% of your token budget \
                 ({accumulated}/{budget}). {remaining} tokens remaining. \
                 Wrap up your current task soon.",
                accumulated = self.accumulated_tokens,
            ))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// GoalManager
// ---------------------------------------------------------------------------

/// Session-level manager that holds the currently active [`Goal`] and drives
/// the continuation loop.
pub struct GoalManager {
    active_goal: Option<Goal>,
}

impl GoalManager {
    /// Create a manager with no active goal.
    pub fn new() -> Self {
        Self { active_goal: None }
    }

    /// Set (or replace) the active goal.
    pub fn set_goal(&mut self, goal: Goal) {
        self.active_goal = Some(goal);
    }

    /// Immutable reference to the active goal, if any.
    pub fn active_goal(&self) -> Option<&Goal> {
        self.active_goal.as_ref()
    }

    /// Mutable reference to the active goal, if any.
    pub fn active_goal_mut(&mut self) -> Option<&mut Goal> {
        self.active_goal.as_mut()
    }

    /// Remove the active goal.
    pub fn clear_goal(&mut self) {
        self.active_goal = None;
    }

    /// Called after each agent turn completes. Records token usage,
    /// increments the continuation counter, and returns
    /// `Some(continuation_prompt)` when the goal should keep going.
    pub fn on_turn_complete(&mut self, tokens_used: u64) -> Option<String> {
        let goal = self.active_goal.as_mut()?;
        goal.record_tokens(tokens_used);
        goal.continuation_count += 1;

        if goal.should_continue() {
            Some(goal.continuation_prompt())
        } else {
            None
        }
    }
}

impl Default for GoalManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn new_goal_is_in_progress() {
        let goal = Goal::new("build the thing".into(), Some(10_000));
        assert_eq!(goal.status, GoalStatus::InProgress);
        assert!(goal.should_continue());
    }

    #[test]
    fn budget_exhaustion_sets_status() {
        let mut goal = Goal::new("test".into(), Some(100));
        let exhausted = goal.record_tokens(100);
        assert!(exhausted);
        assert_eq!(goal.status, GoalStatus::BudgetExhausted);
        assert!(!goal.should_continue());
    }

    #[test]
    fn no_budget_never_exhausts() {
        let mut goal = Goal::new("test".into(), None);
        let exhausted = goal.record_tokens(u64::MAX);
        assert!(!exhausted);
        assert!(goal.should_continue());
    }

    #[test]
    fn max_continuations_respected() {
        let mut goal = Goal::new("test".into(), None);
        goal.continuation_count = 50;
        assert!(!goal.should_continue());
    }

    #[test]
    fn manager_on_turn_complete_drives_continuations() {
        let mut mgr = GoalManager::new();
        assert!(mgr.on_turn_complete(100).is_none());

        mgr.set_goal(Goal::new("do stuff".into(), Some(500)));
        let prompt = mgr.on_turn_complete(100);
        assert!(prompt.is_some());
        assert!(prompt.unwrap().contains("do stuff"));

        // Exhaust budget
        let prompt = mgr.on_turn_complete(400);
        assert!(prompt.is_none());
        assert_eq!(
            mgr.active_goal().unwrap().status,
            GoalStatus::BudgetExhausted,
        );
    }

    #[test]
    fn budget_warning_at_threshold() {
        let mut goal = Goal::new("test".into(), Some(1000));
        goal.record_tokens(799);
        assert!(goal.budget_warning_prompt().is_none());

        goal.record_tokens(1);
        assert!(goal.budget_warning_prompt().is_some());
    }

    #[test]
    fn terminal_status_checks() {
        assert!(!GoalStatus::InProgress.is_terminal());
        assert!(GoalStatus::Completed.is_terminal());
        assert!(GoalStatus::Failed.is_terminal());
        assert!(GoalStatus::BudgetExhausted.is_terminal());
    }
}
