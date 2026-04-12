use crow_patch::{EditOp, FilePrecondition, IntentPlan, ConflictStrategy};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum HydrationError {
    IoError { path: String, reason: String },
}

impl std::fmt::Display for HydrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HydrationError::IoError { path, reason } => write!(f, "Failed to hydrate {}: {}", path, reason),
        }
    }
}
impl std::error::Error for HydrationError {}

/// Intercepts an LLM-compiled IntentPlan and replaces hallucinated or incomplete
/// safety preconditions (hashes, line counts) with ground truth from the workspace.
pub struct PlanHydrator;

impl PlanHydrator {
    pub fn hydrate(plan: &IntentPlan, workspace_root: &Path) -> Result<IntentPlan, HydrationError> {
        let mut hydrated = plan.clone();
        
        for op in &mut hydrated.operations {
            match op {
                EditOp::Modify { path, preconditions, .. } => {
                    let absolute_path = workspace_root.join(path.as_str());
                    let (hash, lines) = Self::compute_file_state(&absolute_path)
                        .map_err(|e| HydrationError::IoError { 
                            path: path.as_str().to_string(), 
                            reason: e 
                        })?;
                    
                    preconditions.content_hash = hash;
                    preconditions.expected_line_count = Some(lines);
                }
                EditOp::Delete { path, precondition } => {
                    let absolute_path = workspace_root.join(path.as_str());
                    let (hash, _) = Self::compute_file_state(&absolute_path)
                        .map_err(|e| HydrationError::IoError { 
                            path: path.as_str().to_string(), 
                            reason: e 
                        })?;
                    *precondition = FilePrecondition::MustExistWithHash(hash);
                }
                EditOp::Rename { from, to, on_conflict, source_precondition, dest_precondition } => {
                    let absolute_source = workspace_root.join(from.as_str());
                    let (hash, _) = Self::compute_file_state(&absolute_source)
                        .map_err(|e| HydrationError::IoError { 
                            path: from.as_str().to_string(), 
                            reason: e 
                        })?;
                    *source_precondition = FilePrecondition::MustExistWithHash(hash);

                    if *on_conflict == ConflictStrategy::Fail {
                        *dest_precondition = FilePrecondition::MustNotExist;
                    } else if *on_conflict == ConflictStrategy::Overwrite {
                        let absolute_dest = workspace_root.join(to.as_str());
                        if absolute_dest.exists() {
                            // If it exists, hydrate its exact hash so we don't blindly overwrite
                            // a file that changed between compilation and application.
                            let (hash, _) = Self::compute_file_state(&absolute_dest)
                                .map_err(|e| HydrationError::IoError { 
                                    path: to.as_str().to_string(), 
                                    reason: e 
                                })?;
                            *dest_precondition = FilePrecondition::MustExistWithHash(hash);
                        } else {
                            *dest_precondition = FilePrecondition::MustNotExist;
                        }
                    }
                }
                EditOp::Create { precondition, .. } => {
                    // Ensure the model doesn't bypass safety by setting a weak precondition.
                    // If conflict strategy were added to Create, we'd handle it here,
                    // but for now Create is strictly MustNotExist.
                    *precondition = FilePrecondition::MustNotExist;
                }
            }
        }

        Ok(hydrated)
    }

    fn compute_file_state(path: &Path) -> Result<(String, usize), String> {
        let content = fs::read(path).map_err(|e| e.to_string())?;
        
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let hash = hex::encode(hasher.finalize());
        
        let lines = std::str::from_utf8(&content).unwrap_or("").lines().count();

        Ok((hash, lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crow_patch::{WorkspacePath, Confidence, SnapshotId, PreconditionState};
    use tempfile::tempdir;

    #[test]
    fn hydrates_modify_op_correctly() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.rs");
        let content = "line 1\nline 2\nline3\n";
        fs::write(&file_path, content).unwrap();

        let plan = IntentPlan {
            base_snapshot_id: SnapshotId("snap1".into()),
            rationale: "test".into(),
            is_partial: false,
            confidence: Confidence::High,
            operations: vec![
                EditOp::Modify {
                    path: WorkspacePath::new("test.rs").unwrap(),
                    preconditions: PreconditionState {
                        content_hash: "hallucinated".into(),
                        expected_line_count: None,
                    },
                    hunks: vec![],
                }
            ],
        };

        let hydrated = PlanHydrator::hydrate(&plan, dir.path()).expect("Hydration must succeed");

        if let EditOp::Modify { preconditions, .. } = &hydrated.operations[0] {
            assert_ne!(preconditions.content_hash, "hallucinated");
            assert_eq!(preconditions.expected_line_count, Some(3)); // 3 lines
        } else {
            panic!("Expected Modify op");
        }
    }
}
