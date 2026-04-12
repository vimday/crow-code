use crow_brain::{IntentCompiler, ReqwestLlmClient};
use crow_intel::RepoWalker;
use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::{Confidence, EditOp, FilePrecondition, IntentPlan, WorkspacePath};
use crow_probe::scan_workspace;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() >= 3 && args[1] == "compile" {
        return run_compile_only(&args[2..]).await;
    }

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

async fn run_compile_only(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    println!("🦅 crow-code Sprint 2 Compile-Only mode initializing...\n");
    let current_dir = env::current_dir()?;

    let prompt = args.join(" ");
    let api_key = env::var("OPENAI_API_KEY")
        .or_else(|_| env::var("CROW_API_KEY"))
        .map_err(|_| "Missing API Key. Please set OPENAI_API_KEY or CROW_API_KEY.")?;
        
    let model = env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4-turbo".to_string());

    println!("[1/3] Gathering Repomap Context via tree-sitter...");
    let walker = RepoWalker::new();
    let repo_map = walker.build_repo_map(&current_dir)?;
    println!("    🎯 Compressed map length: {} bytes", repo_map.map_text.len());

    println!("\n[2/3] Compiling IntentPlan via crow-brain (Model: {})...", model);
    let full_prompt = format!(
        "Context:\n{}\n\nTask:\n{}",
        repo_map.map_text, prompt
    );

    let client = Box::new(ReqwestLlmClient::new(api_key, model, None)?);
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
