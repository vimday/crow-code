use crow_patch::{ConflictStrategy, EditOp, FilePrecondition, IntentPlan};
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum HydrationError {
    IoError { path: String, reason: String },
    SnapshotMismatch { expected: String, actual: String },
}

impl std::fmt::Display for HydrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HydrationError::IoError { path, reason } => {
                write!(f, "Failed to hydrate {path}: {reason}")
            }
            HydrationError::SnapshotMismatch { expected, actual } => {
                write!(
                    f,
                    "Snapshot anchor mismatch! Plan was generated against '{actual}' but we are hydrating against '{expected}'. The timeline has drifted."
                )
            }
        }
    }
}
impl std::error::Error for HydrationError {}

/// Intercepts an LLM-compiled IntentPlan and replaces hallucinated or incomplete
/// safety preconditions (hashes, line counts) with ground truth from the workspace.
pub struct PlanHydrator;

impl PlanHydrator {
    pub fn hydrate(
        plan: &IntentPlan,
        expected_snapshot: &crow_patch::SnapshotId,
        workspace_root: &Path,
    ) -> Result<IntentPlan, HydrationError> {
        if plan.base_snapshot_id != *expected_snapshot {
            return Err(HydrationError::SnapshotMismatch {
                expected: expected_snapshot.0.clone(),
                actual: plan.base_snapshot_id.0.clone(),
            });
        }

        let mut hydrated = plan.clone();

        for op in &mut hydrated.operations {
            match op {
                EditOp::Modify {
                    path,
                    preconditions,
                    ..
                } => {
                    let absolute_path = workspace_root.join(path.as_str());
                    let (hash, lines) = Self::compute_file_state(&absolute_path).map_err(|e| {
                        HydrationError::IoError {
                            path: path.as_str().to_string(),
                            reason: e,
                        }
                    })?;

                    preconditions.content_hash = hash;
                    preconditions.expected_line_count = Some(lines);
                }
                EditOp::Delete { path, precondition } => {
                    let absolute_path = workspace_root.join(path.as_str());
                    let (hash, _) = Self::compute_file_state(&absolute_path).map_err(|e| {
                        HydrationError::IoError {
                            path: path.as_str().to_string(),
                            reason: e,
                        }
                    })?;
                    *precondition = FilePrecondition::MustExistWithHash(hash);
                }
                EditOp::Rename {
                    from,
                    to,
                    on_conflict,
                    source_precondition,
                    dest_precondition,
                } => {
                    let absolute_source = workspace_root.join(from.as_str());
                    let (hash, _) = Self::compute_file_state(&absolute_source).map_err(|e| {
                        HydrationError::IoError {
                            path: from.as_str().to_string(),
                            reason: e,
                        }
                    })?;
                    *source_precondition = FilePrecondition::MustExistWithHash(hash);

                    if *on_conflict == ConflictStrategy::Fail {
                        *dest_precondition = FilePrecondition::MustNotExist;
                    } else if *on_conflict == ConflictStrategy::Overwrite {
                        let absolute_dest = workspace_root.join(to.as_str());
                        if absolute_dest.exists() {
                            // If it exists, hydrate its exact hash so we don't blindly overwrite
                            // a file that changed between compilation and application.
                            let (hash, _) =
                                Self::compute_file_state(&absolute_dest).map_err(|e| {
                                    HydrationError::IoError {
                                        path: to.as_str().to_string(),
                                        reason: e,
                                    }
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

        let hash = crow_patch::sha256_hex(&content);
        let lines = std::str::from_utf8(&content).unwrap_or("").lines().count();

        Ok((hash, lines))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crow_patch::{Confidence, PreconditionState, SnapshotId, WorkspacePath};
    use tempfile::tempdir;

    fn make_plan(ops: Vec<EditOp>) -> IntentPlan {
        IntentPlan {
            base_snapshot_id: SnapshotId("snap1".into()),
            rationale: "test".into(),
            is_partial: false,
            confidence: Confidence::High,
            requires_mcts: true,
            operations: ops,
        }
    }

    #[test]
    fn hydrates_modify_op_correctly() {
        let dir = tempdir().unwrap();
        let content = "line 1\nline 2\nline3\n";
        fs::write(dir.path().join("test.rs"), content).unwrap();

        let plan = make_plan(vec![EditOp::Modify {
            path: WorkspacePath::new("test.rs").unwrap(),
            preconditions: PreconditionState {
                content_hash: "hallucinated".into(),
                expected_line_count: None,
            },
            hunks: vec![],
        }]);

        let hydrated = PlanHydrator::hydrate(&plan, &SnapshotId("snap1".into()), dir.path())
            .expect("Hydration must succeed");

        if let EditOp::Modify { preconditions, .. } = &hydrated.operations[0] {
            assert_ne!(preconditions.content_hash, "hallucinated");
            assert_eq!(preconditions.expected_line_count, Some(3));
        } else {
            panic!("Expected Modify op");
        }
    }

    #[test]
    fn hydrates_delete_op_with_real_hash() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("to_delete.txt"), "goodbye").unwrap();

        let plan = make_plan(vec![EditOp::Delete {
            path: WorkspacePath::new("to_delete.txt").unwrap(),
            precondition: FilePrecondition::MustExist, // model guessed weak precondition
        }]);

        let hydrated =
            PlanHydrator::hydrate(&plan, &SnapshotId("snap1".into()), dir.path()).unwrap();

        if let EditOp::Delete { precondition, .. } = &hydrated.operations[0] {
            match precondition {
                FilePrecondition::MustExistWithHash(h) => assert!(!h.is_empty()),
                other => panic!("Expected MustExistWithHash, got {other:?}"),
            }
        } else {
            panic!("Expected Delete op");
        }
    }

    #[test]
    fn hydrates_rename_fail_sets_dest_must_not_exist() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("old.txt"), "content").unwrap();

        let plan = make_plan(vec![EditOp::Rename {
            from: WorkspacePath::new("old.txt").unwrap(),
            to: WorkspacePath::new("new.txt").unwrap(),
            on_conflict: ConflictStrategy::Fail,
            source_precondition: FilePrecondition::MustExist, // model guessed
            dest_precondition: FilePrecondition::MustExist,   // model guessed wrong
        }]);

        let hydrated =
            PlanHydrator::hydrate(&plan, &SnapshotId("snap1".into()), dir.path()).unwrap();

        if let EditOp::Rename {
            source_precondition,
            dest_precondition,
            ..
        } = &hydrated.operations[0]
        {
            // Source must be hydrated with real hash
            match source_precondition {
                FilePrecondition::MustExistWithHash(h) => assert!(!h.is_empty()),
                other => panic!("Expected source MustExistWithHash, got {other:?}"),
            }
            // Dest must be MustNotExist since on_conflict is Fail
            assert_eq!(*dest_precondition, FilePrecondition::MustNotExist);
        } else {
            panic!("Expected Rename op");
        }
    }

    #[test]
    fn hydrates_rename_overwrite_dest_exists_gets_hash() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("src.txt"), "source content").unwrap();
        fs::write(dir.path().join("dst.txt"), "dest will be overwritten").unwrap();

        let plan = make_plan(vec![EditOp::Rename {
            from: WorkspacePath::new("src.txt").unwrap(),
            to: WorkspacePath::new("dst.txt").unwrap(),
            on_conflict: ConflictStrategy::Overwrite,
            source_precondition: FilePrecondition::MustExist, // hallucinated
            dest_precondition: FilePrecondition::MustNotExist, // hallucinated — wrong!
        }]);

        let hydrated =
            PlanHydrator::hydrate(&plan, &SnapshotId("snap1".into()), dir.path()).unwrap();

        if let EditOp::Rename {
            source_precondition,
            dest_precondition,
            ..
        } = &hydrated.operations[0]
        {
            match source_precondition {
                FilePrecondition::MustExistWithHash(h) => assert!(!h.is_empty()),
                other => panic!("Expected source hash, got {other:?}"),
            }
            // Dest exists, so system must have hydrated its hash
            match dest_precondition {
                FilePrecondition::MustExistWithHash(h) => assert!(!h.is_empty()),
                other => panic!("Expected dest hash for existing file, got {other:?}"),
            }
        } else {
            panic!("Expected Rename op");
        }
    }

    #[test]
    fn hydrates_rename_overwrite_dest_absent_gets_must_not_exist() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("src.txt"), "source content").unwrap();
        // dst.txt intentionally does NOT exist

        let plan = make_plan(vec![EditOp::Rename {
            from: WorkspacePath::new("src.txt").unwrap(),
            to: WorkspacePath::new("dst.txt").unwrap(),
            on_conflict: ConflictStrategy::Overwrite,
            source_precondition: FilePrecondition::MustExist,
            dest_precondition: FilePrecondition::MustExist, // hallucinated — wrong!
        }]);

        let hydrated =
            PlanHydrator::hydrate(&plan, &SnapshotId("snap1".into()), dir.path()).unwrap();

        if let EditOp::Rename {
            dest_precondition, ..
        } = &hydrated.operations[0]
        {
            assert_eq!(*dest_precondition, FilePrecondition::MustNotExist);
        } else {
            panic!("Expected Rename op");
        }
    }
}
