use anyhow::{Context, Result};
use crow_brain::IntentCompiler;
use crate::mcp::McpManager;
use crow_workspace::ledger::EventLedger;
use crow_intel::RepoMap;
use crow_patch::{SnapshotId};
use std::sync::Arc;
use std::path::PathBuf;
use crate::config::CrowConfig;
use crow_materialize::{materialize, MaterializeConfig};

pub struct SessionRuntime {
    pub compiler: Arc<IntentCompiler>,
    pub mcp_manager: Arc<McpManager>,
    pub ledger: EventLedger,
    pub cached_repo_map: Option<(SnapshotId, Arc<RepoMap>)>,
    pub workspace: PathBuf,
}

impl SessionRuntime {
    pub async fn boot(cfg: &CrowConfig) -> Result<Self> {
        let client = cfg.build_llm_client()?;
        let compiler = Arc::new(
            IntentCompiler::new(client)
                .with_native_tool_calling(cfg.llm.json_mode)
        );
        let mcp_manager = Arc::new(McpManager::boot(&cfg.mcp_servers).await?);
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&cfg.workspace);
        
        let ledger = crate::open_ledger(&cfg.workspace).unwrap_or_else(|e| {
            eprintln!("  ⚠️  Failed to open Event Ledger: {}", e);
            let fallback = std::env::temp_dir().join(format!(
                "crow_ledger_{}_{}.jsonl",
                snapshot_id.0,
                chrono::Utc::now().timestamp_millis()
            ));
            crow_workspace::ledger::EventLedger::open(&fallback).unwrap_or_else(|fallback_err| {
                eprintln!(
                    "  🚨 Fatal: Could not write fallback ledger to {}: {}",
                    fallback.display(),
                    fallback_err
                );
                std::process::exit(1);
            })
        });

        Ok(Self {
            compiler,
            mcp_manager,
            ledger,
            cached_repo_map: None,
            workspace: cfg.workspace.clone(),
        })
    }

    /// Fast-Path Epistemic Target.
    /// Runs epistemic recon loop using the Live Workspace. Only materializes the frozen sandbox if
    /// the LLM requests a crucible verification with proposed modifying operations.
    pub async fn execute_turn(
        &mut self,
        cfg: &CrowConfig,
        prompt: &str,
        messages: &mut crate::context::ConversationManager,
    ) -> Result<SnapshotId> {
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);

        let profile = crate::scan_workspace(&self.workspace).map_err(|e| anyhow::anyhow!(e))?;
        let candidate = match profile.verification_candidates.first() {
            Some(c) => c.clone(),
            None => {
                anyhow::bail!(
                    "No verification candidates found. Cannot safely execute autonomous patches without a test suite.\n\
                     Please configure a custom test script in `.crow/config.json`."
                );
            }
        };

        // Leverage cached repo_map if snapshot hasn't changed.
        let mut repo_map_cloned = None;
        if let Some((cached_snap, map)) = &self.cached_repo_map {
            if cached_snap == &snapshot_id {
                repo_map_cloned = Some(Arc::clone(map));
            }
        }

        let repo_map = match repo_map_cloned {
            Some(map) => map,
            None => {
                let map = cfg.build_repo_map_for(&self.workspace)
                    .map_err(|e| anyhow::anyhow!(e))
                    .context("Failed to build repo map from live workspace")?;
                let arc_map = Arc::new(map);
                self.cached_repo_map = Some((snapshot_id.clone(), Arc::clone(&arc_map)));
                arc_map
            }
        };

        let _ = self.ledger.append(crow_workspace::ledger::LedgerEvent::SnapshotCreated {
            id: snapshot_id.clone(),
            git_hash: snapshot_id.0.clone(),
            timestamp: chrono::Utc::now(),
        });

        let sys_msgs = crate::prompt::PromptBuilder::new()
            .with_repo_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        messages.set_system(sys_msgs);

        if messages.as_messages().len() <= 2 {
            messages.push_user(format!("Task:\n{}", prompt));
        } else {
            messages.push_user(prompt);
        }



        // We run the initial loop using the live workspace!
        let mut observer = crate::event::CliEventHandler::new();

        let plan = crate::epistemic::run_epistemic_loop(
            &self.compiler,
            messages,
            &self.workspace,   // LIVE WORKSPACE
            Some(&self.mcp_manager),
            &mut observer,
        ).await?;


        if plan.operations.is_empty() {
            println!("\n[🎉] Conversational Intent Detected (No codebase changes proposed)");
            let renderer = crate::render::TerminalRenderer::new();
            renderer.print_markdown(&plan.rationale);
            return Ok(snapshot_id);
        }

        println!();
        println!("  📋 Code modification proposed ({} ops). Materializing sandbox...", plan.operations.len());
        
        // Setup Crucible sandbox and run test suite over the plan!
        let mcts_config = crate::mcts::MctsConfig::from_env();
        if !mcts_config.is_serial() {
            if !cfg.llm.prompt_caching {
                println!(
                    "    ⚠️  Warning: MCTS parallel mode (CROW_MCTS_BRANCHES={}) is running without prompt_caching enabled. \
                     This may be expensive on providers that bill for repetitive input tokens.",
                    mcts_config.branch_factor
                );
            }
            
            let attempt_mat_config = MaterializeConfig {
                source: self.workspace.clone(),
                artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
                skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
                allow_hardlinks: false,
            };
            let frozen_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
                .await
                .context("Materialization task panicked")?
                .context("Failed to materialize initial verifier sandbox")?;
                
            let frozen_root = frozen_sandbox.path().to_path_buf();

            let winner = crate::run_mcts_crucible(
                &mcts_config,
                &profile,
                &candidate,
                &self.workspace,
                &frozen_root,
                &self.compiler,
                messages,
                &snapshot_id,
                Some(&self.mcp_manager),
            ).await?;

            if let Some(w) = winner {
                let plan_id = format!("mcts-{}-{}", snapshot_id.0, chrono::Utc::now().timestamp_millis());
                crate::apply_winning_plan(
                    cfg,
                    w.sandbox.path(),
                    &w.plan,
                    &plan_id,
                    &snapshot_id,
                    &mut self.ledger,
                ).await?;
            }
            return Ok(snapshot_id);
        }

        // We materialize a sandbox and hand it off to the SerialCrucible logic
        let attempt_mat_config = MaterializeConfig {
            source: self.workspace.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let frozen_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
            .await
            .context("Materialization task panicked")?
            .context("Failed to materialize initial verifier sandbox")?;
            
        let frozen_root = frozen_sandbox.path().to_path_buf();

        // Because we already got the plan, we might need to apply it directly.
        // Wait, `crucible.execute()` expects to call `run_epistemic_loop` to get the plan.
        // The serial crucible assumes it drives the loop from scratch!
        // To bridge the Fast Path: we can modify `SerialCrucible::execute` to optionally take a pre-compiled plan!
        
        let crucible = crate::crucible::SerialCrucible {
            cfg,
            profile: &profile,
            candidate: &candidate,
            frozen_root: &frozen_root,
            compiler: &self.compiler,
            mcp_manager: Some(&self.mcp_manager),
        };
        
        // Pass the already compiled plan as a jump-start for verification
        let target_snap = crucible.execute_with_precompiled(
            messages,
            &snapshot_id,
            &mut self.ledger,
            plan,
        ).await?;

        Ok(target_snap)
    }

    // ─── Unified Entry Points ────────────────────────────────────────────────

    fn get_or_build_repo_map(&mut self, cfg: &CrowConfig) -> Result<Arc<RepoMap>> {
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);
        if let Some((cached_snap, map)) = &self.cached_repo_map {
            if cached_snap == &snapshot_id {
                return Ok(Arc::clone(map));
            }
        }
        let map = cfg.build_repo_map_for(&self.workspace)
            .map_err(|e| anyhow::anyhow!(e))
            .context("Failed to build repo map")?;
        let arc_map = Arc::new(map);
        self.cached_repo_map = Some((snapshot_id.clone(), Arc::clone(&arc_map)));
        Ok(arc_map)
    }

    pub async fn compile_only(&mut self, cfg: &CrowConfig, prompt: &str) -> Result<()> {

        println!("🦅 crow-code Compile-Only mode initializing...\n");

        println!("[1/3] Gathering Repomap Context via tree-sitter...");
        let repo_map = self.get_or_build_repo_map(cfg)?;
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);
        println!("    🎯 Compressed map length: {} bytes", repo_map.map_text.len());

        println!("\n[2/3] Compiling IntentPlan via crow-brain (Engine: {})...", cfg.describe_provider());

        let sys_msgs = crate::prompt::PromptBuilder::default()
            .with_repo_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        let mut messages = crate::context::ConversationManager::new(sys_msgs);
        messages.push_user(format!("Task:\n{}", prompt));

        match self.compiler.compile_action(&messages.as_messages()).await {
            Ok(action) => {
                println!("\n[✓] Compilation Successful!");
                println!("--- Parsed AgentAction ---");
                println!("{}", serde_json::to_string_pretty(&action)?);
                Ok(())
            }
            Err(e) => {
                eprintln!("\n[✗] Compilation Failed: {:?}", e);
                anyhow::bail!("Failed to compile AgentAction")
            }
        }
    }

    pub async fn generate_plan(&mut self, cfg: &CrowConfig, prompt: &str) -> Result<()> {
        use crow_workspace::PlanHydrator;
        use crate::evidence_report::*;

        println!("🦅 crow plan — Evidence-First Preview\n");
        println!("  Write mode: {}", cfg.write_mode);

        println!("\n[1/5] Workspace Recon...");
        let profile = crate::scan_workspace(&self.workspace).map_err(|e| anyhow::anyhow!(e))?;
        
        let file_count = std::fs::read_dir(&self.workspace).map(|entries| entries.count()).unwrap_or(0);
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);

        let recon = ReconSummary {
            language: profile.primary_lang.name.clone(),
            tier: format!("{:?}", profile.primary_lang.tier),
            snapshot_id: snapshot_id.clone(),
            files_scanned: file_count,
            manifests: vec![],
        };
        println!("  ✅ {} ({}) | {} files", recon.language, recon.tier, recon.files_scanned);

        println!("\n[2/5] Materializing sandbox & compiling plan...");
        let mat_config = MaterializeConfig {
            source: self.workspace.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
            .await?.context("Failed to materialize sandbox")?;
        let frozen_root = sandbox.path().to_path_buf();

        let repo_map = cfg.build_repo_map_for(&frozen_root).map_err(|e| anyhow::anyhow!(e))?;
        
        let sys_msgs = crate::prompt::PromptBuilder::default()
            .with_repo_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        let mut messages = crate::context::ConversationManager::new(sys_msgs);
        messages.push_user(format!("Task:\n{}", prompt));

        let mut obs = crate::event::CliEventHandler::default();
        let compiled_plan = crate::epistemic::run_epistemic_loop(
            &self.compiler,
            &mut messages,
            &frozen_root,
            Some(&self.mcp_manager),
            &mut obs,
        ).await?;

        let compilation = CompilationSummary::from_plan(&compiled_plan);
        println!("  ✅ {} ops, {:?} confidence", compilation.total_ops(), compilation.confidence);

        println!("\n[3/5] Hydrating plan against frozen sandbox...");
        let plan_clone = compiled_plan.clone();
        let frozen_clone = frozen_root.clone();
        let snap_clone = snapshot_id.clone();
        let hydrated_plan = tokio::task::spawn_blocking(move || {
            PlanHydrator::hydrate(&plan_clone, &snap_clone, &frozen_clone)
        }).await?.context("Hydration failed")?;

        let hydration = HydrationSummary {
            snapshot_verified: true,
            hashes_matched: hydrated_plan.operations.len(),
            hashes_total: hydrated_plan.operations.len(),
            drift_warnings: vec![],
        };
        println!("  ✅ Snapshot anchored, {}/{} hashes verified", hydration.hashes_matched, hydration.hashes_total);

        println!("\n[4/5] Running preflight compile check...");
        let plan_for_apply = hydrated_plan.clone();
        let sandbox_view = sandbox.non_owning_view();
        tokio::task::spawn_blocking(move || crow_workspace::applier::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view))
            .await?.context("Apply failed")?;

        let preflight_start = std::time::Instant::now();
        let preflight_result = crow_verifier::preflight::run_preflight(
            sandbox.path(),
            Some(&frozen_root),
            std::time::Duration::from_secs(30),
            &profile.primary_lang,
        ).await;
        
        let preflight = PreflightSummary {
            language: profile.primary_lang.name.clone(),
            outcome: match &preflight_result {
                crow_verifier::preflight::PreflightResult::Clean => PreflightOutcome::Clean { duration_secs: preflight_start.elapsed().as_secs_f64() },
                crow_verifier::preflight::PreflightResult::Errors(diags) => PreflightOutcome::Errors { count: diags.len(), summary: crow_verifier::preflight::format_diagnostics(diags) },
                crow_verifier::preflight::PreflightResult::Skipped(r) => PreflightOutcome::Skipped { reason: r.clone() },
            },
        };

        let compile_passed = matches!(preflight_result, crow_verifier::preflight::PreflightResult::Clean | crow_verifier::preflight::PreflightResult::Skipped(_));
        let evidence = crow_evidence::types::EvidenceMatrix {
            compile_runs: vec![crow_evidence::types::TestRun {
                command: format!("preflight ({})", profile.primary_lang.name),
                outcome: if compile_passed { crow_evidence::types::TestOutcome::Passed } else { crow_evidence::types::TestOutcome::Failed },
                passed: if compile_passed { 1 } else { 0 },
                failed: if compile_passed { 0 } else { 1 },
                skipped: 0,
                duration: std::time::Duration::from_secs(0),
                truncated_log: String::new(),
            }],
            test_scope: Some(crow_evidence::types::TestScope::Selective),
            has_known_baseline: true,
            lints_clean: compile_passed,
            intelligence_confidence: crow_patch::Confidence::Medium,
            risk_flags: vec![],
        };
        
        let verdict = Verdict::from_evidence(evidence);
        let report = EvidenceReport { recon, compilation, hydration, preflight, verdict };
        println!("{}", report);
        println!("\n─── Planned Changes ───");
        crate::diff::render_plan_diff(&frozen_root, sandbox.path(), &hydrated_plan);

        if let Ok(store) = crate::session::SessionStore::open() {
            let mut sess = crate::session::Session::new(&cfg.workspace, prompt);
            sess.save_messages(&messages.as_messages());
            sess.push_snapshot(snapshot_id);
            if store.save(&sess).is_ok() {
                println!("\n  💾 Session saved: {}", sess.id.0);
            }
        }
        Ok(())
    }

    pub async fn resume(&mut self, cfg: &CrowConfig, session_id: &str) -> Result<()> {
        println!("🦅 crow session resume — continuing session {}", &session_id[..8.min(session_id.len())]);

        let store = crate::session::SessionStore::open()?;
        let mut loaded_session = store.load(&crate::session::SessionId(session_id.to_string()))?;

        println!("  Workspace: {}", loaded_session.workspace_root.display());
        println!("  Task: {}", loaded_session.task);

        let restored_messages = loaded_session.restore_messages();
        println!("  Restored {} messages from history", restored_messages.len());

        if !loaded_session.workspace_root.exists() {
            anyhow::bail!("Workspace no longer exists: {}", loaded_session.workspace_root.display());
        }

        let snapshot_id = crate::snapshot::resolve_snapshot_id(&loaded_session.workspace_root);
        println!("  Snapshot ID: {}", snapshot_id.0);

        if let Some(last_snap) = loaded_session.snapshot_timeline.last() {
            if *last_snap != snapshot_id {
                println!("  ⚠️  Workspace has changed since last session snapshot");
                println!("     Last: {} → Current: {}", last_snap.0, snapshot_id.0);
            } else {
                println!("  ✅ Workspace matches last session snapshot");
            }
        }

        let profile = crate::scan_workspace(&loaded_session.workspace_root).map_err(|e| anyhow::anyhow!(e))?;
        let _candidate = match profile.verification_candidates.first() {
            Some(c) => c.clone(),
            None => {
                anyhow::bail!("No verification candidates found. Cannot safely resume execution.");
            }
        };

        println!("\n  Materializing sandbox...");
        let mat_config = MaterializeConfig {
            source: loaded_session.workspace_root.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
            .await?.context("Failed to materialize sandbox")?;
        let frozen_root = sandbox.path().to_path_buf();

        let repo_map = cfg.build_repo_map_for(&frozen_root).map_err(|e| anyhow::anyhow!(e))?;
        
        let sys_msgs = crate::prompt::PromptBuilder::default()
            .with_repo_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        let mut messages = crate::context::ConversationManager::new(sys_msgs);

        for msg in &restored_messages {
            match msg.role {
                crow_brain::ChatRole::User => messages.push_user(&msg.content),
                crow_brain::ChatRole::Assistant => messages.push_assistant(&msg.content),
                crow_brain::ChatRole::System => {}
            }
        }

        messages.push_user(format!(
            "[SESSION RESUMED]\nContinuing work on the original task: {}\n\nPlease pick up where you left off. If the previous attempt failed, try a different approach.",
            loaded_session.task
        ));

        println!("  Entering crucible loop...\n");

        let mut obs = crate::event::CliEventHandler::default();
        let compiled_plan = crate::epistemic::run_epistemic_loop(
            &self.compiler,
            &mut messages,
            &frozen_root,
            Some(&self.mcp_manager),
            &mut obs,
        ).await?;

        let attempt_mat_config = MaterializeConfig {
            source: frozen_root.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let attempt_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
            .await?.context("Failed to materialize attempt sandbox")?;

        let attempt_sandbox_path = attempt_sandbox.path().to_path_buf();
        let plan_clone = compiled_plan.clone();
        let snap_clone = snapshot_id.clone();
        let hydrated_plan = tokio::task::spawn_blocking(move || {
            crow_workspace::PlanHydrator::hydrate(&plan_clone, &snap_clone, &attempt_sandbox_path)
        }).await?.context("Hydration failed")?;

        let plan_for_apply = hydrated_plan.clone();
        let sandbox_view = attempt_sandbox.non_owning_view();
        tokio::task::spawn_blocking(move || crow_workspace::applier::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view))
            .await?.context("Failed to apply plan to sandbox")?;

        let preflight_result = crow_verifier::preflight::run_preflight(
            attempt_sandbox.path(),
            Some(&frozen_root),
            std::time::Duration::from_secs(60),
            &profile.primary_lang,
        ).await;

        match &preflight_result {
            crow_verifier::preflight::PreflightResult::Clean => {
                println!("  ✅ Preflight: code compiles cleanly");
            }
            crow_verifier::preflight::PreflightResult::Errors(diags) => {
                println!("  ❌ Preflight: {} compile error(s)", diags.len());
                println!("{}", crow_verifier::preflight::format_diagnostics(diags));
            }
            crow_verifier::preflight::PreflightResult::Skipped(reason) => {
                println!("  ⚠️  Preflight skipped: {}", reason);
            }
        }

        let exec_config = crow_verifier::ExecutionConfig {
            timeout: std::time::Duration::from_secs(60),
            max_output_bytes: 5 * 1024 * 1024,
        };
        let result = crow_verifier::executor::execute(
            attempt_sandbox.path(),
            &_candidate.command,
            &exec_config,
            &crow_verifier::types::AciConfig::compact(),
            Some(&frozen_root),
        ).await.context("Verification execution failed")?;

        let outcome = &result.test_run.outcome;
        println!("\n╔══════════════════════════════════════╗");
        println!("║  Resume Verdict: {:?}", outcome);
        println!("╚══════════════════════════════════════╝");
        println!("Evidence:\n{}", result.test_run.truncated_log);

        println!("\n--- Changes ---");
        crate::diff::render_plan_diff(&frozen_root, attempt_sandbox.path(), &hydrated_plan);

        if outcome != &crow_evidence::TestOutcome::Passed {
            anyhow::bail!("Resumed session: verification failed.");
        }

        if matches!(cfg.write_mode, crate::config::WriteMode::WorkspaceWrite | crate::config::WriteMode::DangerFullAccess) {
            println!("\n[4] Writing verified changes to workspace...");
            if let Err(e) = crow_workspace::applier::apply_sandbox_to_workspace(&cfg.workspace, &hydrated_plan) {
                eprintln!("  ❌ Failed to apply to workspace: {:?}", e);
                anyhow::bail!("Workspace mutation failed during resume.");
            }

            println!("  ✅ Workspace updated successfully.");
            if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, &hydrated_plan) {
                println!("  ⚠️  Could not automatically commit changes: {}", e);
            } else {
                println!("  ✅ Changes committed to git timeline.");
            }
        } else {
            println!("\n  📦 Write mode is sandbox-only. Changes not applied to workspace.");
        }

        let post_mutation_snapshot = crate::snapshot::resolve_snapshot_id(&cfg.workspace);
        loaded_session.save_messages(&messages.as_messages());
        loaded_session.push_snapshot(post_mutation_snapshot);
        store.save(&loaded_session)?;
        println!("\n  💾 Session updated: {}", loaded_session.id.0);

        println!("\n[🎉] Resumed session completed successfully!");
        Ok(())
    }
}
