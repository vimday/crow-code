fn apply_sandbox_to_workspace(
    workspace_root: &std::path::Path,
    plan: &crow_patch::IntentPlan,
) -> Result<()> {
    use crow_patch::EditOp;

    struct RollbackRecord {
        dst_path: std::path::PathBuf,
        original_content: Option<Vec<u8>>,
    }
    let mut rollback_log: Vec<RollbackRecord> = Vec::new();
    let mut created_dirs: Vec<std::path::PathBuf> = Vec::new();

    // Phase 1: Snapshot original states for rollback
    for op in &plan.operations {
        let (dst, from_dst) = match op {
            EditOp::Create { path, .. }
            | EditOp::Modify { path, .. }
            | EditOp::Delete { path, .. } => (path.to_absolute(workspace_root), None),
            EditOp::Rename { from, to, .. } => (
                to.to_absolute(workspace_root),
                Some(from.to_absolute(workspace_root)),
            ),
        };

        // Check symlinks to ensure we don't follow them out of the workspace boundary
        for path_to_eval in std::iter::once(&Some(dst.clone()))
            .chain(std::iter::once(&from_dst))
            .flatten()
        {
            if path_to_eval.is_symlink()
                || (path_to_eval.exists()
                    && !path_to_eval
                        .canonicalize()
                        .unwrap_or_else(|_| path_to_eval.clone())
                        .starts_with(workspace_root))
            {
                anyhow::bail!("Security violation: operation attempts to modify a symlink or an external path: {}", path_to_eval.display());
            }
        }

        let original_content = if dst.exists() && dst.is_file() {
            std::fs::read(&dst).ok()
        } else {
            None
        };
        rollback_log.push(RollbackRecord {
            dst_path: dst.clone(),
            original_content,
        });

        if let Some(ref fdst) = from_dst {
            let original_from = if fdst.exists() && fdst.is_file() {
                std::fs::read(fdst).ok()
            } else {
                None
            };
            rollback_log.push(RollbackRecord {
                dst_path: fdst.clone(),
                original_content: original_from,
            });
        }

        let mut track_new_dir = |p: &std::path::Path| {
            if let Some(mut current) = p.parent() {
                let mut highest_new = None;
                while !current.exists() {
                    highest_new = Some(current.to_path_buf());
                    if let Some(parent) = current.parent() {
                        current = parent;
                    } else {
                        break;
                    }
                }
                if let Some(h) = highest_new {
                    if !created_dirs.contains(&h) {
                        created_dirs.push(h);
                    }
                }
            }
        };

        track_new_dir(&dst);
        if let Some(f) = from_dst {
            track_new_dir(&f);
        }
    }

    // Phase 2: Attempt destructive apply via proper unified applier logic
    let mut apply_failed = false;
    let mut apply_error = String::new();

    // Rehydrate the plan specifically for the live workspace constraints
    match crow_workspace::PlanHydrator::hydrate(plan, &plan.base_snapshot_id, workspace_root) {
        Ok(hydrated_workspace_plan) => {
            let live_view = crow_materialize::SandboxHandle::non_owning_view_from(
                workspace_root.to_path_buf(),
                crow_materialize::MaterializationDriver::SafeCopy,
            );

            if let Err(e) =
                crow_workspace::applier::apply_plan_to_sandbox(&hydrated_workspace_plan, &live_view)
            {
                apply_failed = true;
                apply_error = format!("Workspace Applier Failed: {}", e);
            }
        }
        Err(e) => {
            apply_failed = true;
            apply_error = format!("Failed to hydrate plan for live workspace: {}", e);
        }
    }

    // Phase 3: Rollback on Failure
    if apply_failed {
        eprintln!("\n🚨 Apply failed mid-flight. Executing zero-pollution rollback...");
        for record in rollback_log {
            if let Some(content) = record.original_content {
                if let Some(parent) = record.dst_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&record.dst_path, content);
            } else if record.dst_path.exists() {
                let _ = std::fs::remove_file(&record.dst_path);
            }
        }

        // Clean up any directories we created
        for dir in created_dirs {
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        anyhow::bail!("Transaction failed and rolled back. Cause: {}", apply_error);
    }

    Ok(())
}
