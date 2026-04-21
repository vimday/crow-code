use anyhow::Result;
use crow_materialize::MaterializeConfig;

use crate::config::CrowConfig;
use crate::context;
use crate::epistemic;

/// Pre-warm the Cargo build cache by running `cargo check` on the frozen
/// sandbox. This populates the `CARGO_TARGET_DIR` (keyed to `frozen_root`)
/// with all dependency artifacts so MCTS branches only need incremental
/// recompilation of the patched crate(s).
///
/// Failure is non-fatal: if the warm-up fails (e.g. the project doesn't
pub(crate) async fn warm_build_cache(
    frozen_root: &std::path::Path,
    workspace_root: &std::path::Path,
    profile: &crow_probe::types::ProjectProfile,
    candidate: &crow_probe::types::VerificationCandidate,
    observer: &mut dyn crate::event::EventHandler,
) {
    use std::time::Instant;

    let mut cmd = None;
    match profile.primary_lang.name.as_str() {
        "rust" => {
            let mut c = candidate.command.clone();
            if (c.program == "cargo" || c.program.ends_with("/cargo"))
                && c.args.contains(&"test".to_string())
                && !c.args.contains(&"--no-run".to_string())
            {
                c.args.push("--no-run".to_string());
            }
            // Strip out display colors which pollute verification parsing (just in case)
            if !c.args.iter().any(|a| a.starts_with("--color")) {
                c.args.push("--color=never".to_string());
            }
            cmd = Some(c);
        }
        "typescript" | "javascript" => {
            cmd = Some(crow_probe::VerificationCommand::new(
                "npm",
                vec!["install", "--ignore-scripts"],
            ))
        }
        _ => {}
    };

    let Some(cmd) = cmd else {
        observer.handle_event(crate::event::AgentEvent::Log(format!(
            "    ⏭️  No warm-up cache command configured for language: {}",
            profile.primary_lang.name
        )));
        return;
    };

    observer.handle_event(crate::event::AgentEvent::ActionStart(format!(
        "Pre-warming build cache for {}...",
        profile.primary_lang.name
    )));

    let start = Instant::now();

    // NEW: Bootstrapping cache magic!
    // The previous implementation used an initially EMPTY hash directory, causing a 30s+ cold build.
    // Now, we map the host's actual `target/` directory if it exists, bypassing the cold build instantly!
    let host_target = workspace_root.join("target");
    let crow_target = crow_verifier::executor::compute_target_dir_path(workspace_root);

    let base_cache = if host_target.exists() {
        host_target
    } else {
        crow_target.clone()
    };

    let frozen_cache = crow_verifier::executor::compute_target_dir_path(frozen_root);
    crate::mcts::clone_cache_dir(&base_cache, &frozen_cache).await;

    let exec_config = crow_verifier::ExecutionConfig {
        timeout: std::time::Duration::from_secs(120),
        max_output_bytes: 1024 * 1024,
    };
    let aci_config = crow_verifier::types::AciConfig::compact();

    match crow_verifier::executor::execute(
        frozen_root,
        &cmd,
        &exec_config,
        &aci_config,
        Some(frozen_root), // stable frozen cache key
    )
    .await
    {
        Ok(result) => {
            let elapsed = start.elapsed();
            if result.exit_code == Some(0) {
                // Sync the warmed cache into our isolated global tracker for future runs.
                // We MUST use `crow_target` here, NOT `host_target`, as we do not want to pollute
                // the user's active workspace target/ with our sandbox builds!
                crate::mcts::clone_cache_dir(&frozen_cache, &crow_target).await;
                observer.handle_event(crate::event::AgentEvent::ActionComplete(format!(
                    "Build cache warmed in {:.1}s — MCTS branches will use incremental compilation",
                    elapsed.as_secs_f64()
                )));
            } else {
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "    ⚠️  Warm-up cargo check failed (exit={:?}) in {:.1}s — branches will cold-build",
                    result.exit_code,
                    elapsed.as_secs_f64()
                )));
            }
        }
        Err(e) => {
            observer.handle_event(crate::event::AgentEvent::Log(format!(
                "    ⚠️  Build cache warm-up failed: {e:?} — continuing without cache"
            )));
        }
    }
}

pub(crate) async fn apply_winning_plan(
    cfg: &CrowConfig,
    sandbox_path: &std::path::Path,
    hydrated_plan: &crow_patch::IntentPlan,
    plan_id: &str,
    snapshot_id: &crow_patch::SnapshotId,
    ledger: &std::sync::Mutex<crow_workspace::ledger::EventLedger>,
    observer: &mut dyn crate::event::EventHandler,
) -> Result<()> {
    // ── WriteMode enforcement ────────────────────────────
    match cfg.write_mode {
        crate::config::WriteMode::SandboxOnly => {
            observer.handle_event(crate::event::AgentEvent::Log("  📦 Write mode: sandbox-only — changes remain in sandbox (not applied to workspace)".into()));
            observer.handle_event(crate::event::AgentEvent::Log(
                "     Use CROW_WRITE_MODE=write to enable workspace application.".into(),
            ));
        }
        crate::config::WriteMode::WorkspaceWrite => {
            observer.handle_event(crate::event::AgentEvent::Log(
                "  ✍️  Write mode: workspace-write — applying verified changes to workspace..."
                    .into(),
            ));
            if let Err(e) =
                crow_workspace::applier::apply_sandbox_to_workspace(&cfg.workspace, hydrated_plan)
            {
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "  ❌ Failed to apply to workspace: {e:?}"
                )));
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "     Sandbox remains at: {}",
                    sandbox_path.display()
                )));
                anyhow::bail!("Workspace application failed: {e:?}");
            } else {
                observer.handle_event(crate::event::AgentEvent::Log(
                    "  ✅ Workspace updated successfully.".into(),
                ));
                if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, hydrated_plan)
                {
                    observer.handle_event(crate::event::AgentEvent::Log(format!(
                        "  ⚠️  Could not automatically commit changes: {e}"
                    )));
                } else {
                    observer.handle_event(crate::event::AgentEvent::Log(
                        "  ✅ Changes committed to git timeline.".into(),
                    ));
                }
            }
        }
        crate::config::WriteMode::DangerFullAccess => {
            observer.handle_event(crate::event::AgentEvent::Log(
                "  ⚠️  Write mode: danger-full-access — applying without additional checks..."
                    .into(),
            ));
            if let Err(e) =
                crow_workspace::applier::apply_sandbox_to_workspace(&cfg.workspace, hydrated_plan)
            {
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "  ❌ Failed to apply to workspace: {e:?}"
                )));
                anyhow::bail!("Workspace application failed: {e:?}");
            } else {
                observer.handle_event(crate::event::AgentEvent::Log(
                    "  ✅ Workspace updated.".into(),
                ));
                if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, hydrated_plan)
                {
                    observer.handle_event(crate::event::AgentEvent::Log(format!(
                        "  ⚠️  Could not automatically commit changes: {e}"
                    )));
                } else {
                    observer.handle_event(crate::event::AgentEvent::Log(
                        "  ✅ Changes committed to git timeline.".into(),
                    ));
                }
            }
        }
    }

    if cfg.write_mode != crate::config::WriteMode::SandboxOnly {
        if let Ok(mut l) = ledger.lock() {
            let _ = l
            .append(crow_workspace::ledger::LedgerEvent::PlanApplied {
                plan_id: plan_id.to_string(),
                snapshot_id: snapshot_id.clone(),
                timestamp: chrono::Utc::now(),
            });
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_mcts_crucible(
    mcts_config: &crate::mcts::MctsConfig,
    profile: &crow_probe::types::ProjectProfile,
    candidate: &crow_probe::types::VerificationCandidate,
    workspace_root: &std::path::Path,
    frozen_root: &std::path::Path,
    compiler: &crow_brain::IntentCompiler,
    messages: &mut context::ConversationManager,
    snapshot_id: &crow_patch::SnapshotId,
    mcp_manager: Option<&crate::mcp::McpManager>,
    observer: &mut dyn crate::event::EventHandler,
) -> Result<Option<crate::mcts::BranchOutcome>> {
    // 1. Initial Epistemic Loop (Serial Recon) — with hard timeout
    observer.handle_event(crate::event::AgentEvent::Log(
        "Entering Epistemic Recon Loop (MCTS Pre-exploration)...".into(),
    ));
    let file_state_store = std::sync::Arc::new(crate::file_state::FileStateStore::new());
    let baseline_plan = match tokio::time::timeout(
        std::time::Duration::from_secs(180),
        epistemic::run_epistemic_loop(compiler, messages, frozen_root, mcp_manager, observer, file_state_store),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            observer.handle_event(crate::event::AgentEvent::Error(
                "MCTS pre-exploration timed out after 3 minutes (possible network hang). Aborting."
                    .into(),
            ));
            anyhow::bail!("MCTS pre-exploration timed out after 180s");
        }
    };

    if baseline_plan.operations.is_empty() {
        observer.handle_event(crate::event::AgentEvent::Log(
            "Conversational Intent Detected (No codebase changes proposed)".into(),
        ));
        observer.handle_event(crate::event::AgentEvent::Markdown(
            baseline_plan.rationale.clone(),
        ));
        return Ok(None);
    }

    observer.handle_event(crate::event::AgentEvent::Log(
        "Seeding baseline plan into MCTS branch 0...".into(),
    ));

    // Dynamic MCTS Downgrade for Non-code Changes
    // If the LLM just generated a pure documentation edit or a simple config,
    // there is absolutely zero need to spin up 3 parallel LLMs generating alternative
    // markdown variants and freezing the async pool!
    let mut actual_mcts_config = mcts_config.clone();
    let is_pure_text_change = baseline_plan.operations.iter().all(|op| {
        let path = match op {
            crow_patch::EditOp::Create { path, .. } => path.as_str(),
            crow_patch::EditOp::Modify { path, .. } => path.as_str(),
            crow_patch::EditOp::Delete { path, .. } => path.as_str(),
            crow_patch::EditOp::Rename { from: _, to, .. } => to.as_str(),
        };
        path.ends_with(".md") || path.ends_with(".txt")
    });

    if (is_pure_text_change || !baseline_plan.requires_mcts) && actual_mcts_config.branch_factor > 1
    {
        observer.handle_event(crate::event::AgentEvent::Log("    ⏭️  Baseline plan indicates MCTS bypass (trivial or non-code task). Bypassing parallel diverse search (MCTS downgraded to 1 branch).".into()));
        actual_mcts_config.branch_factor = 1;
    }

    if actual_mcts_config.branch_factor > 1 {
        // Pre-warm the build cache so all MCTS branches start with compiled dependencies.
        warm_build_cache(frozen_root, workspace_root, profile, candidate, observer).await;
    }

    // 2. MCTS Parallel Explore Rounds
    let mat_config = MaterializeConfig {
        source: frozen_root.to_path_buf(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };

    observer.handle_event(crate::event::AgentEvent::Log(format!(
        "Entering MCTS Parallel Crucible ({} branches, {} max rounds)",
        actual_mcts_config.branch_factor, actual_mcts_config.max_rounds
    )));
    let mut current_baseline = baseline_plan;

    for mcts_round in 1..=actual_mcts_config.max_rounds {
        observer.handle_event(crate::event::AgentEvent::Log(format!(
            "▶️ MCTS Round {}/{}",
            mcts_round, actual_mcts_config.max_rounds
        )));

        let mut outcomes = crate::mcts::explore_round(
            &actual_mcts_config,
            compiler,
            &messages.as_messages(),
            current_baseline.clone(),
            frozen_root,
            &mat_config,
            &candidate.command,
            &profile.primary_lang,
            snapshot_id,
        )
        .await;

        if let Some(winner) = crate::mcts::select_winner(&mut outcomes) {
            observer.handle_event(crate::event::AgentEvent::Log(format!(
                "MCTS Branch {} passed on round {}!",
                winner.branch_id, mcts_round
            )));

            // Instead of printing diffuse directly to terminal over Ratatui, log it.
            observer.handle_event(crate::event::AgentEvent::Log(format!(
                "Winning Patch (Branch {}) passed verifier.\nEvidence:\n{}",
                winner.branch_id, winner.log
            )));

            return Ok(Some(winner));
        }

        // All branches failed. Feed diagnostics back and re-derive baseline.
        observer.handle_event(crate::event::AgentEvent::Log(format!(
            "[❗] MCTS Round {mcts_round} failed! Feeding diagnostics back to LLM..."
        )));
        let merged = crate::mcts::merge_diagnostics(&outcomes);
        messages.push_verifier_result("MCTS_AllBranchesFailed", &merged);

        // Re-compile a fresh baseline plan that incorporates the failure
        // feedback. This ensures branch 0 in the next round gets an
        // informed plan instead of repeating the same stale one.
        if mcts_round < actual_mcts_config.max_rounds {
            observer.handle_event(crate::event::AgentEvent::Log(
                "  🧠 Re-deriving baseline plan from failure feedback...".into(),
            ));
            match compiler.compile_action(&messages.as_messages()).await {
                Ok(crow_patch::AgentAction::SubmitPlan { plan }) => {
                    observer.handle_event(crate::event::AgentEvent::Log(
                        "    ✅ New baseline plan generated for next round".into(),
                    ));
                    current_baseline = plan;
                }
                Ok(other) => {
                    // Model wants to do more recon — note it but reuse previous baseline
                    messages.push_assistant(serde_json::to_string(&other).unwrap_or_default());
                    observer.handle_event(crate::event::AgentEvent::Log(format!(
                        "    ⚠️  Model requested {:?} instead of SubmitPlan — reusing previous baseline",
                        match &other {
                            crow_patch::AgentAction::ReadFiles { .. } => "ReadFiles",
                            crow_patch::AgentAction::Recon { .. } => "Recon",
                            _ => "unknown",
                        }
                    )));
                }
                Err(e) => {
                    observer.handle_event(crate::event::AgentEvent::Log(format!(
                        "    ⚠️  Baseline re-derivation failed: {e:?} — reusing previous"
                    )));
                }
            }
        }
    }

    observer.handle_event(crate::event::AgentEvent::Log(format!(
        "MCTS exploration exhausted all {} rounds without finding a passing plan.",
        actual_mcts_config.max_rounds
    )));
    anyhow::bail!(
        "MCTS exploration exhausted all {} rounds without finding a passing plan.",
        actual_mcts_config.max_rounds
    );
}
