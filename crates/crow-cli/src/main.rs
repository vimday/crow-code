mod budget;
mod config;
mod context;
mod diff;
mod epistemic;
mod legacy_god;
mod mcts;

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
    if args.len() >= 2 {
        if args[1] == "compile" {
            return run_compile_only(&args[2..]).await;
        } else if args[1] == "dry-run" {
            return run_dry_run(&args[2..]).await;
        } else if args[1] == "legacy-god" {
            return legacy_god::run_god_pipeline().await;
        }
    }

    println!("Welcome to crow-code. Please provide a command (dry-run, compile, legacy-god).");
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
        "\n[2/3] Compiling IntentPlan via crow-brain (Model: {})...",
        cfg.llm.model
    );

    let client = std::sync::Arc::new(cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?);
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

// ─── Dry-Run command ────────────────────────────────────────────────

async fn run_dry_run(args: &[String]) -> Result<()> {
    use crow_workspace::PlanHydrator;

    println!("🦅 crow-code Dry-Run mode initializing...\n");

    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");

    // ── Step 1: Freeze the timeline FIRST ──────────────────────────
    // We probe the live workspace only to discover ignore patterns,
    // then immediately materialize a sandbox. ALL subsequent work
    // (repo map, LLM compile, hydrate, apply, verify) operates
    // exclusively against this frozen snapshot.

    println!("[1/6] Radaring Workspace: {}", cfg.workspace.display());
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
        .unwrap()
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
        "\n[4/6] Entering Autonomous Crucible Loop (Model: {})...",
        cfg.llm.model
    );

    let client = std::sync::Arc::new(cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?);
    let compiler = IntentCompiler::new(client);

    // Structured message history with proper role separation.
    // System context (repo map + constraints) is set once; subsequent
    // interactions are User (system feedback) and Assistant (LLM output).
    use context::ConversationManager;
    use crow_brain::ChatMessage;

    let mut messages = ConversationManager::new(vec![
        ChatMessage::system("You are an autonomous engineering agent executing the given task."),
        ChatMessage::system(format!(
            "Context (Repository Map):\n{}\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.",
            repo_map.map_text
        )),
    ]);

    messages.push_user(format!("Task:\n{}", prompt));

    // Outer Crucible Loop (max 3 compile-test cycles).
    // Each attempt re-materializes a fresh sandbox so that retries are
    // independent timelines against the same frozen baseline.
    let mcts_config = crate::mcts::MctsConfig::from_env();
    if !mcts_config.is_serial() {
        // MCTS multiplies LLM calls by branch_factor. This is only
        // economically viable when prompt caching is active (~90% input
        // cost reduction). Enforce this as a hard gate, not a comment.
        if !cfg.llm.prompt_caching {
            anyhow::bail!(
                "MCTS parallel mode (CROW_MCTS_BRANCHES={}) requires prompt caching. \
                 Set CROW_PROMPT_CACHE=1 or prompt_caching=true in .crow/config.json.",
                mcts_config.branch_factor
            );
        }
        // Pre-warm the build cache so all MCTS branches start with
        // compiled dependencies. Without this, the first cargo check
        // in every branch hits a cold cache (30-60s); with it, each
        // branch only incrementally recompiles its patched crate (~5s).
        warm_build_cache(&frozen_root, &candidate.command).await;
        return run_mcts_crucible(
            &mcts_config,
            &profile,
            &candidate,
            &frozen_root,
            &compiler,
            &mut messages,
        )
        .await;
    }

    for crucible_attempt in 1..=3 {
        println!("\n▶️ Crucible Attempt {}/3", crucible_attempt);

        let compiled_plan =
            epistemic::run_epistemic_loop(&compiler, &mut messages, &frozen_root).await?;

        // Re-materialize from the FROZEN baseline, not the live workspace.
        // This ensures every crucible attempt starts from the same immutable
        // snapshot, even if the live workspace changes between attempts.
        println!(
            "\n[5/6] Re-materializing fresh sandbox for attempt {} (from frozen baseline)...",
            crucible_attempt
        );
        let attempt_mat_config = MaterializeConfig {
            source: frozen_root.clone(),
            artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
            skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
            allow_hardlinks: false, // MUST be false: verifier executes repo commands inside this sandbox
        };
        let attempt_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
            .await
            .unwrap()
            .context("Failed to re-materialize attempt sandbox")?;
        println!(
            "    🛡️  Fresh attempt sandbox at: {}",
            attempt_sandbox.path().display()
        );

        let attempt_sandbox_path = attempt_sandbox.path().to_path_buf();
        let plan_clone = compiled_plan.clone();
        let hydrated_plan = match tokio::task::spawn_blocking(move || {
            PlanHydrator::hydrate(&plan_clone, &attempt_sandbox_path)
        })
        .await
        .unwrap()
        {
            Ok(p) => p,
            Err(e) => {
                println!("    ❌ Hydration failed: {:?}", e);
                messages.push_user(format!(
                        "[HYDRATION FAILED]\nYour plan failed physical hydration: {:?}\n\nPlease reflect and output a new AgentAction to fix the issue.",
                        e
                    ));
                continue;
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
            .unwrap()
            .context("Failed to apply plan to sandbox")?;
        }
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
            let preflight_result = preflight::cargo_check_preflight(
                attempt_sandbox.path(),
                Some(&frozen_root),
                std::time::Duration::from_secs(30),
            )
            .await;

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

        let outcome = &result.test_run.outcome;
        println!("\n╔══════════════════════════════════════╗");
        println!("║  Dry-Run Verdict: {:?}", outcome);
        println!("╚══════════════════════════════════════╝");
        println!("Evidence:\n{}", result.test_run.truncated_log);

        if result.test_run.outcome == crow_evidence::TestOutcome::Passed {
            println!(
                "\n[🎉] Autonomous execution successful on attempt {}!",
                crucible_attempt
            );
            break;
        } else {
            println!("\n[❗] Verification failed! Re-entering Crucible Loop with ACI log...");
            messages.push_verifier_result(
                &format!("{:?}", result.test_run.outcome),
                &result.test_run.truncated_log,
            );
        }
        drop(attempt_sandbox);
    }

    drop(sandbox);
    Ok(())
}

/// Pre-warm the Cargo build cache by running `cargo check` on the frozen
/// sandbox. This populates the `CARGO_TARGET_DIR` (keyed to `frozen_root`)
/// with all dependency artifacts so MCTS branches only need incremental
/// recompilation of the patched crate(s).
///
/// Failure is non-fatal: if the warm-up fails (e.g. the project doesn't
/// compile in its current state), branches will simply cold-build.
async fn warm_build_cache(
    frozen_root: &std::path::Path,
    _verify_command: &crow_probe::VerificationCommand,
) {
    use std::time::Instant;

    println!("\n[4.5/6] Pre-warming build cache for MCTS...");
    let start = Instant::now();

    let cmd = crow_probe::VerificationCommand::new("cargo", vec!["check", "--color=never"]);

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
        Some(frozen_root), // stable cache key
    )
    .await
    {
        Ok(result) => {
            let elapsed = start.elapsed();
            if result.exit_code == Some(0) {
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
            eprintln!(
                "    ⚠️  Build cache warm-up failed: {:?} — continuing without cache",
                e
            );
        }
    }
}

async fn run_mcts_crucible(
    mcts_config: &crate::mcts::MctsConfig,
    profile: &crow_probe::types::ProjectProfile,
    candidate: &crow_probe::types::VerificationCandidate,
    frozen_root: &std::path::Path,
    compiler: &IntentCompiler,
    messages: &mut context::ConversationManager,
) -> Result<()> {
    // 1. Initial Epistemic Loop (Serial Recon)
    println!("\n[5/6] Entering Epistemic Recon Loop (MCTS Pre-exploration)...");
    let baseline_plan =
        epistemic::run_epistemic_loop(compiler, messages, frozen_root).await?;
    println!("    Seeding baseline plan into MCTS branch 0...");

    // 2. MCTS Parallel Explore Rounds
    let mat_config = MaterializeConfig {
        source: frozen_root.to_path_buf(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };

    println!(
        "\n[6/6] Entering MCTS Parallel Crucible ({} branches, {} max rounds)",
        mcts_config.branch_factor, mcts_config.max_rounds
    );
    let mut current_baseline = baseline_plan;

    for mcts_round in 1..=mcts_config.max_rounds {
        println!("▶️ MCTS Round {}/{}", mcts_round, mcts_config.max_rounds);

        let mut outcomes = crate::mcts::explore_round(
            mcts_config,
            compiler,
            &messages.as_messages(),
            current_baseline.clone(),
            frozen_root,
            &mat_config,
            &candidate.command,
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

            drop(winner.sandbox);
            return Ok(());
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
        if mcts_round < mcts_config.max_rounds {
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

    println!(
        "\n[❌] Outputting final failure after {} MCTS rounds.",
        mcts_config.max_rounds
    );
    Ok(())
}
