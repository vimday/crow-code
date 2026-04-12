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
    println!("    🎯 Compressed map length: {} bytes", repo_map.map_text.len());

    println!("\n[2/3] Compiling IntentPlan via crow-brain (Model: {})...", cfg.model);
    let full_prompt = format!("Context:\n{}\n\nTask:\n{}", repo_map.map_text, prompt);

    let client = Box::new(cfg.build_llm_client()?);
    let compiler = IntentCompiler::new(client);

    match compiler.compile(&full_prompt).await {
        Ok(plan) => {
            println!("\n[✓] Compilation Successful!");
            println!("--- Parsed IntentPlan ---");
            println!("{}", serde_json::to_string_pretty(&plan)?);
            Ok(())
        }
        Err(e) => {
            eprintln!("\n[✗] Compilation Failed: {:?}", e);
            Err("Failed to compile IntentPlan".into())
        }
    }
}

// ─── Dry-Run command ────────────────────────────────────────────────

async fn run_dry_run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use crow_workspace::PlanHydrator;

    println!("🦅 crow-code Dry-Run mode initializing...\n");

    let cfg = CliConfig::from_env()?;
    let prompt = args.join(" ");

    println!("[1/6] Radaring Workspace: {}", cfg.workspace.display());
    let profile = scan_workspace(&cfg.workspace)?;
    let candidate = match profile.verification_candidates.first() {
        Some(c) => c,
        None => return Err("No verification candidates found. Cannot dry-run without a verifier.".into()),
    };

    println!("\n[2/6] Gathering Repomap Context via tree-sitter...");
    let repo_map = cfg.build_repo_map()?;
    println!("    🎯 Compressed map length: {} bytes", repo_map.map_text.len());

    println!("\n[3/6] Compiling IntentPlan via crow-brain (Model: {})...", cfg.model);
    let full_prompt = format!(
        "Context:\n{}\n\nTask:\n{}\n\nConstraints: Please limit your edits to Create and Modify operations if possible for this early iteration.",
        repo_map.map_text, prompt
    );

    let client = Box::new(cfg.build_llm_client()?);
    let compiler = IntentCompiler::new(client);
    let compiled_plan = compiler.compile(&full_prompt).await
        .map_err(|e| format!("Compilation failed: {:?}", e))?;

    println!("\n[4/6] Hydrating IntentPlan (resolving real workspace preconditions)...");
    let hydrated_plan = PlanHydrator::hydrate(&compiled_plan, &cfg.workspace)
        .map_err(|e| format!("Hydration failed: {:?}", e))?;

    println!("    💧 Hydrated Plan:\n{}", serde_json::to_string_pretty(&hydrated_plan)?);

    println!("\n[5/6] Materializing O(1) Sandbox and Applying Plan...");
    let mat_config = MaterializeConfig {
        source: cfg.workspace.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let sandbox = materialize(&mat_config)?;

    apply_plan_to_sandbox(&hydrated_plan, &sandbox)?;
    println!("    💉 Sandbox injection successful!");

    // ── Diff output ──
    println!("\n--- Sandbox Diff (source → patched) ---");
    diff::render_plan_diff(&cfg.workspace, sandbox.path(), &hydrated_plan);

    println!("\n[6/6] Verifying Sandbox with '{}'...", candidate.command.display());
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

    let outcome = &result.test_run.outcome;
    println!("\n╔══════════════════════════════════════╗");
    println!("║  Dry-Run Verdict: {:?}", outcome);
    println!("╚══════════════════════════════════════╝");
    println!("Evidence:\n{}", result.test_run.truncated_log);

    drop(sandbox);
    Ok(())
}
