//! Integration test: exercises the full local pipeline
//! (hydrate → materialize → apply → verify) with a synthetic plan
//! instead of calling the real LLM. This proves the physical loop
//! is wired correctly end-to-end without requiring network access.

use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::{
    Confidence, EditOp, FilePrecondition, IntentPlan, PreconditionState, SnapshotId, WorkspacePath,
};
use crow_probe::types::VerificationCommand;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
use crow_workspace::PlanHydrator;
use std::fs;
use tempfile::TempDir;

/// Scaffold a minimal Rust workspace that `cargo test` can verify.
fn scaffold_rust_project(dir: &std::path::Path) {
    // Cargo.toml
    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    // src/lib.rs with a function the test will call
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        r#"pub fn greet() -> &'static str {
    "hello"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn greet_works() {
        assert_eq!(greet(), "hello");
    }
}
"#,
    )
    .unwrap();
}

#[tokio::test]
async fn synthetic_create_plan_passes_verification() {
    let workspace = TempDir::new().unwrap();
    scaffold_rust_project(workspace.path());

    // ── 1. Build a synthetic plan that creates a new file ──
    let plan = IntentPlan {
        base_snapshot_id: SnapshotId("integration-test".into()),
        rationale: "Add a marker file to prove create-path works".into(),
        is_partial: false,
        confidence: Confidence::High,
        requires_mcts: true,
        operations: vec![EditOp::Create {
            path: WorkspacePath::new("MARKER.txt").unwrap(),
            content: "Created by integration test.\n".into(),
            precondition: FilePrecondition::MustNotExist,
        }],
    };

    // ── 2. Materialize sandbox (Freeze Time) ──
    let config = MaterializeConfig {
        source: workspace.path().to_path_buf(),
        artifact_dirs: vec!["target".into()],
        skip_patterns: vec![],
        allow_hardlinks: false,
    };
    let sandbox = materialize(&config).expect("Materialization should succeed");

    // ── 3. Hydrate against sandbox ──
    let hydrated = PlanHydrator::hydrate(&plan, &plan.base_snapshot_id, sandbox.path())
        .expect("Hydration of a Create op should not fail");

    // Create precondition should still be MustNotExist after hydration
    if let EditOp::Create { precondition, .. } = &hydrated.operations[0] {
        assert_eq!(*precondition, FilePrecondition::MustNotExist);
    }

    // ── 4. Apply ──
    apply_plan_to_sandbox(&hydrated, &sandbox).expect("Apply should succeed");

    // Verify the file exists in the sandbox
    let marker_in_sandbox = sandbox.path().join("MARKER.txt");
    assert!(
        marker_in_sandbox.exists(),
        "MARKER.txt should exist in sandbox"
    );
    assert_eq!(
        fs::read_to_string(&marker_in_sandbox).unwrap(),
        "Created by integration test.\n"
    );

    // Verify the file does NOT exist in the real workspace (zero pollution)
    assert!(
        !workspace.path().join("MARKER.txt").exists(),
        "MARKER.txt must NOT leak into the real workspace"
    );

    // ── 5. Verify ──
    let exec_config = ExecutionConfig {
        timeout: std::time::Duration::from_secs(120),
        max_output_bytes: 5 * 1024 * 1024,
    };

    let verify_cmd = VerificationCommand::new("cargo", vec!["test"]);
    let result = crow_verifier::executor::execute(
        sandbox.path(),
        &verify_cmd,
        &exec_config,
        &AciConfig::compact(),
        None,
    )
    .await
    .expect("Verifier execution should succeed");

    // The existing test should still pass — adding a marker file does not break the build
    assert_eq!(
        format!("{:?}", result.test_run.outcome),
        "Passed",
        "Verification must pass: existing tests should be unaffected by a new marker file"
    );
}

#[tokio::test]
async fn synthetic_modify_plan_hydrates_and_applies() {
    let workspace = TempDir::new().unwrap();
    scaffold_rust_project(workspace.path());

    // ── 1. Build a Modify plan that changes the greet return value ──
    let plan = IntentPlan {
        base_snapshot_id: SnapshotId("integration-modify".into()),
        rationale: "Change greet to return 'world'".into(),
        is_partial: false,
        confidence: Confidence::High,
        requires_mcts: true,
        operations: vec![EditOp::Modify {
            path: WorkspacePath::new("src/lib.rs").unwrap(),
            preconditions: PreconditionState {
                content_hash: "will-be-replaced-by-hydrator".into(),
                expected_line_count: None, // hydrator will fill this
            },
            hunks: vec![crow_patch::DiffHunk {
                original_start: 2,
                remove_block: "    \"hello\"\n".into(),
                insert_block: "    \"world\"\n".into(),
            }],
        }],
    };

    // ── 2. Materialize sandbox (Freeze Time) ──
    let config = MaterializeConfig {
        source: workspace.path().to_path_buf(),
        artifact_dirs: vec!["target".into()],
        skip_patterns: vec![],
        allow_hardlinks: false,
    };
    let sandbox = materialize(&config).unwrap();

    // ── 3. Hydrate against sandbox ──
    let hydrated = PlanHydrator::hydrate(&plan, &plan.base_snapshot_id, sandbox.path())
        .expect("Hydration should succeed");

    if let EditOp::Modify { preconditions, .. } = &hydrated.operations[0] {
        assert_ne!(preconditions.content_hash, "will-be-replaced-by-hydrator");
        assert!(preconditions.expected_line_count.is_some());
    } else {
        panic!("Expected Modify op");
    }

    // ── 4. Apply ──
    apply_plan_to_sandbox(&hydrated, &sandbox).expect("Apply should succeed");

    // Verify the modification happened in the sandbox
    let modified = fs::read_to_string(sandbox.path().join("src/lib.rs")).unwrap();
    assert!(
        modified.contains("\"world\""),
        "Sandbox should have the patched text"
    );
    // The function body should return "world" now, not "hello".
    // Note: "hello" still appears in the test assertion line — that's expected.
    let fn_body: String = modified
        .lines()
        .take_while(|l| !l.contains("#[cfg(test)]"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !fn_body.contains("\"hello\""),
        "Function body should no longer contain the old return value"
    );

    // Verify original workspace is untouched (zero pollution)
    let original = fs::read_to_string(workspace.path().join("src/lib.rs")).unwrap();
    assert!(
        original.contains("\"hello\""),
        "Original workspace must remain untouched"
    );
}
