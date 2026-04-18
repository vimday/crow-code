use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn git_cmd() -> Command {
    let mut cmd = Command::new("git");
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PREFIX");
    cmd
}

/// This integration test acts as the core "Reflective Metric Benchmark" for the zero-pollution invariant.
/// It creates a mock Git repository, sets up `PreconditionState::Tolerant` but then forces an apply failure,
/// or simulates a bad test run, and ensures that `git status` remains completely perfectly clean.
#[test]
fn benchmark_zero_pollution_on_failed_apply() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();

    // 1. Initialize stable Git repo (baseline)
    setup_git_repo(workspace);
    std::fs::write(workspace.join("src_file.rs"), b"fn main() {}").unwrap();
    git_cmd()
        .args(["add", "src_file.rs"])
        .current_dir(workspace)
        .status()
        .unwrap();
    git_cmd()
        .args(["commit", "-m", "init"])
        .current_dir(workspace)
        .status()
        .unwrap();

    let baseline_status = get_git_status(workspace);
    assert!(baseline_status.is_empty(), "Baseline must be clean");

    // 2. Create the Agent's compiled intent plan (simulating a hallucination or logic error)
    // We simulate an edit that tries to modify the file but in a way that we simulate an application error.
    let plan = crow_patch::IntentPlan {
        rationale: "benchmarking fail".to_string(),
        base_snapshot_id: crow_patch::SnapshotId("snapshot-abc".to_string()),
        confidence: crow_patch::Confidence::Low,
        is_partial: false,
        requires_mcts: true,
        operations: vec![crow_patch::EditOp::Modify {
            path: crow_patch::WorkspacePath::new("src_file.rs").unwrap(),
            preconditions: crow_patch::types::PreconditionState {
                content_hash: "invalid-hash-that-will-cause-conflict".to_string(),
                expected_line_count: None,
            },
            hunks: vec![
                // Invalid hunk that causes apply to reject
                crow_patch::types::DiffHunk {
                    original_start: 1,
                    remove_block: "fn doesnt_exist() {}".to_string(),
                    insert_block: "fn injected_malware() {}".to_string(),
                },
            ],
        }],
    };

    // 3. Instead of using the `main.rs` loop directly which requires a full LLM and network stack,
    // we use the actual `applier` directly against a sandbox.
    use crow_materialize::{materialize, MaterializeConfig};
    use crow_workspace::applier::apply_plan_to_sandbox;

    let mat_config = MaterializeConfig {
        source: workspace.to_path_buf(),
        artifact_dirs: vec![],
        skip_patterns: vec![],
        allow_hardlinks: false,
    };

    // Fast O(1) materialization
    let sandbox = materialize(&mat_config).expect("Failed to materialize");

    // Attempt to mutate sandbox
    let apply_result = apply_plan_to_sandbox(&plan, &sandbox.non_owning_view());

    // 4. Assert that the operation failed (Reject Phase)
    assert!(apply_result.is_err(), "The invalid patch must be rejected");

    // 5. Assert Zero Pollution: The original workspace must be completely 100% untouched.
    let final_status = get_git_status(workspace);
    assert!(
        final_status.is_empty(),
        "CRITICAL FAILURE: Workspace was polluted! Status: {}",
        final_status
    );

    let original_content = std::fs::read_to_string(workspace.join("src_file.rs")).unwrap();
    assert_eq!(original_content, "fn main() {}");

    // In a full run, `EventLedger::PlanRolledBack` would be emitted.
    // The metrics guarantee is held.
}

fn setup_git_repo(path: &Path) {
    git_cmd().args(["init"]).current_dir(path).status().unwrap();
    git_cmd()
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .status()
        .unwrap();
    git_cmd()
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .status()
        .unwrap();
}

fn get_git_status(path: &Path) -> String {
    let output = git_cmd()
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
