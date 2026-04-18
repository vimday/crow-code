use anyhow::{Context, Result};
use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::{Confidence, EditOp, FilePrecondition, IntentPlan, WorkspacePath};
use crow_probe::scan_workspace;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
use std::env;

/// Sprint 1: God Pipeline (synthetic self-test)
pub async fn run_god_pipeline() -> Result<()> {
    println!("🦅 crow-code Sprint 1 God Pipeline initializing...\n");

    let current_dir = env::current_dir()?;
    println!("[1/4] Radaring Workspace: {}", current_dir.display());

    let profile = scan_workspace(&current_dir).map_err(|e| anyhow::anyhow!(e))?;
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

    let sandbox = tokio::task::spawn_blocking(move || materialize(&config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize sandbox")?;
    println!(
        "    🛡️  Sandbox established at: {}",
        sandbox.path().display()
    );
    println!("    🏎️  Driver active: {:?}", sandbox.driver());

    println!("\n[3/4] Synthesizing and Applying 'IntentPlan' (Unlink-on-Write enabled)");
    let snapshot_id = crate::snapshot::resolve_snapshot_id(&current_dir);
    let mock_plan = IntentPlan {
        base_snapshot_id: snapshot_id.clone(),
        rationale: "Pipeline synthetic verification inject".into(),
        is_partial: false,
        confidence: Confidence::High,
        operations: vec![EditOp::Create {
            path: WorkspacePath::new("dummy_test_crow.txt")?,
            content: "This is a synthetic artifact created by God Pipeline.\n".into(),
            precondition: FilePrecondition::MustNotExist,
        }],
    };

    let hydrated_plan = crow_workspace::PlanHydrator::hydrate(&mock_plan, &snapshot_id, sandbox.path()).unwrap();
    apply_plan_to_sandbox(&hydrated_plan, &sandbox).context("Failed to apply synthetic plan")?;
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
        None,
    )
    .await
    .context("Verification execution failed")?;

    println!("\n[✓] Verification Cycle Completed");
    println!("--- Result Summary ---");
    println!("Outcome: {:?}", result.test_run.outcome);
    println!(
        "Truncated Evidence Dump:\n{}",
        result.test_run.truncated_log
    );

    Ok(())
}
