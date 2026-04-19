//! The Serial Crucible execution loop.
//!
//! Orchestrates the end-to-end verification and code application flow for a single turn,
//! handling sandbox materialization, compilation preflights, and test execution.

use crate::config::CrowConfig;
use crate::context::ConversationManager;
use crate::epistemic;
use anyhow::{Context, Result};
use crow_brain::IntentCompiler;
use crow_materialize::MaterializeConfig;
use crow_patch::SnapshotId;
use crow_probe::types::{ProjectProfile, VerificationCandidate};
use crow_workspace::ledger::EventLedger;
use std::path::Path;

/// Contains the static references and environment required to safely orchestrate
/// a sandbox evaluation epoch.
pub struct SerialCrucible<'a> {
    /// Workspace configuration and bounds.
    pub cfg: &'a CrowConfig,
    /// Profile of the current workspace containing ignore specs.
    pub profile: &'a ProjectProfile,
    /// The target verification command to assert code correctness.
    pub candidate: &'a VerificationCandidate,
    /// The path to the frozen baseline directory (read-only reference point).
    pub frozen_root: &'a Path,
    /// The orchestrating LLM toolchain.
    pub compiler: &'a IntentCompiler,
    /// Contextual state for MCP plugins and tools.
    pub mcp_manager: Option<&'a crate::mcp::McpManager>,
}

enum EpochOutcome {
    /// Sandbox successfully applied and tested, or conversional short-circuit.
    Success(SnapshotId),
    /// Plan hydration or preflight failed. Does NOT consume a verification run.
    RetryCompile,
    /// Preflight passed, but verifier tests failed. DOES consume a verification run.
    RetryVerification,
}

impl SerialCrucible<'_> {
    pub async fn execute(
        &self,
        messages: &mut ConversationManager,
        snapshot_id: &SnapshotId,
        ledger: &mut EventLedger,
    ) -> Result<SnapshotId> {
        let mut total_attempts = 0;
        let mut verification_runs = 0;
        let max_total_attempts = 10;

        while verification_runs < 3 && total_attempts < max_total_attempts {
            total_attempts += 1;
            println!(
                "\n  ▶️  Crucible Epoch {} (Attempt {}/3)",
                total_attempts,
                verification_runs + 1
            );

            match self
                .run_epoch(
                    messages,
                    snapshot_id,
                    ledger,
                    verification_runs,
                    total_attempts,
                    None,
                )
                .await?
            {
                EpochOutcome::Success(new_snap) => return Ok(new_snap),
                EpochOutcome::RetryCompile => {
                    // LLM hallucinated files or compile errors. Don't consume verifier budget!
                    continue;
                }
                EpochOutcome::RetryVerification => {
                    // Logic issue: verifier executed but failed.
                    verification_runs += 1;
                    continue;
                }
            }
        }

        anyhow::bail!("All crucible attempts failed to pass verification.");
    }

    pub async fn execute_with_precompiled(
        &self,
        messages: &mut ConversationManager,
        snapshot_id: &SnapshotId,
        ledger: &mut EventLedger,
        plan: crow_patch::IntentPlan,
    ) -> Result<SnapshotId> {
        let mut total_attempts = 1; // Since we already did 1 epistemic loop for the precompiled plan
        let mut verification_runs = 0;
        let max_total_attempts = 10;
        
        println!("  ▶️  Crucible Epoch 1 (Fast-Path with precompiled plan)");
        match self.run_epoch(messages, snapshot_id, ledger, verification_runs, total_attempts, Some(plan)).await? {
            EpochOutcome::Success(new_snap) => return Ok(new_snap),
            EpochOutcome::RetryCompile => {},
            EpochOutcome::RetryVerification => { verification_runs += 1; },
        }
        
        // If it failed, fallback to normal execute retry loop
        while verification_runs < 3 && total_attempts < max_total_attempts {
            total_attempts += 1;
            println!(
                "\n  ▶️  Crucible Epoch {} (Attempt {}/3)",
                total_attempts,
                verification_runs + 1
            );

            match self
                .run_epoch(
                    messages,
                    snapshot_id,
                    ledger,
                    verification_runs,
                    total_attempts,
                    None,
                )
                .await?
            {
                EpochOutcome::Success(new_snap) => return Ok(new_snap),
                EpochOutcome::RetryCompile => continue,
                EpochOutcome::RetryVerification => {
                    verification_runs += 1;
                    continue;
                }
            }
        }

        anyhow::bail!("All crucible precompiled attempts failed to pass verification.");
    }

    async fn run_epoch(
        &self,
        messages: &mut ConversationManager,
        snapshot_id: &SnapshotId,
        ledger: &mut EventLedger,
        verification_runs: u32,
        _total_attempts: u32,
        precompiled_plan: Option<crow_patch::IntentPlan>,
    ) -> Result<EpochOutcome> {
        if messages.needs_compaction() {
            eprintln!("  🗜️  Auto-compacting context history...");
            if let Ok(summary) = self
                .compiler
                .compile_summary_of_history(messages.as_messages().as_slice())
                .await
            {
                messages
                    .compact_into_summary(format!("[SYSTEM AUTO-COMPACTED HISTORY]\n{}", summary));
            }
        }

        let compiled_plan = if let Some(p) = precompiled_plan {
            p
        } else {
            let mut observer = crate::event::CliEventHandler::new();

            epistemic::run_epistemic_loop(
                self.compiler,
                messages,
                self.frozen_root,
                self.mcp_manager,
                &mut observer,
            )
            .await?
        };

        if compiled_plan.operations.is_empty() {
            println!("  💬 Conversational response (no code changes)");
            let renderer = crate::render::TerminalRenderer::new();
            let _ = renderer.render_markdown(&compiled_plan.rationale);
            return Ok(EpochOutcome::Success(snapshot_id.clone()));
        }

        // Re-materialize a fresh sandbox from the frozen baseline
        let attempt_mat_config = MaterializeConfig {
            source: self.frozen_root.to_path_buf(),
            artifact_dirs: self.profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: self.profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let attempt_sandbox =
            tokio::task::spawn_blocking(move || crow_materialize::materialize(&attempt_mat_config))
                .await
                .context("Materialization task panicked")?
                .context("Failed to re-materialize attempt sandbox")?;
        // Fresh sandbox ready

        let attempt_sandbox_path = attempt_sandbox.path().to_path_buf();
        let plan_clone = compiled_plan.clone();
        let snap_for_hydrate = snapshot_id.clone();
        let plan_id = format!("plan-{}", chrono::Utc::now().timestamp_millis());
        let hydrated_plan = match tokio::task::spawn_blocking(move || {
            crow_workspace::PlanHydrator::hydrate(
                &plan_clone,
                &snap_for_hydrate,
                &attempt_sandbox_path,
            )
        })
        .await
        {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                eprintln!("  ❌ Hydration failed: {:?}", e);
                messages.push_user(format!(
                    "[HYDRATION FAILED]\nYour plan failed physical hydration: {:?}\n\nPlease reflect and output a new AgentAction to fix the issue.",
                    e
                ));
                return Ok(EpochOutcome::RetryCompile);
            }
            Err(e) => {
                anyhow::bail!("Hydration task panicked: {:?}", e);
            }
        };

        // Plan hydrated successfully

        {
            let plan_for_apply = hydrated_plan.clone();
            let sandbox_view = attempt_sandbox.non_owning_view();
            tokio::task::spawn_blocking(move || {
                crow_workspace::applier::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
            })
            .await
            .context("Apply task panicked")?
            .context("Failed to apply plan to sandbox")?;
        }

        let _ = ledger.append(crow_workspace::ledger::LedgerEvent::PlanHydrated {
            plan_id: plan_id.clone(),
            snapshot_id: snapshot_id.clone(),
            timestamp: chrono::Utc::now(),
        });

        println!("  💉 Plan applied to sandbox");
        crate::diff::render_plan_diff(self.frozen_root, attempt_sandbox.path(), &hydrated_plan);

        // Preflight
        {
            use crow_verifier::preflight::{self, PreflightResult};
            // Preflight compile check
            let start_preflight = std::time::Instant::now();
            let _ = ledger.append(crow_workspace::ledger::LedgerEvent::PreflightStarted {
                plan_id: plan_id.clone(),
                sandbox_path: attempt_sandbox.path().to_string_lossy().into_owned(),
                timestamp: chrono::Utc::now(),
            });

            let preflight_result = crow_verifier::preflight::run_preflight(
                attempt_sandbox.path(),
                Some(self.frozen_root),
                std::time::Duration::from_secs(60),
                &self.profile.primary_lang,
            )
            .await;

            let passed_preflight = matches!(
                preflight_result,
                PreflightResult::Clean | PreflightResult::Skipped(_)
            );
            let _ = ledger.append(crow_workspace::ledger::LedgerEvent::PreflightTested {
                plan_id: plan_id.clone(),
                passed: passed_preflight,
                duration_ms: start_preflight.elapsed().as_millis() as u64,
                timestamp: chrono::Utc::now(),
            });

            match preflight_result {
                PreflightResult::Clean => {
                    println!("  ✅ Preflight: compiles cleanly");
                }
                PreflightResult::Errors(diags) => {
                    let summary = preflight::format_diagnostics(&diags);
                    println!("  ❌ Preflight: {} compile error(s)", diags.len());
                    messages.push_user(format!(
                        "[PREFLIGHT COMPILE CHECK FAILED]\n{}\n\nPlease fix these compile errors and resubmit your plan.",
                        summary
                    ));
                    return Ok(EpochOutcome::RetryCompile);
                }
                PreflightResult::Skipped(reason) => {
                    eprintln!("  ⚠️  Preflight skipped: {}", reason);
                }
            }
        }

        println!("  🧪 Verifying: {}", self.candidate.command.display());
        let exec_config = crow_verifier::types::ExecutionConfig {
            timeout: std::time::Duration::from_secs(60),
            max_output_bytes: 5 * 1024 * 1024,
        };

        let result = crow_verifier::executor::execute(
            attempt_sandbox.path(),
            &self.candidate.command,
            &exec_config,
            &crow_verifier::types::AciConfig::compact(),
            Some(self.frozen_root),
        )
        .await
        .context("Verification execution failed")?;

        let outcome = &result.test_run.outcome;
        if outcome == &crow_evidence::TestOutcome::Passed {
            println!("  ✅ Verdict: PASSED (verification run {})", verification_runs + 1);
            crate::apply_winning_plan(
                self.cfg,
                attempt_sandbox.path(),
                &hydrated_plan,
                &plan_id,
                snapshot_id,
                ledger,
            )
            .await?;
            let new_snapshot_id = crate::snapshot::resolve_snapshot_id(&self.cfg.workspace);
            return Ok(EpochOutcome::Success(new_snapshot_id));
        } else {
            println!("  ❌ Verdict: {:?} — retrying...", result.test_run.outcome);
            messages.push_verifier_result(
                &format!("{:?}", result.test_run.outcome),
                &result.test_run.truncated_log,
            );
            let _ = ledger.append(crow_workspace::ledger::LedgerEvent::PlanRolledBack {
                plan_id,
                reason: format!("Verification failed: {:?}", result.test_run.outcome),
                timestamp: chrono::Utc::now(),
            });
        }

        Ok(EpochOutcome::RetryVerification)
    }
}
