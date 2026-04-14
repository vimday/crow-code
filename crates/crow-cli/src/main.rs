mod config;
mod context;
mod diff;
mod legacy_god;

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

    let messages = vec![ChatMessage::user(format!(
        "Context:\n{}\n\nTask:\n{}",
        repo_map.map_text, prompt
    ))];

    let client = Box::new(cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?);
    let compiler = IntentCompiler::new(client);

    match compiler.compile_action(&messages).await {
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

    let client = Box::new(cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?);
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
                anyhow::bail!(
                    "Epistemic loop exceeded {} steps without producing a SubmitPlan. Aborting.",
                    MAX_EPISTEMIC_STEPS
                );
            }

            println!(
                "  🧠 Epistemic Step {}/{} — Modulating Cognitive Request...",
                epistemic_step, MAX_EPISTEMIC_STEPS
            );
            let action = compiler
                .compile_action(&messages.as_messages())
                .await
                .map_err(|e| anyhow::anyhow!("Compilation failed: {:?}", e))?;

            // Track the agent's action
            messages.push_assistant(serde_json::to_string(&action)?);

            match action {
                crow_patch::AgentAction::ReadFiles { paths, rationale } => {
                    println!("    📖 Agent requests to read files: {:?}", paths);
                    println!("       Rationale: {}", rationale);

                    let mut file_contents = String::from("[READ FILES RESULT]\n");
                    for path in &paths {
                        // Read from FROZEN sandbox, not live workspace
                        let abs_path = path.to_absolute(&frozen_root);

                        // Unified streaming read via BufReader.
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

                        file_contents.push_str(&format!(
                            "--- {} ---\n{}\n\n",
                            path.as_str(),
                            content
                        ));
                    }
                    file_contents.push_str(
                        "Please proceed with your task, or read more files if necessary.",
                    );

                    let path_strings: Vec<String> =
                        paths.iter().map(|s| s.as_str().to_string()).collect();
                    messages.push_file_read(&path_strings, file_contents);
                }
                crow_patch::AgentAction::RunCommand {
                    program,
                    args,
                    rationale,
                } => {
                    println!("    👾 Agent RunCommand: `{} {}`", program, args.join(" "));
                    println!("       Rationale: {}", rationale);

                    // Allowlist: only read-only reconnaissance programs.
                    const ALLOWED_PROGRAMS: &[&str] = &[
                        "ls", "cat", "head", "tail", "find", "wc", "rg", "grep", "awk", "sed",
                        "cargo", "rustc", "python", "python3", "node", "tree", "file", "stat",
                        "du",
                    ];

                    // Extract the basename in case the model passes a path
                    let prog_basename = std::path::Path::new(&program)
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or(&program);

                    if !ALLOWED_PROGRAMS.contains(&prog_basename) {
                        println!("    ⛔ Blocked: `{}` is not in the allowlist", program);
                        messages.push_user(format!(
                            "[RUN COMMAND BLOCKED]\n`{}` is not an allowed reconnaissance command.\nAllowed: {}\nPlease use one of these or proceed to submit_plan.",
                            program,
                            ALLOWED_PROGRAMS.join(", ")
                        ));
                        continue;
                    }

                    let v_cmd = crow_probe::VerificationCommand {
                        program: program.clone(),
                        args: args.clone(),
                        cwd: None,
                    };
                    let exec_config = ExecutionConfig {
                        timeout: std::time::Duration::from_secs(10),
                        max_output_bytes: 512 * 1024, // 512KB hard cap for recon
                    };

                    let result = crow_verifier::executor::execute(
                        &frozen_root,
                        &v_cmd,
                        &exec_config,
                        &AciConfig::compact(),
                    )
                    .await;

                    match result {
                        Ok(res) => {
                            let content = format!(
                                "[RUN COMMAND RESULT]\nCommand: {} {}\nExit Code: {:?}\nOutput:\n{}",
                                program,
                                args.join(" "),
                                res.exit_code,
                                res.test_run.truncated_log
                            );
                            messages.push_user(content);
                        }
                        Err(e) => {
                            messages.push_user(format!(
                                "[RUN COMMAND ERROR]\nFailed to execute `{}`: {:?}",
                                program, e
                            ));
                        }
                    }
                    continue; // Re-prompt LLM with the outputs
                }
                crow_patch::AgentAction::SubmitPlan { plan } => {
                    println!("    ✅ Agent submitted IntentPlan!");
                    break plan;
                }
            }
        };

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
            allow_hardlinks: true, // safe: cloning from our own frozen snapshot
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

        apply_plan_to_sandbox(&hydrated_plan, &attempt_sandbox)
            .context("Failed to apply plan to sandbox")?;
        println!("    💉 Sandbox injection successful!");

        // Diff baseline: frozen_root (pre-patch) → attempt_sandbox (post-patch).
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
