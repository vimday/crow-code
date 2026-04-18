//! Integration test: exercises the full autonomous epistemic loop
//! (ReadFiles → SubmitPlan → hydrate → apply → verify) using a mock
//! LlmClient. This pins the state machine behaviour so that future
//! refactors cannot silently break the cognitive loop.

use async_trait::async_trait;
use crow_brain::{ChatMessage, IntentCompiler, LlmClient};
use crow_materialize::{materialize, MaterializeConfig};
use crow_patch::AgentAction;
use crow_verifier::{types::AciConfig, ExecutionConfig};
use crow_workspace::applier::apply_plan_to_sandbox;
use crow_workspace::PlanHydrator;
use std::fs;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// A mock LLM that plays back a scripted sequence of responses.
/// Each call to `generate()` pops the next response from the queue.
struct ScriptedLlm {
    responses: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    async fn generate(&self, _messages: &[ChatMessage]) -> Result<String, crow_brain::BrainError> {
        let mut resps = self.responses.lock().unwrap();
        if resps.is_empty() {
            Err(crow_brain::BrainError::Config(
                "ScriptedLlm exhausted all scripted responses".into(),
            ))
        } else {
            Ok(resps.remove(0))
        }
    }
}

/// Scaffold a minimal Rust workspace.
fn scaffold_rust_project(dir: &std::path::Path) {
    fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

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

/// Tests the happy path: Agent immediately submits a valid plan (no ReadFiles).
/// Exercises: compile_action → hydrate → apply → verify.
#[tokio::test]
async fn autonomous_loop_direct_submit() {
    let workspace = TempDir::new().unwrap();
    scaffold_rust_project(workspace.path());

    // Script: agent directly submits a Create plan
    let submit_json = r#"{
        "action": "submit_plan",
        "plan": {
            "base_snapshot_id": "auto-test-001",
            "rationale": "Add a marker file",
            "is_partial": false,
            "confidence": "High",
            "operations": [{
                "Create": {
                    "path": "MARKER.txt",
                    "content": "autonomous loop test\n",
                    "precondition": "MustNotExist"
                }
            }]
        }
    }"#;

    let client = std::sync::Arc::new(ScriptedLlm {
        responses: Arc::new(Mutex::new(vec![submit_json.into()])),
    });
    let compiler = IntentCompiler::new(client);

    // Step 1: Freeze sandbox
    let config = MaterializeConfig {
        source: workspace.path().to_path_buf(),
        artifact_dirs: vec!["target".into()],
        skip_patterns: vec![],
        allow_hardlinks: false,
    };
    let sandbox = materialize(&config).expect("Materialization should succeed");

    // Step 2: Compile (epistemic loop — agent submits directly)
    let messages = vec![ChatMessage::user("Add a marker file")];
    let action = compiler
        .compile_action(&messages)
        .await
        .expect("compile_action should succeed");

    let plan = match action {
        AgentAction::SubmitPlan { plan } => plan,
        AgentAction::ReadFiles { .. } => panic!("Expected SubmitPlan, got ReadFiles"),
        AgentAction::Recon { .. } => panic!("Expected SubmitPlan, got Recon"),
    };

    // Step 3: Hydrate against frozen sandbox
    let hydrated = PlanHydrator::hydrate(&plan, &plan.base_snapshot_id, sandbox.path())
        .expect("Hydration should succeed");

    // Step 4: Apply
    apply_plan_to_sandbox(&hydrated, &sandbox).expect("Apply should succeed");

    // Verify file exists in sandbox
    let marker = sandbox.path().join("MARKER.txt");
    assert!(marker.exists(), "MARKER.txt should exist in sandbox");
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "autonomous loop test\n"
    );

    // Step 5: Verify
    let exec_config = ExecutionConfig {
        timeout: std::time::Duration::from_secs(120),
        max_output_bytes: 5 * 1024 * 1024,
    };
    let verify_cmd = crow_probe::types::VerificationCommand::new("cargo", vec!["test"]);
    let result = crow_verifier::executor::execute(
        sandbox.path(),
        &verify_cmd,
        &exec_config,
        &AciConfig::compact(),
        None,
    )
    .await
    .expect("Verifier should succeed");

    println!("TEST LOG: {}", result.test_run.truncated_log);
    assert_eq!(
        format!("{:?}", result.test_run.outcome),
        "Passed",
        "Existing tests should still pass after adding a marker file"
    );
}

/// Tests the epistemic path: Agent first requests ReadFiles, then submits a plan.
/// Exercises: ReadFiles → file injection → SubmitPlan → hydrate → apply.
#[tokio::test]
async fn autonomous_loop_read_then_submit() {
    let workspace = TempDir::new().unwrap();
    scaffold_rust_project(workspace.path());

    // Script response 1: agent requests to read src/lib.rs
    let read_files_json = r#"{
        "action": "read_files",
        "paths": ["src/lib.rs"],
        "rationale": "I need to see the function body before modifying it."
    }"#;

    // Script response 2: after seeing the file, agent submits a Create plan
    let submit_json = r#"{
        "action": "submit_plan",
        "plan": {
            "base_snapshot_id": "auto-test-002",
            "rationale": "Add INSPECTED marker after reading the file",
            "is_partial": false,
            "confidence": "High",
            "operations": [{
                "Create": {
                    "path": "INSPECTED.txt",
                    "content": "File was inspected before plan submission.\n",
                    "precondition": "MustNotExist"
                }
            }]
        }
    }"#;

    let client = std::sync::Arc::new(ScriptedLlm {
        responses: Arc::new(Mutex::new(vec![read_files_json.into(), submit_json.into()])),
    });
    let compiler = IntentCompiler::new(client);

    // Step 1: Freeze sandbox
    let config = MaterializeConfig {
        source: workspace.path().to_path_buf(),
        artifact_dirs: vec!["target".into()],
        skip_patterns: vec![],
        allow_hardlinks: false,
    };
    let sandbox = materialize(&config).expect("Materialization should succeed");
    let frozen_root = sandbox.path().to_path_buf();

    // Step 2: Epistemic loop with structured messages
    let mut messages = vec![ChatMessage::user("Task: inspect and create marker")];

    let plan = loop {
        let action = compiler
            .compile_action(&messages)
            .await
            .expect("compile_action should succeed");

        match action {
            AgentAction::ReadFiles { paths, rationale } => {
                assert!(
                    !paths.is_empty(),
                    "ReadFiles should request at least one file"
                );
                assert!(rationale.len() > 5, "Rationale should be non-trivial");

                // Inject file contents from FROZEN sandbox as a user message
                let mut file_contents = String::from("[READ FILES RESULT]\n");
                for path in paths {
                    let abs_path = path.to_absolute(&frozen_root);
                    let content =
                        fs::read_to_string(&abs_path).unwrap_or_else(|_| "<file not found>".into());
                    file_contents.push_str(&format!("--- {} ---\n{}\n\n", path.as_str(), content));
                }
                file_contents.push_str("Please proceed with your task.");
                messages.push(ChatMessage::user(file_contents));
            }
            AgentAction::Recon { .. } => {
                panic!("Unexpected Recon in scripted test");
            }
            AgentAction::SubmitPlan { plan } => {
                break plan;
            }
        }
    };

    // Step 3: Hydrate against frozen sandbox
    let hydrated = PlanHydrator::hydrate(&plan, &plan.base_snapshot_id, &frozen_root)
        .expect("Hydration should succeed");

    // Step 4: Apply
    apply_plan_to_sandbox(&hydrated, &sandbox).expect("Apply should succeed");

    // Verify the INSPECTED marker exists
    let inspected = sandbox.path().join("INSPECTED.txt");
    assert!(inspected.exists(), "INSPECTED.txt should exist in sandbox");
}

/// Tests that the context string actually contains file content after ReadFiles.
/// This ensures the epistemic loop properly feeds sandbox data, not live workspace data.
#[tokio::test]
async fn epistemic_loop_reads_from_frozen_sandbox() {
    let workspace = TempDir::new().unwrap();
    scaffold_rust_project(workspace.path());

    // Freeze the sandbox
    let config = MaterializeConfig {
        source: workspace.path().to_path_buf(),
        artifact_dirs: vec!["target".into()],
        skip_patterns: vec![],
        allow_hardlinks: false,
    };
    let sandbox = materialize(&config).expect("Materialization should succeed");
    let frozen_root = sandbox.path().to_path_buf();

    // Now mutate the LIVE workspace after freezing — the sandbox should NOT see this.
    fs::write(
        workspace.path().join("src/lib.rs"),
        "// THIS IS A POST-FREEZE MUTATION THAT SHOULD NOT BE VISIBLE\n",
    )
    .unwrap();

    // Read from frozen sandbox (not live workspace)
    let frozen_content = fs::read_to_string(frozen_root.join("src/lib.rs")).unwrap();
    assert!(
        frozen_content.contains("pub fn greet()"),
        "Frozen sandbox should contain original content, not post-freeze mutation"
    );
    assert!(
        !frozen_content.contains("POST-FREEZE MUTATION"),
        "Frozen sandbox must NOT reflect live workspace mutations"
    );
}
