//! Physical sandbox mutation applier.
//!
//! Exclusively applies IntentPlans to SandboxHandles. Incorporates
//! strict defensive Unlink-on-Write barriers.

use crow_materialize::{MaterializationDriver, SandboxHandle};
use crow_patch::{EditOp, FilePrecondition, IntentPlan};
use std::fs;

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Precondition failed: {0}")]
    PreconditionFailed(String),
}

/// Physically implements the LLM IntentPlan inside the isolation boundary.
/// Enforces critical Unlink-on-Write isolation.
pub fn apply_plan_to_sandbox(plan: &IntentPlan, sandbox: &SandboxHandle) -> Result<(), ApplyError> {
    let root = sandbox.path();
    let is_hardlinked = sandbox.driver() == MaterializationDriver::HardlinkTree;

    for op in &plan.operations {
        match op {
            EditOp::Modify {
                path,
                preconditions: _,
                hunks: _,
            } => {
                let abs_path = path.to_absolute(&root.to_path_buf());

                // TODO: Read original content and apply diff hunks accurately.
                let original_content = fs::read_to_string(&abs_path)?;
                let new_content = original_content + "\n// modified by crow\n"; // Mock implementation for Sprint 1

                // [CRITICAL RULE]: If hardlinked, modifying in-place bridges
                // the isolation boundary and mutates the source repository.
                // We must break the physical linkage.
                if is_hardlinked && abs_path.exists() {
                    fs::remove_file(&abs_path)?;
                }

                fs::write(&abs_path, new_content)?;
            }
            EditOp::Create {
                path,
                content,
                precondition,
            } => {
                let abs_path = path.to_absolute(&root.to_path_buf());
                if precondition == &FilePrecondition::MustNotExist && abs_path.exists() {
                    return Err(ApplyError::PreconditionFailed(format!(
                        "{} already exists",
                        path.as_str()
                    )));
                }
                if let Some(parent) = abs_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&abs_path, content)?;
            }
            EditOp::Rename {
                from,
                to,
                on_conflict: _,
                source_precondition: _,
                dest_precondition: _,
            } => {
                let abs_from = from.to_absolute(&root.to_path_buf());
                let abs_to = to.to_absolute(&root.to_path_buf());

                if is_hardlinked && abs_from.exists() && abs_to.exists() {
                    // Prevent inode bridge overwriting
                    fs::remove_file(&abs_to)?;
                }
                fs::rename(&abs_from, &abs_to)?;
            }
            EditOp::Delete {
                path,
                precondition: _,
            } => {
                let abs_path = path.to_absolute(&root.to_path_buf());
                if abs_path.exists() {
                    if abs_path.is_dir() {
                        fs::remove_dir_all(&abs_path)?;
                    } else {
                        fs::remove_file(&abs_path)?;
                    }
                }
            }
        }
    }
    Ok(())
}
