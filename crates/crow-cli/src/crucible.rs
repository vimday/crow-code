use crate::config::CrowConfig;
use crate::context::ConversationManager;
use crate::epistemic;
use crate::epistemic_ui::SpinnerObserver;
use anyhow::{Context, Result};
use crow_brain::IntentCompiler;
use crow_materialize::MaterializeConfig;
use crow_patch::SnapshotId;
use crow_probe::types::{ProjectProfile, VerificationCandidate};
use crow_workspace::ledger::EventLedger;
use std::path::Path;

pub struct SerialCrucible<'a> {
    pub cfg: &'a CrowConfig,
    pub profile: &'a ProjectProfile,
    pub candidate: &'a VerificationCandidate,
    pub frozen_root: &'a Path,
    pub compiler: &'a IntentCompiler,
    pub mcp_manager: Option<&'a crate::mcp::McpManager>,
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
                "\n▶️ Crucible Epoch {} (Verification Run {}/3)",
                total_attempts,
                verification_runs + 1
            );

            if let Some(new_snap) = self
                .run_epoch(
                    messages,
                    snapshot_id,
                    ledger,
                    verification_runs,
                    total_attempts,
                )
                .await?
            {
                return Ok(new_snap);
            }

            verification_runs += 1;
        }

        anyhow::bail!("All crucible attempts failed to pass verification.");
    }

    async fn run_epoch(
        &self,
        messages: &mut ConversationManager,
        snapshot_id: &SnapshotId,
        ledger: &mut EventLedger,
        verification_runs: u32,
        _total_attempts: u32,
    ) -> Result<Option<SnapshotId>> {
        if messages.needs_compaction() {
            println!("  🗜️  Auto-compacting massive context history (Auto-Compaction)...");
            if let Ok(summary) = self
                .compiler
                .compile_summary_of_history(messages.as_messages().as_slice())
                .await
            {
                messages
                    .compact_into_summary(format!("[SYSTEM AUTO-COMPACTED HISTORY]\n{}", summary));
            }
        }

        let mut observer = SpinnerObserver::new(format!(
            "🧠 Epistemic Step {{step}}/{{max}} (Run {}/3) — Modulating Request...",
            verification_runs + 1
        ));

        let compiled_plan = epistemic::run_epistemic_loop(
            self.compiler,
            messages,
            self.frozen_root,
            self.mcp_manager,
            &mut observer,
        )
        .await?;
        observer.finish();

        if compiled_plan.operations.is_empty() {
            println!("\n[🎉] Conversational Intent Detected (No codebase changes proposed)");
            let renderer = crate::render::TerminalRenderer::new();
            let _ = renderer.render_markdown(&compiled_plan.rationale);
            return Ok(Some(snapshot_id.clone()));
        }

        println!("\n[5/6] Re-materializing fresh sandbox (from frozen baseline)...");
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
        println!(
            "    🛡️  Fresh attempt sandbox at: {}",
            attempt_sandbox.path().display()
        );

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
                println!("    ❌ Hydration failed: {:?}", e);
                messages.push_user(format!(
                    "[HYDRATION FAILED]\nYour plan failed physical hydration: {:?}\n\nPlease reflect and output a new AgentAction to fix the issue.",
                    e
                ));
                return Ok(None);
            }
            Err(e) => {
                anyhow::bail!("Hydration task panicked: {:?}", e);
            }
        };

        println!(
            "    💧 Hydrated Plan:\n{}",
            serde_json::to_string_pretty(&hydrated_plan)?
        );

        {
            let plan_for_apply = hydrated_plan.clone();
            let sandbox_view = attempt_sandbox.non_owning_view();
            tokio::task::spawn_blocking(move || {
                crate::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
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

        println!("    💉 Sandbox injection successful!");
        println!("\n--- Sandbox Diff (frozen baseline → patched) ---");
        crate::diff::render_plan_diff(self.frozen_root, attempt_sandbox.path(), &hydrated_plan);

        // Preflight
        {
            use crow_verifier::preflight::{self, PreflightResult};
            println!("\n    🔍 Running preflight compile check...");
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
                    println!("    ✅ Preflight: code compiles cleanly");
                }
                PreflightResult::Errors(diags) => {
                    let summary = preflight::format_diagnostics(&diags);
                    println!("    ❌ Preflight: {} compile error(s) found", diags.len());
                    println!("{}", summary);
                    messages.push_user(format!(
                        "[PREFLIGHT COMPILE CHECK FAILED]\n{}\n\nPlease fix these compile errors and resubmit your plan.",
                        summary
                    ));
                    return Ok(None);
                }
                PreflightResult::Skipped(reason) => {
                    println!("    ⚠️  Preflight skipped: {}", reason);
                }
            }
        }

        println!(
            "\n[6/6] Verifying Sandbox with '{}'...",
            self.candidate.command.display()
        );
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
        println!("\n╔══════════════════════════════════════╗");
        println!("║  Dry-Run Verdict: {:?}", outcome);
        println!("╚══════════════════════════════════════╝");
        println!("Evidence:\n{}", result.test_run.truncated_log);

        if outcome == &crow_evidence::TestOutcome::Passed {
            println!(
                "\n[🎉] Autonomous execution successful on verification run {}!",
                verification_runs + 1
            );
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
            return Ok(Some(new_snapshot_id));
        } else {
            println!("\n[❗] Verification failed! Re-entering Crucible Loop with ACI log...");
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

        Ok(None)
    }
}
