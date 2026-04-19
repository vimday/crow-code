mod budget;
pub mod chat;
mod config;
mod context;
pub mod crucible;
mod diff;
mod epistemic;
pub mod epistemic_ui;
pub mod event;
mod evidence_report;
mod legacy_god;
mod mcp;
pub mod mcts;
pub mod prompt;
pub mod render;
pub mod runtime;
mod session;
pub mod snapshot;
pub mod subagent;
pub mod thread_manager;
pub mod tui;

use anyhow::Result;
use config::CrowConfig;

use crow_materialize::MaterializeConfig;
use crow_probe::scan_workspace;
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str());

    match cmd {
        Some("run") => run_dry_run(&args[2..]).await,
        Some("plan") => run_plan(&args[2..]).await,
        Some("compile") => run_compile_only(&args[2..]).await,
        Some("dry-run") => run_dry_run(&args[2..]).await,
        Some("session") => handle_session_command(&args[2..]).await,
        Some("dashboard") => tui::run_dashboard(std::env::current_dir()?).await,
        Some("dream") => run_autodream().await,
        Some("mcp") => handle_mcp_command(&args[2..]).await,
        Some("legacy-god") => legacy_god::run_god_pipeline().await,
        Some("--help") | Some("-h") | Some("help") => {
            print_help();
            Ok(())
        }
        Some("chat") => chat::run_repl(&CrowConfig::load()?).await,
        Some(cmd) if cmd == "-r" || cmd == "--resume" => {
            // Drop into Console 4.0 Ratatui Workbench with resume flag
            tui::run_workbench(&CrowConfig::load()?, true).await
        }
        Some(unknown) => {
            eprintln!("Unknown command: {}", unknown);
            print_help();
            std::process::exit(1);
        }
        None => {
            // Drop into Console 4.0 Ratatui Workbench
            tui::run_workbench(&CrowConfig::load()?, false).await
        }
    }
}

fn print_help() {
    eprintln!(
        r#"
🦅 crow — evidence-driven coding agent

USAGE:
    crow <COMMAND> [OPTIONS]

COMMANDS:
    chat                      (or no args) Start the Continuous Chat REPL
    run <prompt>              Full autonomous loop (serial or MCTS)
    plan <prompt>             Compile and preview plan with evidence report
    compile <prompt>          Compile-only: show the IntentPlan JSON
    session list              List saved sessions
    session resume <id>       Resume a saved session
    dashboard                 Open the interactive EventLedger & Dream dashboard
    dream                     Run background AutoDream memory consolidation
    mcp                       Manage MCP tools
    dry-run <prompt>          Alias for 'run'
    help                      Show this help

ENVIRONMENT:
    OPENAI_API_KEY            API key (or CROW_API_KEY)
    LLM_BASE_URL              Provider endpoint
    LLM_MODEL                 Model name
    LLM_PROVIDER              Provider type (openai, custom)
    CROW_WRITE_MODE           sandbox | write | danger (default: write)
    CROW_MCTS_BRANCHES        MCTS branch factor (default: 3)
    CROW_MAP_BUDGET           Repo map size budget in bytes

SAFETY:
    crow defaults to workspace-write mode. All mutations go through
    sandboxed verification before touching your workspace. Failed
    operations leave the workspace untouched (zero-pollution guarantee).
"#
    );
}

async fn handle_session_command(args: &[String]) -> Result<()> {
    let subcmd = args.first().map(|s| s.as_str());
    match subcmd {
        Some("list") => {
            let store = session::SessionStore::open()?;
            let sessions = store.list()?;
            if sessions.is_empty() {
                println!("No saved sessions.");
            } else {
                println!(
                    "  ID       │ Task                                     │ Snapshots │ Updated"
                );
                println!(
                    "  ─────────┼──────────────────────────────────────────┼───────────┼────────"
                );
                for s in &sessions {
                    println!("{}", s);
                }
            }
            Ok(())
        }
        Some("resume") => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: crow session resume <session-id>"))?;
            println!("  (use `crow session resume-run <id>` to actually continue execution)");
            let store = session::SessionStore::open()?;
            let session = store.load(&session::SessionId(id.clone()))?;
            println!("Resuming session: {}", session.id.0);
            println!("  Workspace: {}", session.workspace_root.display());
            println!("  Task: {}", session.task);
            println!("  Messages: {}", session.restore_messages().len());
            println!("  Snapshots: {}", session.snapshot_timeline.len());
            Ok(())
        }
        Some("resume-run") => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: crow session resume-run <session-id>"))?;
            // Delegate to async resume
            resume_session_run(id).await
        }
        _ => {
            eprintln!("Usage: crow session <list|resume|resume-run>");
            Ok(())
        }
    }
}

async fn resume_session_run(session_id: &str) -> Result<()> {
    let cfg = CrowConfig::load()?;
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime.resume(&cfg, session_id).await
}

// ─── Compile-Only command ───────────────────────────────────────────

async fn run_compile_only(args: &[String]) -> Result<()> {
    let prompt = args.join(" ");
    let cfg = CrowConfig::load()?;
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime.compile_only(&cfg, &prompt).await
}

// ─── Plan command (Evidence-First Preview) ──────────────────────────

/// `crow plan <prompt>` — compile a plan and display a full evidence report
/// WITHOUT applying changes. This serves as a dry-run for verification tests.
async fn run_plan(args: &[String]) -> Result<()> {
    let prompt = args.join(" ");
    let cfg = CrowConfig::load()?;
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime.generate_plan(&cfg, &prompt).await
}

async fn run_dry_run(args: &[String]) -> Result<()> {
    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");
    let mut messages = context::ConversationManager::new(vec![]);
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime
        .execute_turn(
            &cfg,
            &prompt,
            &mut messages,
            crate::event::ViewMode::default(),
        )
        .await
        .map(|_| ())
}

// `apply_sandbox_to_workspace` moved to `crow_workspace::applier`.

/// Pre-warm the Cargo build cache by running `cargo check` on the frozen
/// sandbox. This populates the `CARGO_TARGET_DIR` (keyed to `frozen_root`)
/// with all dependency artifacts so MCTS branches only need incremental
/// recompilation of the patched crate(s).
///
/// Failure is non-fatal: if the warm-up fails (e.g. the project doesn't
async fn warm_build_cache(
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
                "    ⚠️  Build cache warm-up failed: {:?} — continuing without cache",
                e
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
        config::WriteMode::SandboxOnly => {
            observer.handle_event(crate::event::AgentEvent::Log("  📦 Write mode: sandbox-only — changes remain in sandbox (not applied to workspace)".into()));
            observer.handle_event(crate::event::AgentEvent::Log("     Use CROW_WRITE_MODE=write to enable workspace application.".into()));
        }
        config::WriteMode::WorkspaceWrite => {
            observer.handle_event(crate::event::AgentEvent::Log(
                "  ✍️  Write mode: workspace-write — applying verified changes to workspace...".into()
            ));
            if let Err(e) =
                crow_workspace::applier::apply_sandbox_to_workspace(&cfg.workspace, hydrated_plan)
            {
                observer.handle_event(crate::event::AgentEvent::Log(format!("  ❌ Failed to apply to workspace: {:?}", e)));
                observer.handle_event(crate::event::AgentEvent::Log(format!("     Sandbox remains at: {}", sandbox_path.display())));
                anyhow::bail!("Workspace application failed: {:?}", e);
            } else {
                observer.handle_event(crate::event::AgentEvent::Log("  ✅ Workspace updated successfully.".into()));
                if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, hydrated_plan)
                {
                    observer.handle_event(crate::event::AgentEvent::Log(format!("  ⚠️  Could not automatically commit changes: {}", e)));
                } else {
                    observer.handle_event(crate::event::AgentEvent::Log("  ✅ Changes committed to git timeline.".into()));
                }
            }
        }
        config::WriteMode::DangerFullAccess => {
            observer.handle_event(crate::event::AgentEvent::Log(
                "  ⚠️  Write mode: danger-full-access — applying without additional checks...".into()
            ));
            if let Err(e) =
                crow_workspace::applier::apply_sandbox_to_workspace(&cfg.workspace, hydrated_plan)
            {
                observer.handle_event(crate::event::AgentEvent::Log(format!("  ❌ Failed to apply to workspace: {:?}", e)));
                anyhow::bail!("Workspace application failed: {:?}", e);
            } else {
                observer.handle_event(crate::event::AgentEvent::Log("  ✅ Workspace updated.".into()));
                if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, hydrated_plan)
                {
                    observer.handle_event(crate::event::AgentEvent::Log(format!("  ⚠️  Could not automatically commit changes: {}", e)));
                } else {
                    observer.handle_event(crate::event::AgentEvent::Log("  ✅ Changes committed to git timeline.".into()));
                }
            }
        }
    }

    if cfg.write_mode != config::WriteMode::SandboxOnly {
        let _ = ledger.lock().unwrap().append(crow_workspace::ledger::LedgerEvent::PlanApplied {
            plan_id: plan_id.to_string(),
            snapshot_id: snapshot_id.clone(),
            timestamp: chrono::Utc::now(),
        });
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
    // 1. Initial Epistemic Loop (Serial Recon)
    observer.handle_event(crate::event::AgentEvent::Log("Entering Epistemic Recon Loop (MCTS Pre-exploration)...".into()));
    let baseline_plan =
        epistemic::run_epistemic_loop(compiler, messages, frozen_root, mcp_manager, observer)
            .await?;

    if baseline_plan.operations.is_empty() {
        observer.handle_event(crate::event::AgentEvent::Log("Conversational Intent Detected (No codebase changes proposed)".into()));
        observer.handle_event(crate::event::AgentEvent::Markdown(baseline_plan.rationale.clone()));
        return Ok(None);
    }

    observer.handle_event(crate::event::AgentEvent::Log("Seeding baseline plan into MCTS branch 0...".into()));

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
                "Winning Patch (Branch {}) passed verifier.\nEvidence:\n{}", winner.branch_id, winner.log
            )));

            return Ok(Some(winner));
        }

        // All branches failed. Feed diagnostics back and re-derive baseline.
        observer.handle_event(crate::event::AgentEvent::Log(format!(
            "[❗] MCTS Round {} failed! Feeding diagnostics back to LLM...",
            mcts_round
        )));
        let merged = crate::mcts::merge_diagnostics(&outcomes);
        messages.push_verifier_result("MCTS_AllBranchesFailed", &merged);

        // Re-compile a fresh baseline plan that incorporates the failure
        // feedback. This ensures branch 0 in the next round gets an
        // informed plan instead of repeating the same stale one.
        if mcts_round < actual_mcts_config.max_rounds {
            observer.handle_event(crate::event::AgentEvent::Log("  🧠 Re-deriving baseline plan from failure feedback...".into()));
            match compiler.compile_action(&messages.as_messages()).await {
                Ok(crow_patch::AgentAction::SubmitPlan { plan }) => {
                    observer.handle_event(crate::event::AgentEvent::Log("    ✅ New baseline plan generated for next round".into()));
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
                        "    ⚠️  Baseline re-derivation failed: {:?} — reusing previous",
                        e
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

// ─── MCP Commands ───────────────────────────────────────────────────

async fn handle_mcp_command(args: &[String]) -> Result<()> {
    let subcmd = args.first().map(|s| s.as_str());
    match subcmd {
        Some("list-tools") => {
            let server_name = args.get(1).map(|s| s.as_str());
            let cfg = CrowConfig::load()?;

            let (name, mcp_cfg) = if let Some(n) = server_name {
                let conf = cfg
                    .mcp_servers
                    .get(n)
                    .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", n))?;
                (n, conf)
            } else {
                if cfg.mcp_servers.is_empty() {
                    anyhow::bail!("No MCP servers configured in .crow/config.json");
                }
                // Just grab the first one
                let first = cfg.mcp_servers.iter().next().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Expected at least one MCP server, but map was empty after check"
                    )
                })?;
                (first.0.as_str(), first.1)
            };

            println!(
                "🔌 Connecting to MCP Server: {} ({} {})",
                name,
                mcp_cfg.command,
                mcp_cfg.args.join(" ")
            );

            let args_refs: Vec<&str> = mcp_cfg.args.iter().map(|s| s.as_str()).collect();
            let client = crow_mcp::McpClient::spawn(&mcp_cfg.command, &args_refs)?;

            println!("  Initializing handshake...");
            let init = client.initialize().await?;
            println!(
                "  ✅ Server initialized: {} v{}",
                init.server_info.name, init.server_info.version
            );

            println!("  Fetching tools...");
            let tools = client.list_tools().await?;
            println!("\n🛠️  Available Tools ({}):", tools.tools.len());
            for tool in tools.tools {
                println!(
                    "  - {} : {}",
                    tool.name,
                    tool.description.as_deref().unwrap_or("No description")
                );
            }
            Ok(())
        }
        _ => {
            eprintln!("Usage: crow mcp list-tools [server-name]");
            Ok(())
        }
    }
}

// ─── Ledger ─────────────────────────────────────────────────────────

pub(crate) fn open_ledger(
    workspace: &std::path::Path,
) -> Result<crow_workspace::ledger::EventLedger> {
    use anyhow::Context;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    workspace.to_string_lossy().hash(&mut hasher);
    let hash = format!("{:x}", hasher.finish());

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .context("Could not determine home directory")?;

    let ledger_dir = home.join(".crow").join("ledger");
    std::fs::create_dir_all(&ledger_dir)?;

    let log_path = ledger_dir.join(format!("{}.jsonl", hash));
    Ok(crow_workspace::ledger::EventLedger::open(&log_path)?)
}

// ─── AutoDream ──────────────────────────────────────────────────────

async fn run_autodream() -> Result<()> {
    let cfg = config::CrowConfig::load()?;
    println!("🦅 crow dream — Background Memory Consolidation");

    let dreamer = crow_brain::autodream::AutoDream::new(&cfg.workspace)?;
    let client = cfg.build_llm_client()?;
    dreamer.execute_dream_cycle(client.as_ref()).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_apply_winning_plan_sandbox_only_does_not_append_ledger() {
        let workspace = TempDir::new().unwrap();
        let ledger_dir = workspace.path().join("ledger.jsonl");
        let ledger = crow_workspace::ledger::EventLedger::open(&ledger_dir).unwrap();

        let cfg = config::CrowConfig {
            workspace: workspace.path().to_path_buf(),
            write_mode: config::WriteMode::SandboxOnly,
            llm: Default::default(),
            map_budget: 1024,
            mcp_servers: Default::default(),
        };

        let sandbox = TempDir::new().unwrap();
        let plan = crow_patch::IntentPlan {
            base_snapshot_id: crow_patch::SnapshotId("snap-123".into()),
            rationale: "test".into(),
            is_partial: false,
            confidence: crow_patch::Confidence::High,
            requires_mcts: true,
            operations: vec![],
        };

        let snap_id = crow_patch::SnapshotId("snap-123".into());

        let ledger = std::sync::Mutex::new(ledger);
        let mut obs = crate::event::CliEventHandler::new(crate::event::ViewMode::default());
        apply_winning_plan(
            &cfg,
            sandbox.path(),
            &plan,
            "test-plan",
            &snap_id,
            &ledger,
            &mut obs,
        )
        .await
        .unwrap();

        // Ledger should be empty for SandboxOnly mode
        let contents = fs::read_to_string(&ledger_dir).unwrap_or_default();
        assert!(
            contents.is_empty(),
            "SandboxOnly should not emit PlanApplied event to ledger"
        );
    }
}
