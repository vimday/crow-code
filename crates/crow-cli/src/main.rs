use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::{Confidence, EditOp, FilePrecondition, IntentPlan, WorkspacePath};
use crow_probe::scan_workspace;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🦅 crow-code Sprint 1 God Pipeline initializing...\n");

    let current_dir = env::current_dir()?;
    println!("[1/4] Radaring Workspace: {}", current_dir.display());

    let profile = scan_workspace(&current_dir)?;
    println!(
        "    🎯 Primary Lang: {} (Tier {:?})",
        profile.primary_lang.name, profile.primary_lang.tier
    );

    let candidate = profile
        .verification_candidates
        .first()
        .expect("No verification candidates found!");
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
        // SAFETY: allow_hardlinks MUST be false for any flow that
        // executes arbitrary repo commands via the verifier. The
        // unlink-before-write discipline only protects crow-controlled
        // writes in the applier, not subprocess mutations from build
        // scripts, tests, or codegen tools.
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

    // Drop sandbox cleanly
    drop(sandbox);
    println!("\n[✓] Sprint 0/1 End-to-End sequence complete. Ready for LLM integration.");

    Ok(())
}
