//! Plan diff renderer.
//!
//! Produces human-readable, stable, memory-efficient diff output for each
//! operation in an IntentPlan by comparing source workspace against
//! post-apply sandbox.
//!
//! Uses the `similar` crate (Myers O(ND) algorithm) instead of hand-written
//! LCS, so memory usage is linear in the diff size — not quadratic in
//! the file size. Safe for 100K-line files.

use crow_patch::{EditOp, IntentPlan};
use similar::TextDiff;
use std::path::Path;

/// Render a diff between the source workspace and patched sandbox for every
/// operation in the plan. Output goes to stdout.
pub fn render_plan_diff(source_root: &Path, sandbox_root: &Path, plan: &IntentPlan) {
    for op in &plan.operations {
        match op {
            EditOp::Create { path, .. } => {
                let sandbox_file = sandbox_root.join(path.as_str());
                if let Ok(content) = std::fs::read_to_string(&sandbox_file) {
                    println!("diff --crow /dev/null b/{}", path.as_str());
                    println!("--- /dev/null");
                    println!("+++ b/{}", path.as_str());
                    let line_count = content.lines().count();
                    println!("@@ -0,0 +1,{} @@", line_count);
                    for line in content.lines() {
                        println!("+{}", line);
                    }
                }
            }
            EditOp::Modify { path, .. } => {
                let old =
                    std::fs::read_to_string(source_root.join(path.as_str())).unwrap_or_default();
                let new =
                    std::fs::read_to_string(sandbox_root.join(path.as_str())).unwrap_or_default();
                if old != new {
                    println!("diff --crow a/{f} b/{f}", f = path.as_str());
                    println!("--- a/{}", path.as_str());
                    println!("+++ b/{}", path.as_str());
                    render_myers_diff(&old, &new);
                }
            }
            EditOp::Delete { path, .. } => {
                println!("diff --crow a/{} /dev/null", path.as_str());
                println!("--- a/{}", path.as_str());
                println!("+++ /dev/null");
                if let Ok(content) = std::fs::read_to_string(source_root.join(path.as_str())) {
                    let line_count = content.lines().count();
                    println!("@@ -1,{} +0,0 @@", line_count);
                    for line in content.lines() {
                        println!("-{}", line);
                    }
                }
            }
            EditOp::Rename { from, to, .. } => {
                println!("rename {} => {}", from.as_str(), to.as_str());
            }
        }
    }
}

/// Render a unified diff using Myers O(ND) algorithm via `similar`.
/// Memory usage is O(D) where D is the edit distance, not O(N*M).
fn render_myers_diff(old: &str, new: &str) {
    let diff = TextDiff::from_lines(old, new);
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        print!("{}", hunk);
    }
}
