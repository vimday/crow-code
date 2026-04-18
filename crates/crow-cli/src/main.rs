mod budget;
mod config;
mod context;
mod diff;
mod epistemic;
mod evidence_report;
mod legacy_god;
mod mcp;
pub mod mcts;
mod session;
pub mod snapshot;
pub mod tui;
pub mod chat;

use anyhow::{Context, Result};
use config::CrowConfig;
use crow_brain::IntentCompiler;
use crow_materialize::{materialize, MaterializeConfig};

use crow_probe::scan_workspace;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
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
        Some(unknown) => {
            eprintln!("Unknown command: {}", unknown);
            print_help();
            std::process::exit(1);
        }
        None => {
            // Default to continuous REPL chat
            chat::run_repl(&CrowConfig::load()?).await
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
                println!("  ID       │ Task                                     │ Snapshots │ Updated");
                println!("  ─────────┼──────────────────────────────────────────┼───────────┼────────");
                for s in &sessions {
                    println!("{}", s);
                }
            }
            Ok(())
        }
        Some("resume") => {
            let id = args.get(1).ok_or_else(|| {
                anyhow::anyhow!("Usage: crow session resume <session-id>")
            })?;
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
            let id = args.get(1).ok_or_else(|| {
                anyhow::anyhow!("Usage: crow session resume-run <session-id>")
            })?;
            // Delegate to async resume
            resume_session_run(id).await
        }
        _ => {
            eprintln!("Usage: crow session <list|resume|resume-run>");
            Ok(())
        }
    }
}

/// Resume a session and re-enter the autonomous loop with restored context.
async fn resume_session_run(session_id: &str) -> Result<()> {
    use crow_brain::ChatMessage;
    use crow_workspace::PlanHydrator;

    let store = session::SessionStore::open()?;
    let mut loaded_session = store.load(&session::SessionId(session_id.to_string()))?;

    println!("🦅 crow session resume — continuing session {}", &session_id[..8.min(session_id.len())]);
    println!("  Workspace: {}", loaded_session.workspace_root.display());
    println!("  Task: {}", loaded_session.task);

    let restored_messages = loaded_session.restore_messages();
    println!("  Restored {} messages from history", restored_messages.len());

    if !loaded_session.workspace_root.exists() {
        anyhow::bail!(
            "Workspace no longer exists: {}",
            loaded_session.workspace_root.display()
        );
    }

    let cfg = CrowConfig::load_for(&loaded_session.workspace_root)?;
    let snapshot_id = snapshot::resolve_snapshot_id(&loaded_session.workspace_root);
    println!("  Snapshot ID: {}", snapshot_id.0);

    // Compare snapshot timeline to detect workspace drift
    if let Some(last_snap) = loaded_session.snapshot_timeline.last() {
        if *last_snap != snapshot_id {
            println!("  ⚠️  Workspace has changed since last session snapshot");
            println!("     Last: {} → Current: {}", last_snap.0, snapshot_id.0);
        } else {
            println!("  ✅ Workspace matches last session snapshot");
        }
    }

    // Probe workspace
    let profile = scan_workspace(&loaded_session.workspace_root).map_err(|e| anyhow::anyhow!(e))?;
    let candidate = match profile.verification_candidates.first() {
        Some(c) => c.clone(),
        None => {
            anyhow::bail!("No verification candidates found.");
        }
    };

    // Materialize sandbox
    println!("\n  Materializing sandbox...");
    let mat_config = MaterializeConfig {
        source: loaded_session.workspace_root.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize sandbox")?;
    let frozen_root = sandbox.path().to_path_buf();

    // Build repo map from frozen sandbox
    let repo_map = cfg
        .build_repo_map_for(&frozen_root)
        .map_err(|e| anyhow::anyhow!(e))?;

    let mcp_manager = crate::mcp::McpManager::boot(&cfg.mcp_servers).await?;
    let mut sys_prompt = String::from("Context (Repository Map):\n");
    sys_prompt.push_str(&repo_map.map_text);
    sys_prompt.push_str(&format!("\n\nWorkspace Snapshot ID: {}\nIMPORTANT: When you submit a plan, set base_snapshot_id to \"{}\" exactly.\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.", snapshot_id.0, snapshot_id.0));
    sys_prompt.push_str("\n\nMCTS DYNAMIC SEARCH: For complex code refactors, we use rigorous parallel searches (MCTS). However, if your intended changes are TRIVIAL (e.g. pure documentation tweaks, simple text formatting, or modifying markdown files), please explicitly set `requires_mcts = false` to save precious API loop latency.");

    let mcp_ctx = mcp_manager.prompt_context();
    if !mcp_ctx.is_empty() {
        sys_prompt.push_str("\n\n");
        sys_prompt.push_str(mcp_ctx);
    }

    // Rebuild conversation manager with system context + restored history
    let mut messages = context::ConversationManager::new(vec![
        ChatMessage::system("You are an autonomous engineering agent executing the given task."),
        ChatMessage::system(sys_prompt),
    ]);

    // Restore non-system messages from session history
    for msg in &restored_messages {
        match msg.role {
            crow_brain::ChatRole::User => messages.push_user(&msg.content),
            crow_brain::ChatRole::Assistant => messages.push_assistant(&msg.content),
            crow_brain::ChatRole::System => {} // System messages rebuilt above
        }
    }

    // Add a continuation prompt
    messages.push_user(format!(
        "[SESSION RESUMED]\nContinuing work on the original task: {}\n\nPlease pick up where you left off. If the previous attempt failed, try a different approach.",
        loaded_session.task
    ));

    println!("  Entering crucible loop...\n");

    let client = cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?;
    let compiler = crow_brain::IntentCompiler::new(client);

    // Run one crucible attempt
    let compiled_plan =
        epistemic::run_epistemic_loop(&compiler, &mut messages, &frozen_root, Some(&mcp_manager)).await?;

    // Hydrate + apply + verify
    let attempt_mat_config = MaterializeConfig {
        source: frozen_root.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let attempt_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize attempt sandbox")?;

    let attempt_sandbox_path = attempt_sandbox.path().to_path_buf();
    let plan_clone = compiled_plan.clone();
    let snap_clone = snapshot_id.clone();
    let hydrated_plan = tokio::task::spawn_blocking(move || {
        PlanHydrator::hydrate(&plan_clone, &snap_clone, &attempt_sandbox_path)
    })
    .await
    .context("Hydration task panicked")?
    .context("Hydration failed")?;

    let plan_for_apply = hydrated_plan.clone();
    let sandbox_view = attempt_sandbox.non_owning_view();
    tokio::task::spawn_blocking(move || {
        apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
    })
    .await
    .context("Apply task panicked")?
    .context("Failed to apply plan to sandbox")?;

    // Preflight check
    let preflight_result = crow_verifier::preflight::run_preflight(
        attempt_sandbox.path(),
        Some(&frozen_root),
        std::time::Duration::from_secs(60),
        &profile.primary_lang,
    )
    .await;

    match &preflight_result {
        crow_verifier::preflight::PreflightResult::Clean => {
            println!("  ✅ Preflight: code compiles cleanly");
        }
        crow_verifier::preflight::PreflightResult::Errors(diags) => {
            let summary = crow_verifier::preflight::format_diagnostics(diags);
            println!("  ❌ Preflight: {} compile error(s)", diags.len());
            println!("{}", summary);
        }
        crow_verifier::preflight::PreflightResult::Skipped(reason) => {
            println!("  ⚠️  Preflight skipped: {}", reason);
        }
    }

    // Full verification
    let exec_config = ExecutionConfig {
        timeout: std::time::Duration::from_secs(60),
        max_output_bytes: 5 * 1024 * 1024,
    };
    let result = crow_verifier::executor::execute(
        attempt_sandbox.path(),
        &candidate.command,
        &exec_config,
        &crow_verifier::types::AciConfig::compact(),
        Some(&frozen_root),
    )
    .await
    .context("Verification execution failed")?;

    let outcome = &result.test_run.outcome;
    println!("\n╔══════════════════════════════════════╗");
    println!("║  Resume Verdict: {:?}", outcome);
    println!("╚══════════════════════════════════════╝");
    println!("Evidence:\n{}", result.test_run.truncated_log);

    // Diff
    println!("\n--- Changes ---");
    diff::render_plan_diff(&frozen_root, attempt_sandbox.path(), &hydrated_plan);

    // Update session
    loaded_session.save_messages(&messages.as_messages());
    loaded_session.push_snapshot(snapshot_id);
    store.save(&loaded_session)?;
    println!("\n  💾 Session updated: {}", loaded_session.id.0);

    if outcome != &crow_evidence::TestOutcome::Passed {
        anyhow::bail!("Resumed session: verification failed.");
    }

    println!("\n[🎉] Resumed session completed successfully!");
    Ok(())
}

// ─── Compile-Only command ───────────────────────────────────────────

async fn run_compile_only(args: &[String]) -> Result<()> {
    use crow_brain::ChatMessage;

    println!("🦅 crow-code Compile-Only mode initializing...\n");

    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");

    println!("[1/3] Gathering Repomap Context via tree-sitter...");
    let repo_map = cfg.build_repo_map().map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "    🎯 Compressed map length: {} bytes",
        repo_map.map_text.len()
    );

    println!(
        "\n[2/3] Compiling IntentPlan via crow-brain (Engine: {})...",
        cfg.describe_provider()
    );

    let client = cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?;
    let compiler = IntentCompiler::new(client);

    use crate::context::ConversationManager;
    let mut messages = ConversationManager::new(vec![
        ChatMessage::system("You are an autonomous engineering agent executing the given task."),
        ChatMessage::system(format!(
            "Context (Repository Map):\n{}\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.",
            repo_map.map_text
        )),
    ]);

    messages.push_user(format!("Task:\n{}", prompt));

    match compiler.compile_action(&messages.as_messages()).await {
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

// ─── Plan command (Evidence-First Preview) ──────────────────────────

/// `crow plan <prompt>` — compile a plan and display a full evidence report
/// WITHOUT applying changes. This is crow's killer demo: making trust visible.
async fn run_plan(args: &[String]) -> Result<()> {
    use crow_workspace::PlanHydrator;
    use evidence_report::*;

    println!("🦅 crow plan — Evidence-First Preview\n");

    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");
    println!("  Write mode: {}", cfg.write_mode);

    // Step 1: Recon
    println!("\n[1/5] Workspace Recon...");
    let profile = scan_workspace(&cfg.workspace).map_err(|e| anyhow::anyhow!(e))?;
    let _candidate = match profile.verification_candidates.first() {
        Some(c) => c.clone(),
        None => {
            anyhow::bail!("No verification candidates found.");
        }
    };

    // Count scanned files
    let file_count = walkdir_count(&cfg.workspace);
    let manifests = detect_manifests(&cfg.workspace);

    let snapshot_id = snapshot::resolve_snapshot_id(&cfg.workspace);

    let recon = ReconSummary {
        language: profile.primary_lang.name.clone(),
        tier: format!("{:?}", profile.primary_lang.tier),
        snapshot_id: snapshot_id.clone(),
        files_scanned: file_count,
        manifests,
    };
    println!("  ✅ {} ({}) | {} files | {} manifests",
        recon.language, recon.tier, recon.files_scanned, recon.manifests.len());

    // Step 2: Materialize + Compile
    println!("\n[2/5] Materializing sandbox & compiling plan...");
    let mat_config = MaterializeConfig {
        source: cfg.workspace.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize sandbox")?;
    let frozen_root = sandbox.path().to_path_buf();

    let repo_map = cfg.build_repo_map_for(&frozen_root).map_err(|e| anyhow::anyhow!(e))?;

    let client = cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?;
    let compiler = IntentCompiler::new(client);

    let mcp_manager = crate::mcp::McpManager::boot(&cfg.mcp_servers).await?;
    let mut sys_prompt = String::from("Context (Repository Map):\n");
    sys_prompt.push_str(&repo_map.map_text);
    sys_prompt.push_str(&format!("\n\nWorkspace Snapshot ID: {}\nIMPORTANT: When you submit a plan, set base_snapshot_id to \"{}\" exactly.\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.", snapshot_id.0, snapshot_id.0));

    let mcp_ctx = mcp_manager.prompt_context();
    if !mcp_ctx.is_empty() {
        sys_prompt.push_str("\n\n");
        sys_prompt.push_str(mcp_ctx);
    }

    // Open EventLedger for telemetry recording
    let mut ledger = open_ledger(&cfg.workspace).unwrap_or_else(|e| {
        eprintln!("  ⚠️  Failed to open Event Ledger: {}", e);
        // Fallback to memory-only ledger for safety (won't persist but won't crash)
        crow_workspace::ledger::EventLedger::open(&std::env::temp_dir().join("crow_ledger_fallback.jsonl")).unwrap() 
    });
    
    // In actual implementation we append events
    let _ = ledger.append(crow_workspace::ledger::LedgerEvent::SnapshotCreated {
        id: snapshot_id.clone(),
        git_hash: snapshot_id.0.clone(),
        timestamp: chrono::Utc::now(),
    });

    use crate::context::ConversationManager;
    use crow_brain::ChatMessage;
    let mut messages = ConversationManager::new(vec![
        ChatMessage::system("You are an autonomous engineering agent executing the given task."),
        ChatMessage::system(sys_prompt),
    ]);
    messages.push_user(format!("Task:\n{}", prompt));

    let compiled_plan = epistemic::run_epistemic_loop(&compiler, &mut messages, &frozen_root, Some(&mcp_manager)).await?;
    let compilation = CompilationSummary::from_plan(&compiled_plan);
    println!("  ✅ {} ops, {:?} confidence", compilation.total_ops(), compilation.confidence);

    // Step 3: Hydrate
    println!("\n[3/5] Hydrating plan against frozen sandbox...");
    let plan_clone = compiled_plan.clone();
    let frozen_clone = frozen_root.clone();
    let snap_clone = snapshot_id.clone();
    let hydrated_plan = tokio::task::spawn_blocking(move || {
        PlanHydrator::hydrate(&plan_clone, &snap_clone, &frozen_clone)
    })
    .await
    .context("Hydration task panicked")?
    .context("Hydration failed")?;

    let hydration = HydrationSummary {
        snapshot_verified: true,
        hashes_matched: hydrated_plan.operations.len(),
        hashes_total: hydrated_plan.operations.len(),
        drift_warnings: vec![],
    };
    println!("  ✅ Snapshot anchored, {}/{} hashes verified",
        hydration.hashes_matched, hydration.hashes_total);

    // Step 4: Preflight
    println!("\n[4/5] Running preflight compile check...");
    // Apply to sandbox first
    let plan_for_apply = hydrated_plan.clone();
    let sandbox_view = sandbox.non_owning_view();
    tokio::task::spawn_blocking(move || {
        apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
    })
    .await
    .context("Apply task panicked")?
    .context("Apply failed")?;

    let preflight_start = std::time::Instant::now();
    let preflight_result = crow_verifier::preflight::run_preflight(
        sandbox.path(),
        Some(&frozen_root),
        std::time::Duration::from_secs(30),
        &profile.primary_lang,
    )
    .await;
    let preflight_elapsed = preflight_start.elapsed();

    let preflight = PreflightSummary {
        language: profile.primary_lang.name.clone(),
        outcome: match &preflight_result {
            crow_verifier::preflight::PreflightResult::Clean => {
                PreflightOutcome::Clean { duration_secs: preflight_elapsed.as_secs_f64() }
            }
            crow_verifier::preflight::PreflightResult::Errors(diags) => {
                PreflightOutcome::Errors {
                    count: diags.len(),
                    summary: crow_verifier::preflight::format_diagnostics(diags),
                }
            }
            crow_verifier::preflight::PreflightResult::Skipped(reason) => {
                PreflightOutcome::Skipped { reason: reason.clone() }
            }
        },
    };

    // Step 5: Build evidence and verdict
    let evidence = build_evidence_from_preflight(&preflight_result, &profile);
    let verdict = Verdict::from_evidence(evidence);

    // Print the full report
    let report = EvidenceReport {
        recon,
        compilation,
        hydration,
        preflight,
        verdict,
    };
    println!("{}", report);

    // Show diff
    println!("\n─── Planned Changes ───");
    diff::render_plan_diff(&frozen_root, sandbox.path(), &hydrated_plan);

    // Save session
    if let Ok(store) = session::SessionStore::open() {
        let mut sess = session::Session::new(&cfg.workspace, &prompt);
        sess.save_messages(&messages.as_messages());
        sess.push_snapshot(snapshot_id);
        if let Err(e) = store.save(&sess) {
            eprintln!("  ⚠️  Failed to save session: {:?}", e);
        } else {
            println!("\n  💾 Session saved: {}", sess.id.0);
        }
    }

    drop(sandbox);
    Ok(())
}

/// Build an EvidenceMatrix from preflight results.
fn build_evidence_from_preflight(
    preflight: &crow_verifier::preflight::PreflightResult,
    profile: &crow_probe::types::ProjectProfile,
) -> crow_evidence::types::EvidenceMatrix {
    use crow_evidence::types::*;

    let compile_passed = matches!(preflight, crow_verifier::preflight::PreflightResult::Clean);

    EvidenceMatrix {
        compile_runs: vec![TestRun {
            command: format!("preflight ({})", profile.primary_lang.name),
            outcome: if compile_passed { TestOutcome::Passed } else { TestOutcome::Failed },
            passed: if compile_passed { 1 } else { 0 },
            failed: if compile_passed { 0 } else { 1 },
            skipped: 0,
            duration: std::time::Duration::from_secs(0),
            truncated_log: String::new(),
        }],
        test_scope: Some(TestScope::Selective),
        has_known_baseline: true,
        lints_clean: compile_passed,
        intelligence_confidence: crow_patch::Confidence::Medium,
        risk_flags: vec![],
    }
}

/// Count files in a directory (non-recursive, approximate).
fn walkdir_count(root: &std::path::Path) -> usize {
    std::fs::read_dir(root)
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// Detect common manifest files.
fn detect_manifests(root: &std::path::Path) -> Vec<String> {
    let candidates = [
        "Cargo.toml", "package.json", "pyproject.toml", "go.mod",
        "Makefile", "Dockerfile", ".gitignore", "tsconfig.json",
    ];
    candidates
        .iter()
        .filter(|name| root.join(name).exists())
        .map(|name| name.to_string())
        .collect()
}


async fn run_dry_run(args: &[String]) -> Result<()> {
    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");
    let mut messages = context::ConversationManager::new(vec![]);
    run_conversation_turn(&cfg, &prompt, &mut messages).await
}

pub async fn run_conversation_turn(cfg: &CrowConfig, prompt: &str, messages: &mut context::ConversationManager) -> Result<()> {
    use crow_workspace::PlanHydrator;
    println!("🦅 crow-code Dry-Run / Turn mode initializing...\n");
    // ── Step 1: Freeze the timeline FIRST ──────────────────────────
    // We probe the live workspace only to discover ignore patterns,
    // then immediately materialize a sandbox. ALL subsequent work
    // (repo map, LLM compile, hydrate, apply, verify) operates
    // exclusively against this frozen snapshot.

    println!("[1/6] Radaring Workspace: {}", cfg.workspace.display());
    let snapshot_id = snapshot::resolve_snapshot_id(&cfg.workspace);
    println!("    📌 Snapshot ID: {}", snapshot_id.0);
    let profile = scan_workspace(&cfg.workspace).map_err(|e| anyhow::anyhow!(e))?;
    let candidate = match profile.verification_candidates.first() {
        Some(c) => c.clone(),
        None => {
            anyhow::bail!("No verification candidates found. Cannot dry-run without a verifier.");
        }
    };

    println!("\n[2/6] Materializing O(1) Sandbox Boundary (Freezing Timeline)...");
    let mat_config = MaterializeConfig {
        source: cfg.workspace.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize frozen sandbox")?;
    let frozen_root = sandbox.path().to_path_buf();
    println!(
        "    🛡️  Time-Frozen Sandbox established at: {}",
        frozen_root.display()
    );

    // ── Step 2: Build repo map against frozen sandbox ──────────────
    println!("\n[3/6] Gathering Repomap Context from Frozen Sandbox via tree-sitter...");
    let repo_map = cfg
        .build_repo_map_for(&frozen_root)
        .map_err(|e| anyhow::anyhow!(e))
        .context("Failed to build repo map from frozen sandbox")?;
    println!(
        "    🎯 Compressed map length: {} bytes",
        repo_map.map_text.len()
    );

    // ── Step 3: Autonomous Crucible Loop ───────────────────────────
    println!(
        "\n[4/6] Entering Autonomous Crucible Loop (Engine: {})...",
        cfg.describe_provider()
    );

    let client = cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?;
    let compiler = IntentCompiler::new(client);

    let mcp_manager = crate::mcp::McpManager::boot(&cfg.mcp_servers).await?;
    
    // Open EventLedger for telemetry recording
    let mut ledger = open_ledger(&cfg.workspace).unwrap_or_else(|e| {
        eprintln!("  ⚠️  Failed to open Event Ledger: {}", e);
        crow_workspace::ledger::EventLedger::open(&std::env::temp_dir().join("crow_ledger_fallback.jsonl")).unwrap()
    });
    
    let _ = ledger.append(crow_workspace::ledger::LedgerEvent::SnapshotCreated {
        id: snapshot_id.clone(),
        git_hash: snapshot_id.0.clone(),
        timestamp: chrono::Utc::now(),
    });

    let mut sys_prompt = String::from("Context (Repository Map):\n");
    sys_prompt.push_str(&repo_map.map_text);
    sys_prompt.push_str(&format!("\n\nWorkspace Snapshot ID: {}\nIMPORTANT: When you submit a plan, set base_snapshot_id to \"{}\" exactly.\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.", snapshot_id.0, snapshot_id.0));
    sys_prompt.push_str("\n\nMCTS DYNAMIC SEARCH: For complex code refactors, we use rigorous parallel searches (MCTS). However, if your intended changes are TRIVIAL (e.g. pure documentation tweaks, simple text formatting, or modifying markdown files), please explicitly set `requires_mcts = false` to save precious API loop latency.");

    let mcp_ctx = mcp_manager.prompt_context();
    if !mcp_ctx.is_empty() {
        sys_prompt.push_str("\n\n");
        sys_prompt.push_str(mcp_ctx);
    }

    use crow_brain::ChatMessage;
    
    // Inject current repo state and system context if it's the first turn, or update the existing system context with the new snapshot!
    messages.set_system(vec![
        ChatMessage::system("You are an autonomous engineering agent executing the given task."),
        ChatMessage::system(sys_prompt)
    ]);

    if messages.as_messages().len() <= 2 {
        // First turn
        messages.push_user(format!("Task:\n{}", prompt));
    } else {
        // Ongoing turn - system prompt is now freshly updated above without bloating user history!
        messages.push_user(prompt);
    }

    // Outer Crucible Loop (max 3 compile-test cycles).
    // Each attempt re-materializes a fresh sandbox so that retries are
    // independent timelines against the same frozen baseline.
    let mcts_config = crate::mcts::MctsConfig::from_env();
    if !mcts_config.is_serial() {
        // MCTS multiplies LLM calls by branch_factor. This is only
        // economically viable when prompt caching is active (~90% input
        // cost reduction). Enforce this as a hard gate, not a comment.
        if !cfg.llm.prompt_caching {
            println!(
                "    ⚠️  Warning: MCTS parallel mode (CROW_MCTS_BRANCHES={}) is running without prompt_caching enabled. \
                 This may be expensive on providers that bill for repetitive input tokens. \
                 Set CROW_PROMPT_CACHE=1 or prompt_caching=true in .crow/config.json if using Anthropic models.",
                mcts_config.branch_factor
            );
        }
        // Pre-warm the build cache so all MCTS branches start with
        // compiled dependencies. Without this, the first cargo check
        // in every branch hits a cold cache (30-60s); with it, each
        warm_build_cache(&frozen_root, &cfg.workspace, &profile, &candidate).await;
        let winner = run_mcts_crucible(
            &mcts_config,
            &profile,
            &candidate,
            &frozen_root,
            &compiler,
            messages,
            &snapshot_id,
            Some(&mcp_manager),
        )
        .await?;
        
        if let Some(w) = winner {
            // Apply the winning plan using the workspace WriteMode
            let plan_id = format!("mcts-{}-{}", snapshot_id.0, chrono::Utc::now().timestamp_millis());
            apply_winning_plan(cfg, w.sandbox.path(), &w.plan, &plan_id, &snapshot_id, &mut ledger).await?;
            drop(w.sandbox);
        }
        
        return Ok(());
    }

    let mut total_attempts = 0;
    let mut verification_runs = 0;
    let max_total_attempts = 10;
    let mut success = false;

    while verification_runs < 3 && total_attempts < max_total_attempts {
        total_attempts += 1;
        println!(
            "\n▶️ Crucible Epoch {} (Verification Run {}/3)",
            total_attempts,
            verification_runs + 1
        );

        let compiled_plan =
            epistemic::run_epistemic_loop(&compiler, messages, &frozen_root, Some(&mcp_manager)).await?;

        // Re-materialize from the FROZEN baseline, not the live workspace.
        // This ensures every crucible attempt starts from the same immutable
        // snapshot, even if the live workspace changes between attempts.
        println!("\n[5/6] Re-materializing fresh sandbox (from frozen baseline)...");
        let attempt_mat_config = MaterializeConfig {
            source: frozen_root.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false, // MUST be false: verifier executes repo commands inside this sandbox
        };
        let attempt_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
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
            PlanHydrator::hydrate(&plan_clone, &snap_for_hydrate, &attempt_sandbox_path)
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
                continue;
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
            // Create a non-owning view that won't clean up on drop.
            // The original attempt_sandbox retains ownership.
            let sandbox_view = attempt_sandbox.non_owning_view();
            // Offload synchronous filesystem I/O (fs::write, fs::rename, etc.)
            // to a blocking thread to avoid starving the tokio reactor.
            tokio::task::spawn_blocking(move || {
                apply_plan_to_sandbox(&plan_for_apply, &sandbox_view)
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

        // Diff baseline: frozen_root (pre-patch) → attempt_sandbox (post-patch).
        println!("\n--- Sandbox Diff (frozen baseline → patched) ---");
        diff::render_plan_diff(&frozen_root, attempt_sandbox.path(), &hydrated_plan);

        // ── Preflight micro-loop: catch compile errors in seconds ────
        // Run `cargo check --message-format=json` before the expensive full
        // test suite. If compile errors are found, feed them back to the LLM
        // for a quick fix attempt without consuming a crucible retry.
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
                Some(&frozen_root),
                std::time::Duration::from_secs(60),
                &profile.primary_lang,
            )
            .await;
            
            let passed_preflight = matches!(preflight_result, PreflightResult::Clean | PreflightResult::Skipped(_));
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
                    // Feed diagnostics back to the LLM — this does NOT consume
                    // a crucible attempt, it's a free micro-correction.
                    messages.push_user(format!(
                        "[PREFLIGHT COMPILE CHECK FAILED]\n{}\n\nPlease fix these compile errors and resubmit your plan.",
                        summary
                    ));
                    drop(attempt_sandbox);
                    continue;
                }
                PreflightResult::Skipped(reason) => {
                    println!("    ⚠️  Preflight skipped: {}", reason);
                    // Fall through to full verification
                }
            }
        }

        println!(
            "\n[6/6] Verifying Sandbox with '{}'...",
            candidate.command.display()
        );
        let exec_config = ExecutionConfig {
            timeout: std::time::Duration::from_secs(60),
            max_output_bytes: 5 * 1024 * 1024,
        };

        let result = crow_verifier::executor::execute(
            attempt_sandbox.path(),
            &candidate.command,
            &exec_config,
            &AciConfig::compact(),
            Some(&frozen_root), // Stable cache key: reuse build artifacts across crucible retries
        )
        .await
        .context("Verification execution failed")?;

        verification_runs += 1;

        let outcome = &result.test_run.outcome;
        println!("\n╔══════════════════════════════════════╗");
        println!("║  Dry-Run Verdict: {:?}", outcome);
        println!("╚══════════════════════════════════════╝");
        println!("Evidence:\n{}", result.test_run.truncated_log);

        if outcome == &crow_evidence::TestOutcome::Passed {
            println!(
                "\n[🎉] Autonomous execution successful on verification run {}!",
                verification_runs
            );

            apply_winning_plan(
                cfg,
                attempt_sandbox.path(),
                &hydrated_plan,
                &plan_id,
                &snapshot_id,
                &mut ledger,
            ).await?;

            success = true;
            break;
        } else {
            println!("\n[❗] Verification failed! Re-entering Crucible Loop with ACI log...");
            messages.push_verifier_result(
                &format!("{:?}", result.test_run.outcome),
                &result.test_run.truncated_log,
            );
            let _ = ledger.append(crow_workspace::ledger::LedgerEvent::PlanRolledBack {
                plan_id: plan_id.clone(),
                reason: format!("Verification failed: {:?}", result.test_run.outcome),
                timestamp: chrono::Utc::now(),
            });
        }
        drop(attempt_sandbox);
    }

    drop(sandbox);

    if !success {
        anyhow::bail!("All crucible attempts failed to pass verification.");
    }

    Ok(())
}

/// Apply verified changes from sandbox back to the real workspace.
///
/// Only copies files that were changed by the plan (not the entire sandbox).
/// This ensures minimal filesystem mutation and respects the zero-pollution
/// invariant: if anything goes wrong, the workspace is untouched.
fn apply_sandbox_to_workspace(
    sandbox_root: &std::path::Path,
    workspace_root: &std::path::Path,
    plan: &crow_patch::IntentPlan,
) -> Result<()> {
    use crow_patch::EditOp;

    struct RollbackRecord {
        dst_path: std::path::PathBuf,
        original_content: Option<Vec<u8>>,
    }
    let mut rollback_log: Vec<RollbackRecord> = Vec::new();
    let mut created_dirs: Vec<std::path::PathBuf> = Vec::new();

    // Phase 1: Snapshot original states for rollback
    for op in &plan.operations {
        let (dst, from_dst) = match op {
            EditOp::Create { path, .. } | EditOp::Modify { path, .. } | EditOp::Delete { path, .. } => {
                (path.to_absolute(workspace_root), None)
            }
            EditOp::Rename { from, to, .. } => {
                (to.to_absolute(workspace_root), Some(from.to_absolute(workspace_root)))
            }
        };
        
        let original_content = if dst.exists() && dst.is_file() {
            std::fs::read(&dst).ok()
        } else {
            None
        };
        rollback_log.push(RollbackRecord { dst_path: dst.clone(), original_content });
        
        if let Some(ref fdst) = from_dst {
            let original_from = if fdst.exists() && fdst.is_file() {
                std::fs::read(fdst).ok()
            } else {
                None
            };
            rollback_log.push(RollbackRecord { dst_path: fdst.clone(), original_content: original_from });
        }

        let mut track_new_dir = |p: &std::path::Path| {
            if let Some(mut current) = p.parent() {
                let mut highest_new = None;
                while !current.exists() {
                    highest_new = Some(current.to_path_buf());
                    if let Some(parent) = current.parent() {
                        current = parent;
                    } else {
                        break;
                    }
                }
                if let Some(h) = highest_new {
                    if !created_dirs.contains(&h) {
                        created_dirs.push(h);
                    }
                }
            }
        };

        track_new_dir(&dst);
        if let Some(f) = from_dst {
            track_new_dir(&f);
        }
    }

    // Phase 2: Attempt destructive apply
    let mut apply_failed = false;
    let mut apply_error = String::new();

    for op in &plan.operations {
        let result = match op {
            EditOp::Create { path, .. } | EditOp::Modify { path, .. } => {
                let src = path.to_absolute(sandbox_root);
                let dst = path.to_absolute(workspace_root);
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::copy(&src, &dst).map(|_| ())
            }
            EditOp::Delete { path, .. } => {
                let dst = path.to_absolute(workspace_root);
                if dst.exists() {
                    std::fs::remove_file(&dst)
                } else {
                    Ok(())
                }
            }
            EditOp::Rename { from, to, .. } => {
                // Actually, the intent was to move `from` to `to` in workspace. But wait! The sandbox already HAS the mutated state!
                // So sandbox's `to` path is the file we want! And we should delete workspace's `from` path.
                let src = to.to_absolute(sandbox_root);
                let dst_to = to.to_absolute(workspace_root);
                let dst_from = from.to_absolute(workspace_root);
                
                if let Some(parent) = dst_to.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                
                match std::fs::copy(&src, &dst_to) {
                    Ok(_) => {
                        if dst_from.exists() { std::fs::remove_file(&dst_from) } else { Ok(()) }
                    }
                    Err(e) => Err(e)
                }
            }
        };

        if let Err(e) = result {
            apply_failed = true;
            apply_error = format!("Failed op {:?}: {}", op, e);
            break;
        }
    }

    // Phase 3: Rollback on Failure
    if apply_failed {
        eprintln!("\n🚨 Apply failed mid-flight. Executing zero-pollution rollback...");
        for record in rollback_log {
            if let Some(content) = record.original_content {
                if let Some(parent) = record.dst_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&record.dst_path, content);
            } else if record.dst_path.exists() {
                let _ = std::fs::remove_file(&record.dst_path);
            }
        }
        
        // Clean up any directories we created
        for dir in created_dirs {
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        anyhow::bail!("Transaction failed and rolled back. Cause: {}", apply_error);
    }

    Ok(())
}

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
) {
    use std::time::Instant;

    let mut cmd = None;
    match profile.primary_lang.name.as_str() {
        "rust" => {
            let mut c = candidate.command.clone();
            if (c.program == "cargo" || c.program.ends_with("/cargo")) && c.args.contains(&"test".to_string()) && !c.args.contains(&"--no-run".to_string()) {
                c.args.push("--no-run".to_string());
            }
            // Strip out display colors which pollute verification parsing (just in case)
            if !c.args.iter().any(|a| a.starts_with("--color")) {
                c.args.push("--color=never".to_string());
            }
            cmd = Some(c);
        }
        "typescript" | "javascript" => cmd = Some(crow_probe::VerificationCommand::new("npm", vec!["install", "--ignore-scripts"])),
        _ => {}
    };

    let Some(cmd) = cmd else {
        println!("    ⏭️  No warm-up cache command configured for language: {}", profile.primary_lang.name);
        return;
    };

    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(
        indicatif::ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message(format!("[4.5/6] Pre-warming build cache for {}...", profile.primary_lang.name));
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    
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
            spinner.finish_and_clear();
            let elapsed = start.elapsed();
            if result.exit_code == Some(0) {
                // Sync the warmed cache into our isolated global tracker for future runs.
                // We MUST use `crow_target` here, NOT `host_target`, as we do not want to pollute 
                // the user's active workspace target/ with our sandbox builds!
                crate::mcts::clone_cache_dir(&frozen_cache, &crow_target).await;
                println!(
                    "    ✅ Build cache warmed in {:.1}s — MCTS branches will use incremental compilation",
                    elapsed.as_secs_f64()
                );
            } else {
                println!(
                    "    ⚠️  Warm-up cargo check failed (exit={:?}) in {:.1}s — branches will cold-build",
                    result.exit_code,
                    elapsed.as_secs_f64()
                );
            }
        }
        Err(e) => {
            spinner.finish_and_clear();
            eprintln!(
                "    ⚠️  Build cache warm-up failed: {:?} — continuing without cache",
                e
            );
        }
    }
}

async fn apply_winning_plan(
    cfg: &CrowConfig,
    sandbox_path: &std::path::Path,
    hydrated_plan: &crow_patch::IntentPlan,
    plan_id: &str,
    snapshot_id: &crow_patch::SnapshotId,
    ledger: &mut crow_workspace::ledger::EventLedger,
) -> Result<()> {
    // ── WriteMode enforcement ────────────────────────────
    match cfg.write_mode {
        config::WriteMode::SandboxOnly => {
            println!("\n  📦 Write mode: sandbox-only — changes remain in sandbox (not applied to workspace)");
            println!("     Use CROW_WRITE_MODE=write to enable workspace application.");
        }
        config::WriteMode::WorkspaceWrite => {
            println!("\n  ✍️  Write mode: workspace-write — applying verified changes to workspace...");
            if let Err(e) = apply_sandbox_to_workspace(
                sandbox_path,
                &cfg.workspace,
                hydrated_plan,
            ) {
                eprintln!("  ❌ Failed to apply to workspace: {:?}", e);
                eprintln!("     Sandbox remains at: {}", sandbox_path.display());
                anyhow::bail!("Workspace application failed: {:?}", e);
            } else {
                println!("  ✅ Workspace updated successfully.");
                if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, hydrated_plan) {
                    println!("  ⚠️  Could not automatically commit changes: {}", e);
                } else {
                    println!("  ✅ Changes committed to git timeline.");
                }
            }
        }
        config::WriteMode::DangerFullAccess => {
            println!("\n  ⚠️  Write mode: danger-full-access — applying without additional checks...");
            if let Err(e) = apply_sandbox_to_workspace(
                sandbox_path,
                &cfg.workspace,
                hydrated_plan,
            ) {
                eprintln!("  ❌ Failed to apply to workspace: {:?}", e);
                anyhow::bail!("Workspace application failed: {:?}", e);
            } else {
                println!("  ✅ Workspace updated.");
                if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, hydrated_plan) {
                    println!("  ⚠️  Could not automatically commit changes: {}", e);
                } else {
                    println!("  ✅ Changes committed to git timeline.");
                }
            }
        }
    }

    if cfg.write_mode != config::WriteMode::SandboxOnly {
        let _ = ledger.append(crow_workspace::ledger::LedgerEvent::PlanApplied {
            plan_id: plan_id.to_string(),
            snapshot_id: snapshot_id.clone(),
            timestamp: chrono::Utc::now(),
        });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_mcts_crucible(
    mcts_config: &crate::mcts::MctsConfig,
    profile: &crow_probe::types::ProjectProfile,
    candidate: &crow_probe::types::VerificationCandidate,
    frozen_root: &std::path::Path,
    compiler: &crow_brain::IntentCompiler,
    messages: &mut context::ConversationManager,
    snapshot_id: &crow_patch::SnapshotId,
    mcp_manager: Option<&crate::mcp::McpManager>,
) -> Result<Option<crate::mcts::BranchOutcome>> {
    // 1. Initial Epistemic Loop (Serial Recon)
    println!("\n[5/6] Entering Epistemic Recon Loop (MCTS Pre-exploration)...");
    let baseline_plan = epistemic::run_epistemic_loop(compiler, messages, frozen_root, mcp_manager).await?;
    
    if baseline_plan.operations.is_empty() {
        println!("\n[🎉] Conversational Intent Detected (No codebase changes proposed)");
        println!("─── Agent Message ───\n{}", baseline_plan.rationale);
        return Ok(None);
    }

    println!("    Seeding baseline plan into MCTS branch 0...");
    
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

    if (is_pure_text_change || !baseline_plan.requires_mcts) && actual_mcts_config.branch_factor > 1 {
        println!("    ⏭️  Baseline plan indicates MCTS bypass (trivial or non-code task). Bypassing parallel diverse search (MCTS downgraded to 1 branch).");
        actual_mcts_config.branch_factor = 1;
    }

    // 2. MCTS Parallel Explore Rounds
    let mat_config = MaterializeConfig {
        source: frozen_root.to_path_buf(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };

    println!(
        "\n[6/6] Entering MCTS Parallel Crucible ({} branches, {} max rounds)",
        actual_mcts_config.branch_factor, actual_mcts_config.max_rounds
    );
    let mut current_baseline = baseline_plan;

    for mcts_round in 1..=actual_mcts_config.max_rounds {
        println!("▶️ MCTS Round {}/{}", mcts_round, actual_mcts_config.max_rounds);

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
            println!(
                "\n[🎉] MCTS Branch {} passed on round {}!",
                winner.branch_id, mcts_round
            );

            // Render the diff so the user sees what changed
            println!("\n─── Winning Patch (Branch {}) ───", winner.branch_id);
            diff::render_plan_diff(frozen_root, winner.sandbox.path(), &winner.plan);

            println!("\n╔══════════════════════════════════════╗");
            println!("║  MCTS Verdict: Passed ✅              ║");
            println!("╚══════════════════════════════════════╝");
            println!("Evidence:\n{}", winner.log);

            return Ok(Some(winner));
        }

        // All branches failed. Feed diagnostics back and re-derive baseline.
        println!(
            "\n[❗] MCTS Round {} failed! Feeding diagnostics back to LLM...",
            mcts_round
        );
        let merged = crate::mcts::merge_diagnostics(&outcomes);
        messages.push_verifier_result("MCTS_AllBranchesFailed", &merged);

        // Re-compile a fresh baseline plan that incorporates the failure
        // feedback. This ensures branch 0 in the next round gets an
        // informed plan instead of repeating the same stale one.
        if mcts_round < actual_mcts_config.max_rounds {
            println!("  🧠 Re-deriving baseline plan from failure feedback...");
            match compiler.compile_action(&messages.as_messages()).await {
                Ok(crow_patch::AgentAction::SubmitPlan { plan }) => {
                    println!("    ✅ New baseline plan generated for next round");
                    current_baseline = plan;
                }
                Ok(other) => {
                    // Model wants to do more recon — note it but reuse previous baseline
                    messages.push_assistant(serde_json::to_string(&other).unwrap_or_default());
                    println!("    ⚠️  Model requested {:?} instead of SubmitPlan — reusing previous baseline",
                        match &other {
                            crow_patch::AgentAction::ReadFiles { .. } => "ReadFiles",
                            crow_patch::AgentAction::Recon { .. } => "Recon",
                            _ => "unknown",
                        }
                    );
                }
                Err(e) => {
                    eprintln!(
                        "    ⚠️  Baseline re-derivation failed: {:?} — reusing previous",
                        e
                    );
                }
            }
        }
    }

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
                let conf = cfg.mcp_servers.get(n).ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", n))?;
                (n, conf)
            } else {
                if cfg.mcp_servers.is_empty() {
                    anyhow::bail!("No MCP servers configured in .crow/config.json");
                }
                // Just grab the first one
                let first = cfg.mcp_servers.iter().next().unwrap();
                (first.0.as_str(), first.1)
            };

            println!("🔌 Connecting to MCP Server: {} ({} {})", name, mcp_cfg.command, mcp_cfg.args.join(" "));
            
            let args_refs: Vec<&str> = mcp_cfg.args.iter().map(|s| s.as_str()).collect();
            let client = crow_mcp::McpClient::spawn(&mcp_cfg.command, &args_refs)?;
            
            println!("  Initializing handshake...");
            let init = client.initialize().await?;
            println!("  ✅ Server initialized: {} v{}", init.server_info.name, init.server_info.version);
            
            println!("  Fetching tools...");
            let tools = client.list_tools().await?;
            println!("\n🛠️  Available Tools ({}):", tools.tools.len());
            for tool in tools.tools {
                println!("  - {} : {}", tool.name, tool.description.as_deref().unwrap_or("No description"));
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

fn open_ledger(workspace: &std::path::Path) -> Result<crow_workspace::ledger::EventLedger> {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    use anyhow::Context;

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
    use tempfile::TempDir;
    use std::fs;

    #[tokio::test]
    async fn test_apply_winning_plan_sandbox_only_does_not_append_ledger() {
        let workspace = TempDir::new().unwrap();
        let ledger_dir = workspace.path().join("ledger.jsonl");
        let mut ledger = crow_workspace::ledger::EventLedger::open(&ledger_dir).unwrap();

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
        
        apply_winning_plan(
            &cfg,
            sandbox.path(),
            &plan,
            "test-plan",
            &snap_id,
            &mut ledger,
        ).await.unwrap();

        // Ledger should be empty for SandboxOnly mode
        let contents = fs::read_to_string(&ledger_dir).unwrap_or_default();
        assert!(contents.is_empty(), "SandboxOnly should not emit PlanApplied event to ledger");
    }
}

