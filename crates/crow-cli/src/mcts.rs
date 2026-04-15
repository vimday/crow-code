//! Parallel MCTS (Monte Carlo Tree Search) crucible engine.
//!
//! Replaces the serial `for attempt in 1..=3` loop with concurrent
//! multi-branch exploration. Each branch gets its own sandbox, applies
//! its own patch, and verifies independently. The first branch to pass
//! wins; if none pass, diagnostics are merged for the next round.
//!
//! # Economic Gate
//!
//! MCTS multiplies LLM calls by the branch factor. This module should
//! only be enabled when prompt caching is active (90% input cost reduction),
//! making parallel calls economically viable.

use crow_brain::ChatMessage;
use crow_evidence::TestOutcome;
use crow_materialize::{materialize, MaterializeConfig, SandboxHandle};
use crow_patch::IntentPlan;
use crow_verifier::types::{AciConfig, ExecutionConfig};
use std::path::Path;

// ─── Configuration ──────────────────────────────────────────────────

/// Configuration for parallel MCTS exploration.
#[derive(Debug, Clone)]
pub struct MctsConfig {
    /// Number of parallel branches per round (default: 3).
    pub branch_factor: usize,
    /// Maximum MCTS rounds before giving up (default: 2).
    pub max_rounds: usize,
    /// LLM temperature for branch diversity (default: 0.8).
    pub temperature: f64,
}

impl Default for MctsConfig {
    fn default() -> Self {
        Self {
            branch_factor: 3,
            max_rounds: 2,
            temperature: 0.8,
        }
    }
}

impl MctsConfig {
    /// Load from environment variables with defaults.
    pub fn from_env() -> Self {
        let branch_factor = std::env::var("CROW_MCTS_BRANCHES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3_usize)
            .clamp(1, 8);

        let max_rounds = std::env::var("CROW_MCTS_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2_usize)
            .clamp(1, 5);

        let temperature = std::env::var("CROW_MCTS_TEMPERATURE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.8_f64)
            .clamp(0.0, 2.0);

        Self {
            branch_factor,
            max_rounds,
            temperature,
        }
    }

    /// Returns true if MCTS is effectively disabled (single branch).
    pub fn is_serial(&self) -> bool {
        self.branch_factor <= 1
    }
}

// ─── Branch Result ──────────────────────────────────────────────────

/// The outcome of a single MCTS branch.
pub struct BranchOutcome {
    /// Which branch index (0-based) produced this result.
    pub branch_id: usize,
    /// The compiled plan that was applied.
    #[allow(dead_code)]
    pub plan: IntentPlan,
    /// The sandbox where the plan was applied and verified.
    #[allow(dead_code)]
    pub sandbox: SandboxHandle,
    /// Whether verification passed.
    pub passed: bool,
    /// Verification log for diagnostic feedback.
    pub log: String,
}

// ─── Exploration Engine ─────────────────────────────────────────────

/// Run parallel MCTS exploration for a single round.
///
/// Spawns `branch_factor` concurrent pipelines:
///   LLM generate → hydrate → apply → preflight → verify
///
/// Returns the first passing branch, or all branch outcomes if none pass.
pub async fn explore_round(
    config: &MctsConfig,
    messages: &[ChatMessage],
    frozen_root: &Path,
    mat_config: &MaterializeConfig,
    verify_command: &crow_probe::VerificationCommand,
) -> Vec<BranchOutcome> {
    use tokio::task::JoinSet;

    let mut join_set = JoinSet::new();

    // Launch N branches concurrently.
    for branch_id in 0..config.branch_factor {
        // Each branch gets its own cloned context.
        let _msgs = messages.to_vec();
        let frozen = frozen_root.to_path_buf();
        let mat_cfg = mat_config.clone();
        let cmd = verify_command.clone();
        let _temperature = config.temperature;

        join_set.spawn(async move { run_branch(branch_id, &frozen, &mat_cfg, &cmd).await });
    }

    // Collect results as they complete.
    let mut outcomes = Vec::with_capacity(config.branch_factor);
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => {
                eprintln!("    ⚠️  MCTS branch panicked: {:?}", e);
            }
        }
    }

    outcomes
}

/// Execute a single MCTS branch: materialize → compile → hydrate → apply → verify.
async fn run_branch(
    branch_id: usize,
    frozen_root: &Path,
    mat_config: &MaterializeConfig,
    verify_command: &crow_probe::VerificationCommand,
) -> BranchOutcome {
    // Step 1: Materialize a fresh sandbox from the frozen baseline.
    let sandbox = match tokio::task::spawn_blocking({
        let cfg = mat_config.clone();
        move || materialize(&cfg)
    })
    .await
    {
        Ok(Ok(sb)) => sb,
        Ok(Err(e)) => {
            return BranchOutcome {
                branch_id,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: format!("Materialization failed: {:?}", e),
            };
        }
        Err(e) => {
            return BranchOutcome {
                branch_id,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: format!("Materialization panicked: {:?}", e),
            };
        }
    };

    // NOTE: In the full implementation, steps 2-5 would be:
    //   2. LLM generate with temperature (compiler.compile_action_with_temp)
    //   3. Hydrate the plan against the sandbox
    //   4. Apply the plan to the sandbox
    //   5. Run preflight + verification
    //
    // For now, this is the structural skeleton. The actual LLM call integration
    // requires threading the compiler (which holds `Box<dyn LlmClient>` — not
    // Send across spawn boundaries without `Arc`). This will be wired up when
    // the compiler is refactored to use `Arc<dyn LlmClient>` in a follow-up.

    // Placeholder: run verification on the unmodified sandbox to validate the pipeline.
    let exec_config = ExecutionConfig {
        timeout: std::time::Duration::from_secs(60),
        max_output_bytes: 5 * 1024 * 1024,
    };

    let result = crow_verifier::executor::execute(
        sandbox.path(),
        verify_command,
        &exec_config,
        &AciConfig::compact(),
        Some(frozen_root),
    )
    .await;

    match result {
        Ok(r) => BranchOutcome {
            branch_id,
            plan: empty_plan(),
            sandbox,
            passed: r.test_run.outcome == TestOutcome::Passed,
            log: r.test_run.truncated_log,
        },
        Err(e) => BranchOutcome {
            branch_id,
            plan: empty_plan(),
            sandbox,
            passed: false,
            log: format!("Verification error: {:?}", e),
        },
    }
}

/// Select the winning branch from MCTS outcomes.
/// Returns the first passing branch, or None if all failed.
pub fn select_winner(outcomes: &mut Vec<BranchOutcome>) -> Option<BranchOutcome> {
    outcomes
        .iter()
        .position(|o| o.passed)
        .map(|pos| outcomes.remove(pos))
}

/// Merge failure diagnostics from all branches into a single feedback string.
pub fn merge_diagnostics(outcomes: &[BranchOutcome]) -> String {
    let mut out = String::from("[MCTS: All branches failed]\n\n");
    for o in outcomes {
        let snippet = safe_truncate(&o.log, 500);
        let ellipsis = if snippet.len() < o.log.len() {
            "..."
        } else {
            ""
        };
        out.push_str(&format!(
            "Branch {}: {}{}\n",
            o.branch_id, snippet, ellipsis
        ));
    }
    out
}

// ─── Helpers ────────────────────────────────────────────────────────

/// UTF-8 safe truncation — never panics on multibyte boundaries.
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn empty_plan() -> IntentPlan {
    IntentPlan {
        base_snapshot_id: crow_patch::SnapshotId("mcts-placeholder".into()),
        rationale: "MCTS branch placeholder".into(),
        is_partial: true,
        confidence: crow_patch::Confidence::None,
        operations: vec![],
    }
}

/// Create a temporary sandbox handle that points to /tmp and won't clean up.
fn dummy_sandbox() -> SandboxHandle {
    let path = std::env::temp_dir().join("crow_mcts_dummy");
    let _ = std::fs::create_dir_all(&path);
    crow_materialize::SandboxHandle::non_owning_view_from(
        path,
        crow_materialize::MaterializationDriver::SafeCopy,
    )
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        let cfg = MctsConfig::default();
        assert_eq!(cfg.branch_factor, 3);
        assert_eq!(cfg.max_rounds, 2);
        assert!((cfg.temperature - 0.8).abs() < f64::EPSILON);
        assert!(!cfg.is_serial());
    }

    #[test]
    fn config_serial_when_single_branch() {
        let cfg = MctsConfig {
            branch_factor: 1,
            ..Default::default()
        };
        assert!(cfg.is_serial());
    }

    #[test]
    fn select_winner_picks_first_passing() {
        let mut outcomes = vec![
            BranchOutcome {
                branch_id: 0,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: "failed".into(),
            },
            BranchOutcome {
                branch_id: 1,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: true,
                log: "passed".into(),
            },
            BranchOutcome {
                branch_id: 2,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: true,
                log: "also passed".into(),
            },
        ];

        let winner = select_winner(&mut outcomes).unwrap();
        assert_eq!(winner.branch_id, 1);
        assert!(winner.passed);
    }

    #[test]
    fn select_winner_returns_none_when_all_fail() {
        let mut outcomes = vec![BranchOutcome {
            branch_id: 0,
            plan: empty_plan(),
            sandbox: dummy_sandbox(),
            passed: false,
            log: "failed".into(),
        }];

        assert!(select_winner(&mut outcomes).is_none());
    }

    #[test]
    fn merge_diagnostics_formats_all_branches() {
        let outcomes = vec![
            BranchOutcome {
                branch_id: 0,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: "error A".into(),
            },
            BranchOutcome {
                branch_id: 1,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: "error B".into(),
            },
        ];

        let merged = merge_diagnostics(&outcomes);
        assert!(merged.contains("All branches failed"));
        assert!(merged.contains("Branch 0: error A"));
        assert!(merged.contains("Branch 1: error B"));
    }
}
