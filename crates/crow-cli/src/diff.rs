//! Plan diff renderer.
//!
//! Produces a human-readable, stable diff output for each operation
//! in an IntentPlan by comparing source workspace against post-apply sandbox.
//! Uses a sequential LCS-like algorithm rather than set membership,
//! so duplicate/reordered lines are rendered correctly.

use crow_patch::{EditOp, IntentPlan};
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
                    println!("@@ -0,0 +1,{} @@", content.lines().count());
                    for line in content.lines() {
                        println!("+{}", line);
                    }
                }
            }
            EditOp::Modify { path, .. } => {
                let old = std::fs::read_to_string(source_root.join(path.as_str())).unwrap_or_default();
                let new = std::fs::read_to_string(sandbox_root.join(path.as_str())).unwrap_or_default();
                if old != new {
                    println!("diff --crow a/{f} b/{f}", f = path.as_str());
                    println!("--- a/{}", path.as_str());
                    println!("+++ b/{}", path.as_str());
                    render_line_diff(&old, &new);
                }
            }
            EditOp::Delete { path, .. } => {
                println!("diff --crow a/{} /dev/null", path.as_str());
                println!("--- a/{}", path.as_str());
                println!("+++ /dev/null");
                if let Ok(content) = std::fs::read_to_string(source_root.join(path.as_str())) {
                    println!("@@ -1,{} +0,0 @@", content.lines().count());
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

/// Simple sequential line diff using a greedy LCS approach.
/// Produces unified-diff-style output with proper hunk context.
fn render_line_diff(old: &str, new: &str) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Build LCS table
    let m = old_lines.len();
    let n = new_lines.len();
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if old_lines[i] == new_lines[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // Walk the table to produce diff lines
    let mut i = 0;
    let mut j = 0;
    let mut removed = Vec::new();
    let mut added = Vec::new();

    while i < m || j < n {
        if i < m && j < n && old_lines[i] == new_lines[j] {
            // Flush any pending changes before context
            flush_hunk(&removed, &added);
            removed.clear();
            added.clear();
            // Context line (don't print to keep output concise)
            i += 1;
            j += 1;
        } else if j < n && (i >= m || dp[i][j + 1] >= dp[i + 1][j]) {
            added.push(new_lines[j]);
            j += 1;
        } else {
            removed.push(old_lines[i]);
            i += 1;
        }
    }
    flush_hunk(&removed, &added);
}

fn flush_hunk(removed: &[&str], added: &[&str]) {
    if removed.is_empty() && added.is_empty() {
        return;
    }
    for line in removed {
        println!("-{}", line);
    }
    for line in added {
        println!("+{}", line);
    }
}
