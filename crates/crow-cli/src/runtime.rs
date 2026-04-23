use crate::config::CrowConfig;
use crow_runtime::mcp::McpManager;
use anyhow::{Context, Result};
use crow_brain::IntentCompiler;
use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::SnapshotId;
use crow_workspace::ledger::EventLedger;
use std::sync::{Arc, Mutex};

pub struct SessionRuntime {
    pub compiler: Arc<IntentCompiler>,
    pub mcp_manager: Arc<McpManager>,
    pub ledger: Mutex<EventLedger>,
    pub cached_repo_map: Mutex<Option<(SnapshotId, std::sync::Arc<crow_intel::ContextMap>)>>,
    pub workspace: std::path::PathBuf,
    pub task_registry: crow_runtime::registry::TaskRegistry,
    pub team_registry: crow_runtime::registry::TeamRegistry,
    pub tool_registry: std::sync::Arc<crow_tools::ToolRegistry>,
    pub permissions: std::sync::Arc<crow_tools::PermissionEnforcer>,
}

impl SessionRuntime {
    pub async fn boot(cfg: &CrowConfig) -> Result<Self> {
        let client = cfg.build_llm_client()?;
        let compiler =
            Arc::new(IntentCompiler::new(client).with_native_tool_calling(cfg.llm.json_mode));
        let converted_mcp_servers: std::collections::HashMap<String, crow_runtime::mcp::ServerConfig> = cfg.mcp_servers.iter().map(|(k, v)| {
            (k.clone(), crow_runtime::mcp::ServerConfig {
                command: v.command.clone(),
                args: v.args.clone(),
            })
        }).collect();
        let mcp_manager = Arc::new(McpManager::boot(&converted_mcp_servers).await?);
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&cfg.workspace);

        let ledger = crate::open_ledger(&cfg.workspace).unwrap_or_else(|e| {
            eprintln!("  ⚠️  Failed to open Event Ledger: {e}");
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

        let mut tool_registry = crow_tools::ToolRegistry::new();
        // Recon tools
        tool_registry.register(Box::new(crow_tools::recon::ListDirTool));
        tool_registry.register(Box::new(crow_tools::recon::SearchTool));
        tool_registry.register(Box::new(crow_tools::recon::FetchUrlTool));
        tool_registry.register(Box::new(crow_tools::recon::FileInfoTool));
        tool_registry.register(Box::new(crow_tools::recon::WordCountTool));
        tool_registry.register(Box::new(crow_tools::recon::DirTreeTool));
        tool_registry.register(Box::new(crow_tools::recon::ReadFilesTool));
        // Action tools
        tool_registry.register(Box::new(crow_tools::bash::BashTool));
        tool_registry.register(Box::new(crow_tools::file_edit::FileEditTool));
        tool_registry.register(Box::new(crow_tools::file_write::FileWriteTool));
        tool_registry.register(Box::new(crow_tools::grep::GrepTool));
        tool_registry.register(Box::new(crow_tools::glob::GlobTool));

        Ok(Self {
            compiler,
            mcp_manager,
            ledger: Mutex::new(ledger),
            cached_repo_map: Mutex::new(None),
            workspace: cfg.workspace.clone(),
            task_registry: crow_runtime::registry::TaskRegistry::new(),
            team_registry: crow_runtime::registry::TeamRegistry::new(),
            tool_registry: std::sync::Arc::new(tool_registry),
            permissions: std::sync::Arc::new(crow_tools::PermissionEnforcer { mode: crow_tools::WriteMode::Sandbox }),
        })
    }

    /// Materializes a frozen copy of the workspace to prevent time-of-check divergence.
    async fn materialize_baseline(
        &self,
        profile: &crow_probe::ProjectProfile,
    ) -> Result<crow_materialize::SandboxHandle> {
        let baseline_mat_config = MaterializeConfig {
            source: self.workspace.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        tokio::task::spawn_blocking(move || materialize(&baseline_mat_config))
            .await
            .context("Materialization task panicked")?
            .context("Failed to materialize baseline snapshot")
    }

    /// Builds a semantic map of the codebase, hitting memory cache if the snapshot hash hasn't mutated.
    fn build_context_map_with_cache(
        &self,
        cfg: &CrowConfig,
        snapshot_id: &SnapshotId,
        frozen_root: &std::path::Path,
    ) -> Result<Arc<crow_intel::ContextMap>> {
        let mut repo_map_cloned = None;
        if let Some((cached_snap, map)) = self
            .cached_repo_map
            .lock()
            .map_err(|_| anyhow::anyhow!("Cache lock poisoned"))?
            .as_ref()
        {
            if cached_snap == snapshot_id {
                repo_map_cloned = Some(Arc::clone(map));
            }
        }

        match repo_map_cloned {
            Some(map) => Ok(map),
            None => {
                let map = cfg
                    .build_context_map_for(frozen_root)
                    .map_err(|e| anyhow::anyhow!(e))
                    .context("Failed to build repo map from frozen baseline")?;
                let arc_map = Arc::new(map);
                *self.cached_repo_map.lock().map_err(|_| anyhow::anyhow!("Cache lock poisoned"))? =
                    Some((snapshot_id.clone(), Arc::clone(&arc_map)));
                Ok(arc_map)
            }
        }
    }

    /// Discovers system and repository skills, validates dependencies, and implicitly matches them to the given prompt.
    fn load_and_resolve_skills(
        &self,
        prompt: &str,
        observer: &mut dyn crate::event::EventHandler,
    ) -> Vec<crow_brain::skill::Skill> {
        let mut skill_dirs = Vec::new();
        if let Some(home) = dirs::home_dir() {
            skill_dirs.push(home.join(".crow").join("skills"));
        }
        skill_dirs.push(self.workspace.join(".crow").join("skills"));

        let skill_loader = crow_brain::skill::SkillLoader::new(skill_dirs);
        let loaded_skills = match skill_loader.load_all() {
            Ok(skills) => skills,
            Err(e) => {
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "    ⚠️ Skill loading failed: {e}"
                )));
                Vec::new()
            }
        };

        crow_brain::skill::resolve_skills_for_context(&loaded_skills, prompt)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Snapshot-safe execution turn.
    /// Freezes the workspace baseline first, then runs the epistemic loop against
    /// the frozen snapshot. This ensures planning context and verification baseline
    /// are always the same snapshot — no time-of-check/time-of-use divergence.
    pub async fn execute_turn(
        &self,
        cfg: &CrowConfig,
        prompt: &str,
        messages: &mut crow_runtime::context::ConversationManager,
        view_mode: crate::event::ViewMode,
    ) -> Result<SnapshotId> {
        let mut observer = crate::event::CliEventHandler::new(view_mode);
        self.execute_turn_with_observer(cfg, prompt, messages, &mut observer)
            .await
    }

    pub async fn execute_turn_with_observer(
        &self,
        cfg: &CrowConfig,
        prompt: &str,
        messages: &mut crow_runtime::context::ConversationManager,
        observer: &mut dyn crate::event::EventHandler,
    ) -> Result<SnapshotId> {
        let file_state_store = std::sync::Arc::new(crow_runtime::file_state::FileStateStore::new());
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

        // Emit structured phase transition: Materializing
        observer.handle_event(crate::event::AgentEvent::Turn(
            crate::event::TurnEvent::PhaseChanged {
                turn_id: String::new(),
                phase: crate::event::TurnPhase::Materializing,
            },
        ));

        // ── Step 1: Freeze baseline BEFORE planning ──
        let frozen_sandbox = self.materialize_baseline(&profile).await?;
        let frozen_root = frozen_sandbox.path().to_path_buf();

        // Emit structured phase transition: BuildingRepoMap
        observer.handle_event(crate::event::AgentEvent::Turn(
            crate::event::TurnEvent::PhaseChanged {
                turn_id: String::new(),
                phase: crate::event::TurnPhase::BuildingRepoMap,
            },
        ));

        // Build repo map from the FROZEN snapshot, not live workspace
        let repo_map = self.build_context_map_with_cache(cfg, &snapshot_id, &frozen_root)?;

        let _ = self.ledger.lock().map_err(|_| anyhow::anyhow!("Ledger lock poisoned"))?.append(
            crow_workspace::ledger::LedgerEvent::SnapshotCreated {
                id: snapshot_id.clone(),
                git_hash: snapshot_id.0.clone(),
                timestamp: chrono::Utc::now(),
            },
        );

        let available_skills = self.load_and_resolve_skills(prompt, observer);

        let sys_msgs = crate::prompt::PromptBuilder::new()
            .with_context_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_dynamic_skills(&available_skills)
            .with_contract(&snapshot_id)
            .build();

        messages.set_system(sys_msgs);

        if messages.as_messages().len() <= 2 {
            messages.push_user(format!("Task:\n{prompt}"));
        } else {
            messages.push_user(prompt);
        }

        // Emit structured phase transition: Compacting
        observer.handle_event(crate::event::AgentEvent::Turn(
            crate::event::TurnEvent::PhaseChanged {
                turn_id: String::new(),
                phase: crate::event::TurnPhase::Compacting,
            },
        ));
        observer.handle_event(crate::event::AgentEvent::Compacting { active: true });
        match messages.compact_history(&self.compiler).await {
            Ok(true) => {
                observer.handle_event(crate::event::AgentEvent::Compacting { active: false });
            }
            Ok(false) => {
                // No compaction needed — silently clear the indicator
                observer.handle_event(crate::event::AgentEvent::Compacting { active: false });
            }
            Err(e) => {
                observer.handle_event(crate::event::AgentEvent::Compacting { active: false });
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "Warning: Context compression failed: {e}"
                )));
            }
        }

        // ── Step 2: Epistemic loop against FROZEN baseline ──
        let plan = crow_runtime::epistemic::run_epistemic_loop(
            &self.compiler,
            messages,
            &frozen_root, // FROZEN SNAPSHOT — not live workspace
            Some(&self.mcp_manager),
            observer,
            std::sync::Arc::clone(&file_state_store),
            std::sync::Arc::clone(&self.tool_registry),
            std::sync::Arc::clone(&self.permissions),
        )
        .await?;

        if plan.operations.is_empty() {
            if !plan.rationale.trim().is_empty() {
                observer.handle_event(crate::event::AgentEvent::Markdown(plan.rationale.clone()));
            }
            return Ok(snapshot_id);
        }

        observer.handle_event(crate::event::AgentEvent::Log(format!(
            "Code modification proposed ({} ops). Verifying...",
            plan.operations.len()
        )));

        // Capture the agent's rationale before the plan is moved into the crucible.
        // This ensures every turn produces a readable final summary.
        let turn_rationale = plan.rationale.clone();
        let _turn_op_count = plan.operations.len();

        // ── Step 3: Crucible verification against the SAME frozen baseline ──
        let mcts_config = crate::mcts::MctsConfig::from_env();
        if !mcts_config.is_serial() {
            if !cfg.llm.prompt_caching {
                observer.handle_event(crate::event::AgentEvent::Log(format!(
                    "Warning: MCTS parallel mode (CROW_MCTS_BRANCHES={}) is running without prompt_caching enabled.",
                    mcts_config.branch_factor
                )));
            }

            let winner = crate::crucible_runner::run_mcts_crucible(
                &mcts_config,
                &profile,
                &candidate,
                &self.workspace,
                &frozen_root,
                &self.compiler,
                messages,
                &snapshot_id,
                Some(&self.mcp_manager),
                observer,
            )
            .await?;

            if let Some(w) = winner {
                let plan_id = format!(
                    "mcts-{}-{}",
                    snapshot_id.0,
                    chrono::Utc::now().timestamp_millis()
                );
                crate::crucible_runner::apply_winning_plan(
                    cfg,
                    w.sandbox.path(),
                    &w.plan,
                    &plan_id,
                    &snapshot_id,
                    &self.ledger,
                    observer,
                )
                .await?;

                // Emit the agent's rationale as a proper final summary
                if !turn_rationale.trim().is_empty() {
                    observer.handle_event(crate::event::AgentEvent::Markdown(turn_rationale));
                }
            }
            return Ok(snapshot_id);
        }

        // Serial crucible: use the same frozen baseline for verification
        let crucible = crate::crucible::SerialCrucible {
            cfg,
            profile: &profile,
            candidate: &candidate,
            frozen_root: &frozen_root,
            compiler: &self.compiler,
            mcp_manager: Some(&self.mcp_manager),
        };

        // Pass the already compiled plan as a jump-start for verification
        let target_snap = crucible
            .execute_with_precompiled(messages, &snapshot_id, &self.ledger, plan, observer)
            .await?;

        // P0-3: Emit the agent's rationale as a final summary cell
        // This replaces the old "Done" with actual readable content.
        if !turn_rationale.trim().is_empty() {
            observer.handle_event(crate::event::AgentEvent::Markdown(turn_rationale));
        }

        Ok(target_snap)
    }

    /// Execute a turn using the native tool-calling agent loop.
    ///
    /// Unlike `execute_turn_with_observer` which uses the legacy epistemic
    /// loop + crucible verification pipeline, this method uses the new
    /// streaming tool-call state machine where the LLM directly invokes
    /// tools (bash, file_edit, file_write, grep, etc.) and writes to the
    /// workspace in real-time.
    ///
    /// The agent is responsible for its own verification (e.g., running
    /// `cargo check` via the bash tool) rather than relying on the
    /// automated crucible.
    pub async fn execute_native_turn(
        &self,
        cfg: &CrowConfig,
        prompt: &str,
        messages: &mut crow_runtime::context::ConversationManager,
        observer: &mut dyn crate::event::EventHandler,
    ) -> Result<SnapshotId> {
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);

        let _profile = crate::scan_workspace(&self.workspace).map_err(|e| anyhow::anyhow!(e))?;

        // Emit structured phase transitions
        observer.handle_event(crate::event::AgentEvent::Turn(
            crate::event::TurnEvent::PhaseChanged {
                turn_id: String::new(),
                phase: crate::event::TurnPhase::BuildingRepoMap,
            },
        ));

        // Build repo map from live workspace (no frozen snapshot needed for native mode)
        let repo_map = self.build_context_map_with_cache(cfg, &snapshot_id, &self.workspace)?;

        let available_skills = self.load_and_resolve_skills(prompt, observer);

        let sys_msgs = crate::prompt::PromptBuilder::new()
            .with_context_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_dynamic_skills(&available_skills)
            .with_contract(&snapshot_id)
            .build();

        messages.set_system(sys_msgs);

        if messages.as_messages().len() <= 2 {
            messages.push_user(format!("Task:\n{prompt}"));
        } else {
            messages.push_user(prompt);
        }

        // Compact if needed
        observer.handle_event(crate::event::AgentEvent::Compacting { active: true });
        let _ = messages.compact_history(&self.compiler).await;
        observer.handle_event(crate::event::AgentEvent::Compacting { active: false });

        // Run the native agent loop
        observer.handle_event(crate::event::AgentEvent::Turn(
            crate::event::TurnEvent::PhaseChanged {
                turn_id: String::new(),
                phase: crate::event::TurnPhase::EpistemicLoop { step: 0, max_steps: 40 },
            },
        ));

        let result = crow_runtime::agent_loop::run_agent_loop(
            &self.compiler,
            messages,
            &self.workspace,  // Live workspace — agent writes directly
            std::sync::Arc::clone(&self.tool_registry),
            std::sync::Arc::clone(&self.permissions),
            observer,
        )
        .await?;

        // Emit final text as markdown
        if !result.final_text.trim().is_empty() {
            observer.handle_event(crate::event::AgentEvent::Markdown(result.final_text));
        }

        observer.handle_event(crate::event::AgentEvent::Turn(
            crate::event::TurnEvent::PhaseChanged {
                turn_id: String::new(),
                phase: crate::event::TurnPhase::Complete,
            },
        ));

        Ok(snapshot_id)
    }

    // ─── Unified Entry Points ────────────────────────────────────────────────

    fn get_or_build_context_map(&self, cfg: &CrowConfig) -> Result<Arc<crow_intel::ContextMap>> {
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);
        if let Ok(guard) = self.cached_repo_map.lock() {
            if let Some((cached_snap, map)) = guard.as_ref() {
                if cached_snap == &snapshot_id {
                    return Ok(Arc::clone(map));
                }
            }
        }
        let map = cfg
            .build_context_map_for(&self.workspace)
            .map_err(|e| anyhow::anyhow!(e))
            .context("Failed to build repo map")?;
        let arc_map = Arc::new(map);
        if let Ok(mut guard) = self.cached_repo_map.lock() {
            *guard = Some((snapshot_id, Arc::clone(&arc_map)));
        }
        Ok(arc_map)
    }

    pub async fn compile_only(&self, cfg: &CrowConfig, prompt: &str) -> Result<()> {
        println!("🦅 crow-code Compile-Only mode initializing...\n");

        println!("[1/3] Gathering Repomap Context via tree-sitter...");
        let repo_map = self.get_or_build_context_map(cfg)?;
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);
        println!(
            "    🎯 Compressed map length: {} bytes",
            repo_map.map_text.len()
        );

        println!(
            "\n[2/3] Compiling IntentPlan via crow-brain (Engine: {})...",
            cfg.describe_provider()
        );

        let sys_msgs = crate::prompt::PromptBuilder::default()
            .with_context_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        let mut messages = crow_runtime::context::ConversationManager::new(sys_msgs);
        messages.push_user(format!("Task:\n{prompt}"));

        match self.compiler.compile_action(&messages.as_messages()).await {
            Ok(action) => {
                println!("\n[✓] Compilation Successful!");
                println!("--- Parsed AgentAction ---");
                println!("{}", serde_json::to_string_pretty(&action)?);
                Ok(())
            }
            Err(e) => {
                eprintln!("\n[✗] Compilation Failed: {e:?}");
                anyhow::bail!("Failed to compile AgentAction")
            }
        }
    }

    pub async fn generate_plan(&self, cfg: &CrowConfig, prompt: &str) -> Result<()> {
        use crate::evidence_report::*;
        use crow_workspace::PlanHydrator;

        println!("🦅 crow plan — Evidence-First Preview\n");
        println!("  Write mode: {}", cfg.write_mode);

        println!("\n[1/5] Workspace Recon...");
        let profile = crate::scan_workspace(&self.workspace).map_err(|e| anyhow::anyhow!(e))?;

        let file_count = std::fs::read_dir(&self.workspace)
            .map(std::iter::Iterator::count)
            .unwrap_or(0);
        let snapshot_id = crate::snapshot::resolve_snapshot_id(&self.workspace);

        let recon = ReconSummary {
            language: profile.primary_lang.name.clone(),
            tier: format!("{:?}", profile.primary_lang.tier),
            snapshot_id: snapshot_id.clone(),
            files_scanned: file_count,
            manifests: vec![],
        };
        println!(
            "  ✅ {} ({}) | {} files",
            recon.language, recon.tier, recon.files_scanned
        );

        println!("\n[2/5] Materializing sandbox & compiling plan...");
        let mat_config = MaterializeConfig {
            source: self.workspace.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
            .await?
            .context("Failed to materialize sandbox")?;
        let frozen_root = sandbox.path().to_path_buf();

        let repo_map = cfg
            .build_context_map_for(&frozen_root)
            .map_err(|e| anyhow::anyhow!(e))?;

        let sys_msgs = crate::prompt::PromptBuilder::default()
            .with_context_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        let mut messages = crow_runtime::context::ConversationManager::new(sys_msgs);
        messages.push_user(format!("Task:\n{prompt}"));

        let mut obs = crate::event::CliEventHandler::default();
        let file_state_store = std::sync::Arc::new(crow_runtime::file_state::FileStateStore::new());
        let compiled_plan = crow_runtime::epistemic::run_epistemic_loop(
            &self.compiler,
            &mut messages,
            &frozen_root,
            Some(&self.mcp_manager),
            &mut obs,
            std::sync::Arc::clone(&file_state_store),
            std::sync::Arc::clone(&self.tool_registry),
            std::sync::Arc::clone(&self.permissions),
        )
        .await?;

        let compilation = CompilationSummary::from_plan(&compiled_plan);
        println!(
            "  ✅ {} ops, {:?} confidence",
            compilation.total_ops(),
            compilation.confidence
        );

        println!("\n[3/5] Hydrating plan against frozen sandbox...");
        let plan_clone = compiled_plan.clone();
        let frozen_clone = frozen_root.clone();
        let snap_clone = snapshot_id.clone();
        let hydrated_plan = tokio::task::spawn_blocking(move || {
            PlanHydrator::hydrate(&plan_clone, &snap_clone, &frozen_clone)
        })
        .await?
        .context("Hydration failed")?;

        let hydration = HydrationSummary {
            snapshot_verified: true,
            hashes_matched: hydrated_plan.operations.len(),
            hashes_total: hydrated_plan.operations.len(),
            drift_warnings: vec![],
        };
        println!(
            "  ✅ Snapshot anchored, {}/{} hashes verified",
            hydration.hashes_matched, hydration.hashes_total
        );

        println!("\n[4/5] Running preflight compile check...");
        let plan_for_apply = hydrated_plan.clone();
        let sandbox_view = sandbox.non_owning_view();
        tokio::task::spawn_blocking(move || {
            crow_workspace::applier::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
        })
        .await?
        .context("Apply failed")?;

        let preflight_start = std::time::Instant::now();
        let preflight_result = crow_verifier::preflight::run_preflight(
            sandbox.path(),
            Some(&frozen_root),
            std::time::Duration::from_secs(30),
            &profile.primary_lang,
        )
        .await;

        let preflight = PreflightSummary {
            language: profile.primary_lang.name.clone(),
            outcome: match &preflight_result {
                crow_verifier::preflight::PreflightResult::Clean => PreflightOutcome::Clean {
                    duration_secs: preflight_start.elapsed().as_secs_f64(),
                },
                crow_verifier::preflight::PreflightResult::Errors(diags) => {
                    PreflightOutcome::Errors {
                        count: diags.len(),
                        summary: crow_verifier::preflight::format_diagnostics(diags),
                    }
                }
                crow_verifier::preflight::PreflightResult::Skipped(r) => {
                    PreflightOutcome::Skipped { reason: r.clone() }
                }
            },
        };

        let compile_passed = matches!(
            preflight_result,
            crow_verifier::preflight::PreflightResult::Clean
                | crow_verifier::preflight::PreflightResult::Skipped(_)
        );
        let evidence = crow_evidence::types::EvidenceMatrix {
            compile_runs: vec![crow_evidence::types::TestRun {
                command: format!("preflight ({})", profile.primary_lang.name),
                outcome: if compile_passed {
                    crow_evidence::types::TestOutcome::Passed
                } else {
                    crow_evidence::types::TestOutcome::Failed
                },
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
        let report = EvidenceReport {
            recon,
            compilation,
            hydration,
            preflight,
            verdict,
        };
        println!("{report}");
        println!("\n─── Planned Changes ───");
        crate::diff::render_plan_diff(&frozen_root, sandbox.path(), &hydrated_plan);

        if let Ok(store) = crow_runtime::session::SessionStore::open() {
            let mut sess = crow_runtime::session::Session::new(&cfg.workspace, prompt);
            sess.save_messages(&messages.as_messages());
            sess.push_snapshot(snapshot_id);
            if store.save(&sess).is_ok() {
                println!("\n  💾 Session saved: {}", sess.id.0);
            }
        }
        Ok(())
    }

    pub async fn resume(&self, cfg: &CrowConfig, session_id: &str) -> Result<()> {
        println!(
            "🦅 crow session resume — continuing session {}",
            &session_id[..8.min(session_id.len())]
        );

        let store = crow_runtime::session::SessionStore::open()?;
        let mut loaded_session = store.load(&crow_runtime::session::SessionId(session_id.to_string()))?;

        println!("  Workspace: {}", loaded_session.workspace_root.display());
        println!("  Task: {}", loaded_session.task);

        let restored_messages = loaded_session.restore_messages();
        println!(
            "  Restored {} messages from history",
            restored_messages.len()
        );

        if !loaded_session.workspace_root.exists() {
            anyhow::bail!(
                "Workspace no longer exists: {}",
                loaded_session.workspace_root.display()
            );
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

        let profile = crate::scan_workspace(&loaded_session.workspace_root)
            .map_err(|e| anyhow::anyhow!(e))?;
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
            .await?
            .context("Failed to materialize sandbox")?;
        let frozen_root = sandbox.path().to_path_buf();

        let repo_map = cfg
            .build_context_map_for(&frozen_root)
            .map_err(|e| anyhow::anyhow!(e))?;

        let sys_msgs = crate::prompt::PromptBuilder::default()
            .with_context_map(&repo_map, &snapshot_id)
            .with_mcp(Some(&self.mcp_manager))
            .with_contract(&snapshot_id)
            .build();

        let mut messages = crow_runtime::context::ConversationManager::new(sys_msgs);

        for msg in &restored_messages {
            match msg.role {
                crow_brain::ChatRole::User => messages.push_user(&msg.content),
                crow_brain::ChatRole::Assistant => messages.push_assistant(&msg.content),
                crow_brain::ChatRole::System => {}
                crow_brain::ChatRole::Tool => {
                    // Tool results are pushed with their tool_call_id if available
                    if let Some(ref tc_id) = msg.tool_call_id {
                        messages.push_tool_result(tc_id, &msg.content);
                    } else {
                        messages.push_user(&msg.content);
                    }
                }
            }
        }

        messages.push_user(format!(
            "[SESSION RESUMED]\nContinuing work on the original task: {}\n\nPlease pick up where you left off. If the previous attempt failed, try a different approach.",
            loaded_session.task
        ));

        println!("  Entering crucible loop...\n");

        let mut obs = crate::event::CliEventHandler::default();
        let file_state_store = std::sync::Arc::new(crow_runtime::file_state::FileStateStore::new());
        let compiled_plan = crow_runtime::epistemic::run_epistemic_loop(
            &self.compiler,
            &mut messages,
            &frozen_root,
            Some(&self.mcp_manager),
            &mut obs,
            std::sync::Arc::clone(&file_state_store),
            std::sync::Arc::clone(&self.tool_registry),
            std::sync::Arc::clone(&self.permissions),
        )
        .await?;

        let attempt_mat_config = MaterializeConfig {
            source: frozen_root.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false,
        };
        let attempt_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
            .await?
            .context("Failed to materialize attempt sandbox")?;

        let attempt_sandbox_path = attempt_sandbox.path().to_path_buf();
        let plan_clone = compiled_plan.clone();
        let snap_clone = snapshot_id.clone();
        let hydrated_plan = tokio::task::spawn_blocking(move || {
            crow_workspace::PlanHydrator::hydrate(&plan_clone, &snap_clone, &attempt_sandbox_path)
        })
        .await?
        .context("Hydration failed")?;

        let plan_for_apply = hydrated_plan.clone();
        let sandbox_view = attempt_sandbox.non_owning_view();
        tokio::task::spawn_blocking(move || {
            crow_workspace::applier::apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
        })
        .await?
        .context("Failed to apply plan to sandbox")?;

        let preflight_result = crow_verifier::preflight::run_preflight(
            attempt_sandbox.path(),
            Some(&frozen_root),
            std::time::Duration::from_secs(60),
            &profile.primary_lang,
        )
        .await;

        match &preflight_result {
            crow_verifier::preflight::PreflightResult::Clean => {
                println!("  ✅ Preflight: compiles cleanly");
            }
            crow_verifier::preflight::PreflightResult::Errors(diags) => {
                let summary = crow_verifier::preflight::format_diagnostics(diags);
                eprintln!("  ❌ Preflight: {} compile error(s)", diags.len());
                eprintln!("{summary}");
                anyhow::bail!(
                    "Resume aborted: preflight compile check failed with {} error(s).\n\
                     Re-enter the session and fix the compile errors before resuming.\n{}",
                    diags.len(),
                    summary
                );
            }
            crow_verifier::preflight::PreflightResult::Skipped(reason) => {
                eprintln!("  ⚠️  Preflight skipped: {reason}");
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
        )
        .await
        .context("Verification execution failed")?;

        let outcome = &result.test_run.outcome;
        if outcome != &crow_evidence::TestOutcome::Passed {
            println!("  ❌ Resume verdict: {outcome:?}");
            anyhow::bail!("Resumed session: verification failed.");
        }
        println!("  ✅ Resume verdict: PASSED");

        if matches!(
            cfg.write_mode,
            crate::config::WriteMode::WorkspaceWrite | crate::config::WriteMode::DangerFullAccess
        ) {
            println!("\n[4] Writing verified changes to workspace...");
            if let Err(e) =
                crow_workspace::applier::apply_sandbox_to_workspace(&cfg.workspace, &hydrated_plan).await
            {
                eprintln!("  ❌ Failed to apply to workspace: {e:?}");
                anyhow::bail!("Workspace mutation failed during resume.");
            }

            println!("  ✅ Workspace updated successfully.");
            if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, &hydrated_plan) {
                println!("  ⚠️  Could not automatically commit changes: {e}");
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
