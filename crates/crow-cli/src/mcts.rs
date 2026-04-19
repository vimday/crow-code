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
/// `baseline_plan` seeds branch 0 with a plan the epistemic loop already
/// produced. This avoids the cost of a redundant LLM call and ensures
/// the first valid idea is never thrown away. Branches 1..N call the
/// LLM with elevated temperature for diversity.
///
/// Returns outcomes for all branches (callers use `select_winner`).
#[allow(clippy::too_many_arguments)]
pub async fn explore_round(
    config: &MctsConfig,
    compiler: &crow_brain::IntentCompiler,
    messages: &[ChatMessage],
    baseline_plan: IntentPlan,
    frozen_root: &Path,
    mat_config: &MaterializeConfig,
    verify_command: &crow_probe::VerificationCommand,
    lang: &crow_probe::DetectedLanguage,
    snapshot_id: &crow_patch::SnapshotId,
) -> Vec<BranchOutcome> {
    use tokio::task::JoinSet;

    let mut join_set = JoinSet::new();

    // Branch 0: use the baseline plan (no LLM call).
    {
        let frozen = frozen_root.to_path_buf();
        let mat_cfg = mat_config.clone();
        let cmd = verify_command.clone();
        let plan = baseline_plan.clone();
        let lang_clone = lang.clone();
        let snap_clone = snapshot_id.clone();

        join_set.spawn(async move {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                run_branch_with_plan(0, plan, &frozen, &mat_cfg, &cmd, &lang_clone, &snap_clone),
            ).await {
                Ok(outcome) => outcome,
                Err(_) => BranchOutcome {
                    branch_id: 0,
                    plan: empty_plan(),
                    sandbox: dummy_sandbox(),
                    passed: false,
                    log: "Branch 0 timed out after 120s".into(),
                },
            }
        });
    }

    // Branches 1..N: generate fresh plans with temperature.
    for branch_id in 1..config.branch_factor {
        let msgs = messages.to_vec();
        let frozen = frozen_root.to_path_buf();
        let mat_cfg = mat_config.clone();
        let cmd = verify_command.clone();
        let comp = compiler.clone();
        let temperature = config.temperature;
        let lang_clone = lang.clone();
        let snap_clone = snapshot_id.clone();

        join_set.spawn(async move {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                run_branch(
                    branch_id,
                    &comp,
                    &msgs,
                    temperature,
                    &frozen,
                    &mat_cfg,
                    &cmd,
                    &lang_clone,
                    &snap_clone,
                ),
            ).await {
                Ok(outcome) => outcome,
                Err(_) => BranchOutcome {
                    branch_id,
                    plan: empty_plan(),
                    sandbox: dummy_sandbox(),
                    passed: false,
                    log: format!("Branch {} timed out after 120s (likely network hang)", branch_id),
                },
            }
        });
    }

    // Collect results as they complete. If any branch passes, abort
    // remaining branches immediately (early termination) to save LLM
    // and compute costs.
    let mut outcomes = Vec::with_capacity(config.branch_factor);
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(outcome) => {
                let is_winner = outcome.passed;
                outcomes.push(outcome);
                if is_winner {
                    let remaining = join_set.len();
                    if remaining > 0 {
                        println!(
                            "    ⚡ Early termination: aborting {} remaining branch(es)",
                            remaining
                        );
                        join_set.abort_all();
                    }
                    break;
                }
            }
            Err(e) => {
                if e.is_cancelled() {
                    // Expected from abort_all(); don't report as error.
                    continue;
                }
                eprintln!("    ⚠️  MCTS branch panicked: {:?}", e);
            }
        }
    }

    outcomes
}

/// Execute a single MCTS branch: materialize → compile → hydrate → apply → verify.
#[allow(clippy::too_many_arguments)]
async fn run_branch(
    branch_id: usize,
    compiler: &crow_brain::IntentCompiler,
    messages: &[ChatMessage],
    temperature: f64,
    frozen_root: &Path,
    mat_config: &MaterializeConfig,
    verify_command: &crow_probe::VerificationCommand,
    lang: &crow_probe::DetectedLanguage,
    snapshot_id: &crow_patch::SnapshotId,
) -> BranchOutcome {
    let branch_start = std::time::Instant::now();

    // MCTS Alignment: Append a deterministic suffix to the very last message.
    // The entire conversation history is structurally 100% identical across all branches,
    // ensuring massive Anthropic cache hits. Only the final few tokens differ to force diversity.
    let mut aligned_messages = messages.to_vec();
    if let Some(last) = aligned_messages.last_mut() {
        last.content
            .push_str(&format!("\n\n[MCTS EXPLORATION ARM: {}]", branch_id));
    }

    // LLM generate with temperature for diversity.
    let action = match compiler
        .compile_action_with_temperature(&aligned_messages, temperature)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            return BranchOutcome {
                branch_id,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: format!("LLM generation failed: {:?}", e),
            };
        }
    };

    let plan = match action {
        crow_patch::AgentAction::SubmitPlan { plan } => plan,
        other => {
            return BranchOutcome {
                branch_id,
                plan: empty_plan(),
                sandbox: dummy_sandbox(),
                passed: false,
                log: format!("Branch produced non-SubmitPlan action: {:?}", other),
            };
        }
    };

    println!(
        "    Branch {}: LLM completed in {:.1}s",
        branch_id,
        branch_start.elapsed().as_secs_f64()
    );

    // Fast-Pruning: Pre-hydrate the plan against the *frozen* root before doing any disk I/O to
    // materialize a full sandbox. If the LLM hallucinated files or broke checksums, abort early!
    if let Err(e) = crow_workspace::PlanHydrator::hydrate(&plan, snapshot_id, frozen_root) {
        return BranchOutcome {
            branch_id,
            plan,
            sandbox: dummy_sandbox(),
            passed: false,
            log: format!("Early Pruning: Syntactic invalidity or out-of-bounds mutation detected against frozen root: {:?}", e),
        };
    }

    // Only fully materialize if we passed early pruning
    let sandbox = match materialize_sandbox(branch_id, mat_config).await {
        Ok(sb) => sb,
        Err(outcome) => return outcome,
    };

    let outcome = evaluate_plan(
        branch_id,
        plan,
        sandbox,
        frozen_root,
        verify_command,
        lang,
        snapshot_id,
    )
    .await;
    println!(
        "    Branch {}: total {:.1}s — {}",
        branch_id,
        branch_start.elapsed().as_secs_f64(),
        if outcome.passed {
            "✅ PASSED"
        } else {
            "❌ FAILED"
        }
    );
    outcome
}

/// Branch 0 fast-path: use a pre-existing plan (no LLM call).
async fn run_branch_with_plan(
    branch_id: usize,
    plan: IntentPlan,
    frozen_root: &Path,
    mat_config: &MaterializeConfig,
    verify_command: &crow_probe::VerificationCommand,
    lang: &crow_probe::DetectedLanguage,
    snapshot_id: &crow_patch::SnapshotId,
) -> BranchOutcome {
    let branch_start = std::time::Instant::now();
    let sandbox = match materialize_sandbox(branch_id, mat_config).await {
        Ok(sb) => sb,
        Err(outcome) => return outcome,
    };

    let outcome = evaluate_plan(
        branch_id,
        plan,
        sandbox,
        frozen_root,
        verify_command,
        lang,
        snapshot_id,
    )
    .await;
    println!(
        "    Branch {}: total {:.1}s — {}",
        branch_id,
        branch_start.elapsed().as_secs_f64(),
        if outcome.passed {
            "✅ PASSED"
        } else {
            "❌ FAILED"
        }
    );
    outcome
}

// ─── Shared Pipeline Stages ─────────────────────────────────────────

/// Clone the target cache directory to avoid lock contention between parallel branches.
pub async fn clone_cache_dir(src: &Path, dst: &Path) {
    if !src.exists() {
        return;
    }

    if dst.exists() {
        let _ = tokio::fs::remove_dir_all(dst).await;
    }

    #[cfg(target_os = "macos")]
    {
        // Try `cp -cR` for fast APFS clone
        let status = tokio::process::Command::new("cp")
            .arg("-cR")
            .arg(src)
            .arg(dst)
            .status()
            .await;
        if let Ok(st) = status {
            if st.success() {
                return;
            } else {
                eprintln!("    ⚠️  macOS fast path clone failed with exit code {:?}, falling back to cp -a", st.code());
            }
        } else if let Err(e) = status {
            eprintln!(
                "    ⚠️  macOS fast path clone execution failed: {}, falling back to cp -a",
                e
            );
        }
    }

    // Fallback: standard recursive copy.
    match tokio::process::Command::new("cp")
        .arg("-a")
        .arg(src)
        .arg(dst)
        .status()
        .await
    {
        Ok(st) => {
            if !st.success() {
                eprintln!("    ⚠️  Cache clone failed with exit code {:?} — MCTS branch will use cold cache", st.code());
            }
        }
        Err(e) => {
            eprintln!("    ⚠️  Failed to execute cache clone command: {} — MCTS branch will use cold cache", e);
        }
    }
}

/// Materialize a fresh sandbox. Returns Ok(sandbox) or Err(BranchOutcome).
async fn materialize_sandbox(
    branch_id: usize,
    mat_config: &MaterializeConfig,
) -> Result<SandboxHandle, BranchOutcome> {
    match tokio::task::spawn_blocking({
        let cfg = mat_config.clone();
        move || materialize(&cfg)
    })
    .await
    {
        Ok(Ok(sb)) => Ok(sb),
        Ok(Err(e)) => Err(BranchOutcome {
            branch_id,
            plan: empty_plan(),
            sandbox: dummy_sandbox(),
            passed: false,
            log: format!("Materialization failed: {:?}", e),
        }),
        Err(e) => Err(BranchOutcome {
            branch_id,
            plan: empty_plan(),
            sandbox: dummy_sandbox(),
            passed: false,
            log: format!("Materialization panicked: {:?}", e),
        }),
    }
}

/// Hydrate → apply → preflight → verify pipeline shared by all branches.
async fn evaluate_plan(
    branch_id: usize,
    plan: IntentPlan,
    sandbox: SandboxHandle,
    frozen_root: &Path,
    verify_command: &crow_probe::VerificationCommand,
    lang: &crow_probe::DetectedLanguage,
    snapshot_id: &crow_patch::SnapshotId,
) -> BranchOutcome {
    // Hydrate plan
    let sandbox_path = sandbox.path().to_path_buf();
    let sandbox_path_clone = sandbox_path.clone();
    let snap_for_eval = snapshot_id.clone();
    let plan_clone = plan.clone();
    let hydrate_result = tokio::task::spawn_blocking(move || {
        crow_workspace::PlanHydrator::hydrate(&plan_clone, &snap_for_eval, &sandbox_path_clone)
    })
    .await;
    let hydrated_plan = match hydrate_result {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            return BranchOutcome {
                branch_id,
                plan,
                sandbox,
                passed: false,
                log: format!("Hydration failed: {:?}", e),
            };
        }
        Err(e) => {
            return BranchOutcome {
                branch_id,
                plan,
                sandbox,
                passed: false,
                log: format!("Hydration task panicked: {:?}", e),
            };
        }
    };

    // Apply plan
    {
        let plan_for_apply = hydrated_plan.clone();
        let sandbox_view = sandbox.non_owning_view();
        let apply_result = tokio::task::spawn_blocking(move || {
            crow_workspace::applier::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
        })
        .await;
        match apply_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return BranchOutcome {
                    branch_id,
                    plan: hydrated_plan,
                    sandbox,
                    passed: false,
                    log: format!("Sandbox patch injection failed: {:?}", e),
                };
            }
            Err(e) => {
                return BranchOutcome {
                    branch_id,
                    plan: hydrated_plan,
                    sandbox,
                    passed: false,
                    log: format!("Apply task panicked: {:?}", e),
                };
            }
        }
    }

    // NEW: Branch specific cache copying
    // Clone the `frozen_root`'s baseline cache into the `sandbox.path()`'s unique cache
    // to avoid Cargo lock contention during parallel evaluation!
    let baseline_cache = crow_verifier::executor::compute_target_dir_path(frozen_root);
    let branch_cache = crow_verifier::executor::compute_target_dir_path(sandbox.path());
    clone_cache_dir(&baseline_cache, &branch_cache).await;

    // Preflight
    let preflight_result = crow_verifier::preflight::run_preflight(
        &sandbox_path,
        Some(sandbox.path()),
        std::time::Duration::from_secs(60),
        lang,
    )
    .await;

    if let crow_verifier::preflight::PreflightResult::Errors(diags) = preflight_result {
        let summary = crow_verifier::preflight::format_diagnostics(&diags);
        return BranchOutcome {
            branch_id,
            plan: hydrated_plan,
            sandbox,
            passed: false,
            log: format!("Preflight compile failed:\n{}", summary),
        };
    }

    // Full Verification
    let exec_config = crow_verifier::ExecutionConfig {
        timeout: std::time::Duration::from_secs(60),
        max_output_bytes: 5 * 1024 * 1024,
    };

    let result = crow_verifier::executor::execute(
        sandbox.path(),
        verify_command,
        &exec_config,
        &crow_verifier::types::AciConfig::compact(),
        Some(sandbox.path()), // Use branch cache!
    )
    .await;

    match result {
        Ok(r) => BranchOutcome {
            branch_id,
            plan: hydrated_plan,
            sandbox,
            passed: r.test_run.outcome == TestOutcome::Passed,
            log: r.test_run.truncated_log,
        },
        Err(e) => BranchOutcome {
            branch_id,
            plan: hydrated_plan,
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

/// Merge failure diagnostics from all branches into structured LLM feedback.
///
/// Produces a categorized summary so the model can distinguish between
/// compile errors (fix the syntax), test failures (fix the logic), and
/// infrastructure issues (ignore and retry).
pub fn merge_diagnostics(outcomes: &[BranchOutcome]) -> String {
    let total = outcomes.len();
    let mut out = format!(
        "[MCTS ROUND FAILED — {}/{} branches failed]\n\n",
        outcomes.iter().filter(|o| !o.passed).count(),
        total
    );

    // Categorize failures by stage for clearer feedback
    let mut compile_failures = Vec::new();
    let mut test_failures = Vec::new();
    let mut infra_failures = Vec::new();

    for o in outcomes {
        if o.passed {
            continue;
        }
        let snippet = safe_truncate(&o.log, 800);
        let entry = format!("Branch {}: {}", o.branch_id, snippet);

        if o.log.contains("COMPILE ERRORS") || o.log.contains("Preflight compile failed") {
            compile_failures.push(entry);
        } else if o.log.contains("Verification error")
            || o.log.contains("test result: FAILED")
            || o.log.starts_with("test ")
        {
            test_failures.push(entry);
        } else {
            infra_failures.push(entry);
        }
    }

    if !compile_failures.is_empty() {
        out.push_str("── Compile Errors (fix syntax/types first) ──\n");
        for f in &compile_failures {
            out.push_str(f);
            out.push('\n');
        }
        out.push('\n');
    }

    if !test_failures.is_empty() {
        out.push_str("── Test Failures (logic errors) ──\n");
        for f in &test_failures {
            out.push_str(f);
            out.push('\n');
        }
        out.push('\n');
    }

    if !infra_failures.is_empty() {
        out.push_str("── Other (materialization/hydration/infra) ──\n");
        for f in &infra_failures {
            out.push_str(f);
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("Please analyze the errors above and produce a corrected submit_plan.\n");
    out
}

// ─── Helpers ────────────────────────────────────────────────────────

/// UTF-8 safe truncation — delegates to the shared implementation.
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    crow_patch::safe_truncate(s, max_bytes)
}

fn empty_plan() -> IntentPlan {
    IntentPlan {
        base_snapshot_id: crow_patch::SnapshotId("pending".into()),
        rationale: "MCTS branch placeholder".into(),
        is_partial: true,
        confidence: crow_patch::Confidence::None,
        requires_mcts: true,
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
        assert!(merged.contains("MCTS ROUND FAILED"));
        assert!(merged.contains("2/2 branches failed"));
        assert!(merged.contains("Branch 0: error A"));
        assert!(merged.contains("Branch 1: error B"));
        assert!(merged.contains("produce a corrected submit_plan"));
    }
}
