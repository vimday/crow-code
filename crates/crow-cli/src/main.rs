mod config;
mod diff;

use config::CliConfig;
use crow_brain::IntentCompiler;
use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::{Confidence, EditOp, FilePrecondition, IntentPlan, WorkspacePath};
use crow_probe::scan_workspace;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() >= 2 {
        if args[1] == "compile" {
            return run_compile_only(&args[2..]).await;
        } else if args[1] == "dry-run" {
            return run_dry_run(&args[2..]).await;
        }
    }

    run_god_pipeline().await
}

// ─── Sprint 1: God Pipeline (synthetic self-test) ───────────────────

async fn run_god_pipeline() -> Result<(), Box<dyn std::error::Error>> {
    println!("🦅 crow-code Sprint 1 God Pipeline initializing...\n");

    let current_dir = env::current_dir()?;
    println!("[1/4] Radaring Workspace: {}", current_dir.display());

    let profile = scan_workspace(&current_dir)?;
    println!(
        "    🎯 Primary Lang: {} (Tier {:?})",
        profile.primary_lang.name, profile.primary_lang.tier
    );

    let candidate = match profile.verification_candidates.first() {
        Some(c) => c,
        None => {
            println!("    ⚠️  No verification candidates found for this workspace.");
            println!("    Outcome: NoVerifierAvailable");
            println!("\n[✓] Probe complete. No verification surface discovered.");
            return Ok(());
        }
    };
    println!(
        "    ⚔️  Target Candidate: {} [confidence: {:?}]",
        candidate.command.display(),
        candidate.confidence
    );

    println!("\n[2/4] Materializing O(1) Sandbox Boundary...");
    let config = MaterializeConfig {
        source: current_dir.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };

    let sandbox = materialize(&config).map_err(|e| format!("Materialization failed: {}", e))?;
    println!(
        "    🛡️  Sandbox established at: {}",
        sandbox.path().display()
    );
    println!("    🏎️  Driver active: {:?}", sandbox.driver());

    println!("\n[3/4] Synthesizing and Applying 'IntentPlan' (Unlink-on-Write enabled)");
    let mock_plan = IntentPlan {
        base_snapshot_id: crow_patch::SnapshotId("snapshot-001".into()),
        rationale: "Pipeline synthetic verification inject".into(),
        is_partial: false,
        confidence: Confidence::High,
        operations: vec![EditOp::Create {
            path: WorkspacePath::new("dummy_test_crow.txt")?,
            content: "This is a synthetic artifact created by God Pipeline.\n".into(),
            precondition: FilePrecondition::MustNotExist,
        }],
    };

    apply_plan_to_sandbox(&mock_plan, &sandbox)?;
    println!("    💉 Inject successful!");

    println!("\n[4/4] Engaging Verifier execution inside sandbox...");
    let exec_config = ExecutionConfig {
        timeout: std::time::Duration::from_secs(60),
        max_output_bytes: 5 * 1024 * 1024,
    };

    let result = crow_verifier::executor::execute(
        sandbox.path(),
        &candidate.command,
        &exec_config,
        &AciConfig::compact(),
    )?;

    println!("\n[✓] Verification Cycle Completed");
    println!("--- Result Summary ---");
    println!("Outcome: {:?}", result.test_run.outcome);
    println!(
        "Truncated Evidence Dump:\n{}",
        result.test_run.truncated_log
    );

    drop(sandbox);
    println!("\n[✓] Sprint 0/1 End-to-End sequence complete. Ready for LLM integration.");

    Ok(())
}

// ─── Compile-Only command ───────────────────────────────────────────

async fn run_compile_only(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    println!("🦅 crow-code Compile-Only mode initializing...\n");

    let cfg = CliConfig::from_env()?;
    let prompt = args.join(" ");

    println!("[1/3] Gathering Repomap Context via tree-sitter...");
    let repo_map = cfg.build_repo_map()?;
    println!(
        "    🎯 Compressed map length: {} bytes",
        repo_map.map_text.len()
    );

    println!(
        "\n[2/3] Compiling IntentPlan via crow-brain (Model: {})...",
        cfg.model
    );
    let full_prompt = format!("Context:\n{}\n\nTask:\n{}", repo_map.map_text, prompt);

    let client = Box::new(cfg.build_llm_client()?);
    let compiler = IntentCompiler::new(client);

    match compiler.compile_action(&full_prompt).await {
        Ok(action) => {
            println!("\n[✓] Compilation Successful!");
            println!("--- Parsed AgentAction ---");
            println!("{}", serde_json::to_string_pretty(&action)?);
            Ok(())
        }
        Err(e) => {
            eprintln!("\n[✗] Compilation Failed: {:?}", e);
            Err("Failed to compile AgentAction".into())
        }
    }
}

// ─── Dry-Run command ────────────────────────────────────────────────

async fn run_dry_run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use crow_workspace::PlanHydrator;

    println!("🦅 crow-code Dry-Run mode initializing...\n");

    let cfg = CliConfig::from_env()?;
    let prompt = args.join(" ");

    // ── Step 1: Freeze the timeline FIRST ──────────────────────────
    // We probe the live workspace only to discover ignore patterns,
    // then immediately materialize a sandbox. ALL subsequent work
    // (repo map, LLM compile, hydrate, apply, verify) operates
    // exclusively against this frozen snapshot.

    println!("[1/6] Radaring Workspace: {}", cfg.workspace.display());
    let profile = scan_workspace(&cfg.workspace)?;
    let candidate = match profile.verification_candidates.first() {
        Some(c) => c.clone(),
        None => {
            return Err(
                "No verification candidates found. Cannot dry-run without a verifier.".into(),
            )
        }
    };

    println!("\n[2/6] Materializing O(1) Sandbox Boundary (Freezing Timeline)...");
    let mat_config = MaterializeConfig {
        source: cfg.workspace.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let sandbox = materialize(&mat_config)?;
    let frozen_root = sandbox.path().to_path_buf();
    println!(
        "    🛡️  Time-Frozen Sandbox established at: {}",
        frozen_root.display()
    );

    // ── Step 2: Build repo map against frozen sandbox ──────────────
    println!("\n[3/6] Gathering Repomap Context from Frozen Sandbox via tree-sitter...");
    let repo_map = cfg.build_repo_map_for(&frozen_root)?;
    println!(
        "    🎯 Compressed map length: {} bytes",
        repo_map.map_text.len()
    );

    // ── Step 3: Autonomous Crucible Loop ───────────────────────────
    println!(
        "\n[4/6] Entering Autonomous Crucible Loop (Model: {})...",
        cfg.model
    );

    let client = Box::new(cfg.build_llm_client()?);
    let compiler = IntentCompiler::new(client);

    let mut messages_context = format!(
        "Context:\n{}\n\nTask:\n{}\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.",
        repo_map.map_text, prompt
    );

    // Outer Crucible Loop (max 3 compile-test cycles)
    for crucible_attempt in 1..=3 {
        println!("\n▶️ Crucible Attempt {}/3", crucible_attempt);

        // Inner Epistemic Loop (bounded)
        const MAX_EPISTEMIC_STEPS: usize = 7;
        const MAX_FILE_BYTES: u64 = 50 * 1024; // 50 KB
        const MAX_FILE_LINES: usize = 500;
        let mut epistemic_step = 0;

        let compiled_plan = loop {
            epistemic_step += 1;
            if epistemic_step > MAX_EPISTEMIC_STEPS {
                return Err(format!(
                    "Epistemic loop exceeded {} steps without producing a SubmitPlan. Aborting.",
                    MAX_EPISTEMIC_STEPS
                )
                .into());
            }

            println!(
                "  🧠 Epistemic Step {}/{} — Modulating Cognitive Request...",
                epistemic_step, MAX_EPISTEMIC_STEPS
            );
            let action = compiler
                .compile_action(&messages_context)
                .await
                .map_err(|e| format!("Compilation failed: {:?}", e))?;

            match action {
                crow_patch::AgentAction::ReadFiles { paths, rationale } => {
                    println!("    📖 Agent requests to read files: {:?}", paths);
                    println!("       Rationale: {}", rationale);

                    messages_context.push_str("\n\n[SYSTEM: READ FILES RESULT]\n");
                    for path in paths {
                        // Read from FROZEN sandbox, not live workspace
                        let abs_path = path.to_absolute(&frozen_root);

                        // Unified streaming read via BufReader.
                        // The line limit (MAX_FILE_LINES) is an independent hard
                        // gate that applies to ALL files regardless of byte size.
                        // The byte threshold only controls the truncation warning.
                        use std::io::{BufRead, BufReader};
                        let file_size = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);

                        let content = match std::fs::File::open(&abs_path) {
                            Ok(file) => {
                                let reader = BufReader::new(file);
                                let lines: Vec<String> = reader
                                    .lines()
                                    .map_while(Result::ok)
                                    .take(MAX_FILE_LINES)
                                    .collect();
                                let was_truncated =
                                    file_size > MAX_FILE_BYTES || lines.len() >= MAX_FILE_LINES;
                                let text = lines.join("\n");
                                if was_truncated {
                                    format!(
                                        "{}\n\n[SYSTEM WARNING: File truncated. Original size: {} bytes, showing first {} lines only.]",
                                        text, file_size, lines.len()
                                    )
                                } else {
                                    text
                                }
                            }
                            Err(_) => "<file not found or unreadable>".into(),
                        };

                        messages_context.push_str(&format!(
                            "--- {} ---\n{}\n\n",
                            path.as_str(),
                            content
                        ));
                    }
                    messages_context.push_str(
                        "Please proceed with your task, or read more files if necessary.",
                    );
                }
                crow_patch::AgentAction::SubmitPlan { plan } => {
                    println!("    ✅ Agent submitted IntentPlan!");
                    break plan;
                }
            }
        };

        // Re-materialize a fresh sandbox for each crucible attempt so that
        // each retry operates on a clean copy of the frozen baseline, not a
        // previously-polluted sandbox. This gives "independent timeline" semantics.
        println!(
            "\n[5/6] Re-materializing fresh sandbox for attempt {}...",
            crucible_attempt
        );
        let attempt_sandbox = materialize(&mat_config)?;
        println!(
            "    🛡️  Fresh attempt sandbox at: {}",
            attempt_sandbox.path().display()
        );

        let hydrated_plan = match PlanHydrator::hydrate(&compiled_plan, attempt_sandbox.path()) {
            Ok(p) => p,
            Err(e) => {
                println!("    ❌ Hydration failed: {:?}", e);
                messages_context.push_str(&format!(
                    "\n\n[SYSTEM: HYDRATION FAILED]\nYour plan failed physical hydration: {:?}\n\nPlease reflect and output a new AgentAction to fix the issue.",
                    e
                ));
                continue;
            }
        };

        println!(
            "    💧 Hydrated Plan:\n{}",
            serde_json::to_string_pretty(&hydrated_plan)?
        );

        apply_plan_to_sandbox(&hydrated_plan, &attempt_sandbox)?;
        println!("    💉 Sandbox injection successful!");

        // Diff baseline: frozen_root (pre-patch) → attempt_sandbox (post-patch).
        // Both sides from materialized snapshots, never the live workspace.
        println!("\n--- Sandbox Diff (frozen baseline → patched) ---");
        diff::render_plan_diff(&frozen_root, attempt_sandbox.path(), &hydrated_plan);

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
        )?;

        let outcome = &result.test_run.outcome;
        println!("\n╔══════════════════════════════════════╗");
        println!("║  Dry-Run Verdict: {:?}", outcome);
        println!("╚══════════════════════════════════════╝");
        println!("Evidence:\n{}", result.test_run.truncated_log);

        if format!("{:?}", result.test_run.outcome) == "Passed" {
            println!(
                "\n[🎉] Autonomous execution successful on attempt {}!",
                crucible_attempt
            );
            break;
        } else {
            println!("\n[❗] Verification failed! Re-entering Crucible Loop with ACI log...");
            messages_context.push_str(&format!(
                "\n\n[SYSTEM: VERIFICATION FAILED]\nYour previous plan resulted in a failed test execution.\nLog:\n{}\n\nPlease reflect and output a new AgentAction to fix the issue. If you need to read more files to understand the failure, use the read_files action.",
                result.test_run.truncated_log
            ));
        }
        drop(attempt_sandbox);
    }

    drop(sandbox);
    Ok(())
}
