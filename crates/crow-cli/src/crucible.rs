//! The Serial Crucible execution loop.
//!
//! Orchestrates the end-to-end verification and code application flow for a single turn,
//! handling sandbox materialization, compilation preflights, and test execution.
//!
//! All output goes through the `EventHandler` observer — no direct stdout/stderr.

use crate::config::CrowConfig;
use crate::event::{AgentEvent, EventHandler};
use anyhow::{Context, Result};
use crow_brain::IntentCompiler;
use crow_materialize::MaterializeConfig;
use crow_patch::SnapshotId;
use crow_probe::types::{ProjectProfile, VerificationCandidate};
use crow_runtime::context::ConversationManager;
use crow_runtime::epistemic;
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
    pub mcp_manager: Option<&'a crow_runtime::mcp::McpManager>,
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
        ledger: &std::sync::Mutex<EventLedger>,
        observer: &mut dyn EventHandler,
    ) -> Result<SnapshotId> {
        let mut total_attempts = 0;
        let mut verification_runs = 0;
        let max_total_attempts = 10;

        while verification_runs < 3 && total_attempts < max_total_attempts {
            total_attempts += 1;
            observer.handle_event(AgentEvent::ActionStart(format!(
                "Crucible Epoch {} (Attempt {}/3)",
                total_attempts,
                verification_runs + 1
            )));

            // Check cancellation before each epoch
            if observer.is_cancelled() {
                observer.handle_event(AgentEvent::Log("Crucible cancelled by user.".into()));
                return Ok(snapshot_id.clone());
            }

            match self
                .run_epoch(
                    messages,
                    snapshot_id,
                    ledger,
                    verification_runs,
                    total_attempts,
                    None,
                    observer,
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
        ledger: &std::sync::Mutex<EventLedger>,
        plan: crow_patch::IntentPlan,
        observer: &mut dyn EventHandler,
    ) -> Result<SnapshotId> {
        let mut total_attempts = 1;
        let mut verification_runs = 0;
        let max_total_attempts = 10;

        observer.handle_event(AgentEvent::ActionStart(
            "Crucible Epoch 1 (Fast-Path with precompiled plan)".into(),
        ));
        match self
            .run_epoch(
                messages,
                snapshot_id,
                ledger,
                verification_runs,
                total_attempts,
                Some(plan),
                observer,
            )
            .await?
        {
            EpochOutcome::Success(new_snap) => return Ok(new_snap),
            EpochOutcome::RetryCompile => {}
            EpochOutcome::RetryVerification => {
                verification_runs += 1;
            }
        }

        // If it failed, fallback to normal execute retry loop
        while verification_runs < 3 && total_attempts < max_total_attempts {
            total_attempts += 1;
            observer.handle_event(AgentEvent::ActionStart(format!(
                "Crucible Epoch {} (Attempt {}/3)",
                total_attempts,
                verification_runs + 1
            )));

            if observer.is_cancelled() {
                observer.handle_event(AgentEvent::Log("Crucible cancelled by user.".into()));
                return Ok(snapshot_id.clone());
            }

            match self
                .run_epoch(
                    messages,
                    snapshot_id,
                    ledger,
                    verification_runs,
                    total_attempts,
                    None,
                    observer,
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

    #[allow(clippy::too_many_arguments)]
    async fn run_epoch(
        &self,
        messages: &mut ConversationManager,
        snapshot_id: &SnapshotId,
        ledger: &std::sync::Mutex<EventLedger>,
        verification_runs: u32,
        _total_attempts: u32,
        precompiled_plan: Option<crow_patch::IntentPlan>,
        observer: &mut dyn EventHandler,
    ) -> Result<EpochOutcome> {
        if messages.needs_compaction() {
            observer.handle_event(AgentEvent::Log("Auto-compacting context history...".into()));
            if let Ok(summary) = self
                .compiler
                .compile_summary_of_history(messages.as_messages().as_slice())
                .await
            {
                messages
                    .compact_into_summary(format!("[SYSTEM AUTO-COMPACTED HISTORY]\n{summary}"));
            }
        }

        let compiled_plan = if let Some(p) = precompiled_plan {
            p
        } else {
            let file_state_store =
                std::sync::Arc::new(crow_runtime::file_state::FileStateStore::new());
            epistemic::run_epistemic_loop(
                self.compiler,
                messages,
                self.frozen_root,
                self.mcp_manager,
                observer,
                file_state_store,
                std::sync::Arc::new(crow_tools::ToolRegistry::new()),
                std::sync::Arc::new(crow_tools::PermissionEnforcer::new(
                    crow_tools::WriteMode::Sandbox,
                )),
            )
            .await?
        };

        if compiled_plan.operations.is_empty() {
            observer.handle_event(AgentEvent::Log(
                "Conversational response (no code changes)".into(),
            ));
            if !compiled_plan.rationale.trim().is_empty() {
                observer.handle_event(AgentEvent::Markdown(compiled_plan.rationale));
            }
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
                observer.handle_event(AgentEvent::Error(format!("Hydration failed: {e:?}")));
                messages.push_user(format!(
                    "[HYDRATION FAILED]\nYour plan failed physical hydration: {e:?}\n\nPlease reflect and output a new AgentAction to fix the issue."
                ));
                return Ok(EpochOutcome::RetryCompile);
            }
            Err(e) => {
                anyhow::bail!("Hydration task panicked: {e:?}");
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

        if let Ok(mut l) = ledger.lock() {
            let _ = l.append(crow_workspace::ledger::LedgerEvent::PlanHydrated {
                plan_id: plan_id.clone(),
                snapshot_id: snapshot_id.clone(),
                timestamp: chrono::Utc::now(),
            });
        }

        observer.handle_event(AgentEvent::ActionComplete("Plan applied to sandbox".into()));

        // Write the patch file to .crow/logs/latest.patch (always useful),
        // but skip the stdout-based diff render — the TUI gets structured events.
        // CLI callers that want the colored diff can call render_plan_diff separately.
        {
            let crow_dir = self.frozen_root.join(".crow").join("logs");
            let _ = std::fs::create_dir_all(&crow_dir);
            let patch_path = crow_dir.join("latest.patch");
            if let Ok(mut f) = std::fs::File::create(&patch_path) {
                for op in &hydrated_plan.operations {
                    let patch_text = crate::diff::generate_patch_text(
                        self.frozen_root,
                        attempt_sandbox.path(),
                        op,
                    );
                    use std::io::Write;
                    let _ = write!(f, "{patch_text}");
                }
            }
        }

        // Preflight
        {
            use crow_verifier::preflight::{self, PreflightResult};
            // Preflight compile check
            let start_preflight = std::time::Instant::now();
            if let Ok(mut guard) = ledger.lock() {
                let _ = guard.append(crow_workspace::ledger::LedgerEvent::PreflightStarted {
                    plan_id: plan_id.clone(),
                    sandbox_path: attempt_sandbox.path().to_string_lossy().into_owned(),
                    timestamp: chrono::Utc::now(),
                });
            }

            observer.handle_event(AgentEvent::CruciblePreflight("Compile check".into()));

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
            if let Ok(mut guard) = ledger.lock() {
                let _ = guard.append(crow_workspace::ledger::LedgerEvent::PreflightTested {
                    plan_id: plan_id.clone(),
                    passed: passed_preflight,
                    duration_ms: start_preflight.elapsed().as_millis() as u64,
                    timestamp: chrono::Utc::now(),
                });
            }

            match preflight_result {
                PreflightResult::Clean => {
                    observer.handle_event(AgentEvent::ActionComplete(
                        "Preflight: compiles cleanly".into(),
                    ));
                }
                PreflightResult::Errors(diags) => {
                    let summary = preflight::format_diagnostics(&diags);
                    observer.handle_event(AgentEvent::Error(format!(
                        "Preflight: {} compile error(s)",
                        diags.len()
                    )));
                    messages.push_user(format!(
                        "[PREFLIGHT COMPILE CHECK FAILED]\n{summary}\n\nPlease fix these compile errors and resubmit your plan."
                    ));
                    return Ok(EpochOutcome::RetryCompile);
                }
                PreflightResult::Skipped(reason) => {
                    observer.handle_event(AgentEvent::Log(format!("Preflight skipped: {reason}")));
                }
            }
        }

        observer.handle_event(AgentEvent::CruciblePreflight(format!(
            "Verifying: {}",
            self.candidate.command.display()
        )));
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
            observer.handle_event(AgentEvent::ActionComplete(format!(
                "Verdict: PASSED (verification run {})",
                verification_runs + 1
            )));
            crate::crucible_runner::apply_winning_plan(
                self.cfg,
                attempt_sandbox.path(),
                &hydrated_plan,
                &plan_id,
                snapshot_id,
                ledger,
                observer,
            )
            .await?;
            let new_snapshot_id = crate::snapshot::resolve_snapshot_id(&self.cfg.workspace);
            return Ok(EpochOutcome::Success(new_snapshot_id));
        } else {
            observer.handle_event(AgentEvent::Error(format!(
                "Verdict: {:?} — retrying...",
                result.test_run.outcome
            )));
            messages.push_verifier_result(
                &format!("{:?}", result.test_run.outcome),
                &result.test_run.truncated_log,
            );
            if let Ok(mut guard) = ledger.lock() {
                let _ = guard.append(crow_workspace::ledger::LedgerEvent::PlanRolledBack {
                    plan_id,
                    reason: format!("Verification failed: {:?}", result.test_run.outcome),
                    timestamp: chrono::Utc::now(),
                });
            }
        }

        Ok(EpochOutcome::RetryVerification)
    }
}
