//! Shared epistemic engine for the autonomous crucible loop.
//!
//! Extracts the common logic for the ReadFiles / Recon / SubmitPlan
//! interaction cycle used by both the serial crucible (`run_dry_run`)
//! and the MCTS crucible (`run_mcts_crucible`).
//!
//! # Design Principles
//!
//! - **Single source of truth** for recon command translation, file
//!   reading, and epistemic loop control.
//! - **Strict safety**: all paths are resolved against the frozen sandbox,
//!   all commands are allowlisted, all output is budget-capped.

use anyhow::Result;
use crow_brain::IntentCompiler;
use crow_patch::{AgentAction, IntentPlan, ReconAction};
use crow_probe::VerificationCommand;
use std::path::Path;

use crate::context::ConversationManager;

// ─── Constants ──────────────────────────────────────────────────────

/// Maximum bytes to read from a single file before truncation.
const MAX_FILE_BYTES: u64 = 50 * 1024; // 50 KB

/// Maximum lines to read from a single file.
const MAX_FILE_LINES: usize = 500;

/// Maximum epistemic steps before bailing out.
const MAX_EPISTEMIC_STEPS: usize = 7;

// ─── Epistemic Loop ─────────────────────────────────────────────────

/// Run the epistemic loop until a `SubmitPlan` is produced.
///
/// Drives the ReadFiles → Recon → SubmitPlan cycle, feeding tool
/// results back into the conversation context. Returns the compiled
/// `IntentPlan` when the LLM submits one.
///
/// Used by both the serial crucible and MCTS pre-exploration.
pub async fn run_epistemic_loop(
    compiler: &IntentCompiler,
    messages: &mut ConversationManager,
    frozen_root: &Path,
) -> Result<IntentPlan> {
    let mut epistemic_step = 0;

    loop {
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

        // If it's a SubmitPlan, return immediately before pushing to history.
        if let AgentAction::SubmitPlan { plan } = action {
            println!("    ✅ Agent submitted IntentPlan!");
            return Ok(plan);
        }

        // Track the agent's action in conversation history.
        messages.push_assistant(serde_json::to_string(&action)?);

        match action {
            AgentAction::ReadFiles { paths, rationale } => {
                println!("    📖 Agent requests to read files: {:?}", paths);
                println!("       Rationale: {}", rationale);

                let file_contents = read_files_to_context(&paths, frozen_root);
                let path_strings: Vec<String> =
                    paths.iter().map(|s| s.as_str().to_string()).collect();
                messages.push_file_read(&path_strings, file_contents);
            }
            AgentAction::Recon { tool, rationale } => {
                println!("    🔍 Agent Recon: {:?}", tool);
                println!("       Rationale: {}", rationale);

                execute_recon(&tool, frozen_root, messages).await;
            }
            AgentAction::SubmitPlan { .. } => {
                // Already handled above via the early return.
                unreachable!("SubmitPlan is intercepted before push_assistant")
            }
        }
    }
}

// ─── File Reading ───────────────────────────────────────────────────

/// Read multiple files from the frozen sandbox into a formatted context string.
///
/// Each file is truncated at `MAX_FILE_BYTES` / `MAX_FILE_LINES` (whichever
/// triggers first). A system warning is appended if truncation occurred.
fn read_files_to_context(
    paths: &[crow_patch::WorkspacePath],
    frozen_root: &Path,
) -> String {
    use std::io::{BufRead, BufReader};

    let mut file_contents = String::from("[READ FILES RESULT]\n");

    for path in paths {
        let abs_path = path.to_absolute(frozen_root);
        let file_size = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);

        let content = match std::fs::File::open(&abs_path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                let lines: Vec<String> = reader
                    .lines()
                    .map_while(Result::ok)
                    .take(MAX_FILE_LINES)
                    .collect();
                let was_truncated = file_size > MAX_FILE_BYTES || lines.len() >= MAX_FILE_LINES;
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

        file_contents.push_str(&format!("--- {} ---\n{}\n\n", path.as_str(), content));
    }

    file_contents.push_str("Please proceed with your task, or read more files if necessary.");
    file_contents
}

// ─── Recon Execution ────────────────────────────────────────────────

/// Translate a `ReconAction` into a safe command invocation, execute it
/// against the frozen sandbox, and push the result into the conversation.
async fn execute_recon(
    tool: &ReconAction,
    frozen_root: &Path,
    messages: &mut ConversationManager,
) {
    let (program, args, description) = build_recon_command(tool);

    let v_cmd = VerificationCommand {
        program: program.clone(),
        args,
        cwd: None,
    };
    let exec_config = crow_verifier::ExecutionConfig {
        timeout: std::time::Duration::from_secs(10),
        max_output_bytes: 512 * 1024, // 512KB hard cap for recon
    };

    let result = crow_verifier::executor::execute(
        frozen_root,
        &v_cmd,
        &exec_config,
        &crow_verifier::types::AciConfig::compact(),
        None, // Recon: ephemeral, no cache reuse needed
    )
    .await;

    match result {
        Ok(res) => {
            let tool_name = recon_tool_name(tool);
            messages.push_recon_result(tool_name, &description, &res.test_run.truncated_log);
        }
        Err(e) => {
            messages.push_user(format!(
                "[RECON ERROR]\nFailed to execute `{}`: {:?}",
                program, e
            ));
        }
    }
}

/// Translate a `ReconAction` into `(program, args, description)`.
///
/// Single source of truth — uses the strictest variant from both
/// the serial and MCTS paths:
/// - `wc -l -c --` (always include count flags)
/// - `DirTree` depth clamped to `.min(10)`
/// - Formatted `rg` description for cleaner logs
fn build_recon_command(tool: &ReconAction) -> (String, Vec<String>, String) {
    match tool {
        ReconAction::ListDir { path } => (
            "ls".to_string(),
            vec![
                "-la".to_string(),
                "--".to_string(),
                path.as_str().to_string(),
            ],
            format!("ls -la -- {}", path.as_str()),
        ),
        ReconAction::Search {
            pattern,
            path,
            glob,
        } => {
            let mut a = vec![
                "-rn".to_string(),
                "-e".to_string(), // Explicitly mark pattern so it's not parsed as flag
                pattern.clone(),
            ];
            if let Some(g) = glob {
                a.push("-g".to_string());
                a.push(g.clone());
            }
            a.push("--".to_string()); // Terminate options before path
            if let Some(p) = path {
                a.push(p.as_str().to_string());
            } else {
                a.push(".".to_string());
            }
            let desc = format!(
                "rg -rn -e '{}' {}",
                pattern,
                path.as_ref().map(|p| p.as_str()).unwrap_or(".")
            );
            ("rg".to_string(), a, desc)
        }
        ReconAction::FileInfo { path } => (
            "file".to_string(),
            vec!["--".to_string(), path.as_str().to_string()],
            format!("file -- {}", path.as_str()),
        ),
        ReconAction::WordCount { path } => (
            "wc".to_string(),
            vec![
                "-l".to_string(),
                "-c".to_string(),
                "--".to_string(),
                path.as_str().to_string(),
            ],
            format!("wc -lc -- {}", path.as_str()),
        ),
        ReconAction::DirTree { path, max_depth } => {
            let depth = max_depth.unwrap_or(3).min(10);
            (
                "tree".to_string(),
                vec![
                    "-L".to_string(),
                    depth.to_string(),
                    "--".to_string(),
                    path.as_str().to_string(),
                ],
                format!("tree -L {} -- {}", depth, path.as_str()),
            )
        }
    }
}

/// Map a `ReconAction` variant to its tool name string for compression heuristics.
fn recon_tool_name(tool: &ReconAction) -> &'static str {
    match tool {
        ReconAction::ListDir { .. } => "list_dir",
        ReconAction::Search { .. } => "search",
        ReconAction::FileInfo { .. } => "file_info",
        ReconAction::WordCount { .. } => "word_count",
        ReconAction::DirTree { .. } => "dir_tree",
    }
}
