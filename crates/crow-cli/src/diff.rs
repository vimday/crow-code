use crossterm::style::Stylize;
use crow_patch::{EditOp, IntentPlan};
use similar::TextDiff;
use std::io::Write;
use std::path::Path;

/// Generate plain text unified patch for a single EditOp.
pub fn generate_patch_text(source_root: &Path, sandbox_root: &Path, op: &EditOp) -> String {
    let mut full_patch_text = String::new();
    match op {
        EditOp::Create { path, .. } => {
            let sandbox_file = sandbox_root.join(path.as_str());
            if let Ok(content) = std::fs::read_to_string(&sandbox_file) {
                let added = content.lines().count();
                full_patch_text.push_str(&format!("diff --crow /dev/null b/{}\n", path.as_str()));
                full_patch_text.push_str("--- /dev/null\n");
                full_patch_text.push_str(&format!("+++ b/{}\n", path.as_str()));
                full_patch_text.push_str(&format!("@@ -0,0 +1,{added} @@\n"));
                for line in content.lines() {
                    full_patch_text.push_str(&format!("+{line}\n"));
                }
            }
        }
        EditOp::Modify { path, .. } => {
            let old = std::fs::read_to_string(source_root.join(path.as_str())).unwrap_or_default();
            let new = std::fs::read_to_string(sandbox_root.join(path.as_str())).unwrap_or_default();
            if old != new {
                let diff = TextDiff::from_lines(&old, &new);
                full_patch_text.push_str(&format!("diff --crow a/{f} b/{f}\n", f = path.as_str()));
                full_patch_text.push_str(&format!("--- a/{}\n", path.as_str()));
                full_patch_text.push_str(&format!("+++ b/{}\n", path.as_str()));
                for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
                    full_patch_text.push_str(&format!("{hunk}"));
                }
            }
        }
        EditOp::Delete { path, .. } => {
            if let Ok(content) = std::fs::read_to_string(source_root.join(path.as_str())) {
                let removed = content.lines().count();
                full_patch_text.push_str(&format!("diff --crow a/{} /dev/null\n", path.as_str()));
                full_patch_text.push_str(&format!("--- a/{}\n", path.as_str()));
                full_patch_text.push_str("+++ /dev/null\n");
                full_patch_text.push_str(&format!("@@ -1,{removed} +0,0 @@\n"));
                for line in content.lines() {
                    full_patch_text.push_str(&format!("-{line}\n"));
                }
            }
        }
        EditOp::Rename { from, to, .. } => {
            full_patch_text.push_str(&format!("rename {} => {}\n", from.as_str(), to.as_str()));
        }
    }
    full_patch_text
}

/// Render a summary of the diff to stdout and write the full patch to `.crow/logs/latest.patch`.
pub fn render_plan_diff(source_root: &Path, sandbox_root: &Path, plan: &IntentPlan) {
    let crow_dir = source_root.join(".crow").join("logs");
    let _ = std::fs::create_dir_all(&crow_dir);
    let patch_path = crow_dir.join("latest.patch");
    let mut patch_file = std::fs::File::create(&patch_path).ok();

    println!();
    println!(
        "  {}",
        "◆ Plan Diff Summary"
            .bold()
            .with(crossterm::style::Color::AnsiValue(221))
    );

    for op in &plan.operations {
        let mut added = 0;
        let mut removed = 0;

        let full_patch_text = generate_patch_text(source_root, sandbox_root, op);
        if let Some(ref mut pf) = patch_file {
            let _ = pf.write_all(full_patch_text.as_bytes());
        }

        match op {
            EditOp::Create { path, .. } => {
                let sandbox_file = sandbox_root.join(path.as_str());
                if let Ok(content) = std::fs::read_to_string(&sandbox_file) {
                    added = content.lines().count();
                }
                print_file_stat(path.as_str(), added, removed, "create");
            }
            EditOp::Modify { path, .. } => {
                let old =
                    std::fs::read_to_string(source_root.join(path.as_str())).unwrap_or_default();
                let new =
                    std::fs::read_to_string(sandbox_root.join(path.as_str())).unwrap_or_default();
                if old != new {
                    let diff = TextDiff::from_lines(&old, &new);
                    for change in diff.iter_all_changes() {
                        match change.tag() {
                            similar::ChangeTag::Delete => removed += 1,
                            similar::ChangeTag::Insert => added += 1,
                            similar::ChangeTag::Equal => {}
                        }
                    }
                    print_file_stat(path.as_str(), added, removed, "modify");
                }
            }
            EditOp::Delete { path, .. } => {
                if let Ok(content) = std::fs::read_to_string(source_root.join(path.as_str())) {
                    removed = content.lines().count();
                }
                print_file_stat(path.as_str(), added, removed, "delete");
            }
            EditOp::Rename { from, to, .. } => {
                println!(
                    "    {} {} {} {}",
                    "renamed:".with(crossterm::style::Color::AnsiValue(245)),
                    from.as_str(),
                    "=>".with(crossterm::style::Color::AnsiValue(240)),
                    to.as_str()
                );
            }
        }
    }

    println!(
        "    {}",
        "(View full diff in .crow/logs/latest.patch)".with(crossterm::style::Color::AnsiValue(240))
    );
}

fn print_file_stat(path: &str, added: usize, removed: usize, op: &str) {
    let op_color = match op {
        "create" => crossterm::style::Color::AnsiValue(114), // green
        "delete" => crossterm::style::Color::AnsiValue(203), // red
        _ => crossterm::style::Color::AnsiValue(110),        // blueish for modify
    };

    let display_path = if path.chars().count() > 38 {
        format!("...{}", &path[path.chars().count().saturating_sub(35)..])
    } else {
        path.to_string()
    };

    let pad = 40_usize.saturating_sub(display_path.chars().count());
    let path_disp = format!("{}{}", display_path, " ".repeat(pad));

    let a_str = if added > 0 {
        format!("+{added}")
            .with(crossterm::style::Color::AnsiValue(114))
            .to_string()
    } else {
        "  ".to_string()
    };

    let r_str = if removed > 0 {
        format!("-{removed}")
            .with(crossterm::style::Color::AnsiValue(203))
            .to_string()
    } else {
        "  ".to_string()
    };

    let max_ticks: usize = 10;
    let total = added + removed;
    let mut bar_str = String::new();

    if total > 0 {
        let added_ticks = ((added as f64 / total as f64) * max_ticks as f64).round() as usize;
        let removed_ticks = ((removed as f64 / total as f64) * max_ticks as f64).round() as usize;

        let diff = max_ticks.saturating_sub(added_ticks + removed_ticks);
        let final_added_ticks = added_ticks + diff;

        bar_str.push_str(
            &"█"
                .repeat(final_added_ticks)
                .with(crossterm::style::Color::AnsiValue(114))
                .to_string(),
        );
        bar_str.push_str(
            &"█"
                .repeat(removed_ticks)
                .with(crossterm::style::Color::AnsiValue(203))
                .to_string(),
        );
    } else {
        bar_str = "░"
            .repeat(max_ticks)
            .with(crossterm::style::Color::AnsiValue(240))
            .to_string();
    }

    let total_stat = format!("{a_str:>18} {r_str:>18}");

    println!(
        "    {} |{}| {} [{}]",
        path_disp.with(crossterm::style::Color::AnsiValue(248)),
        bar_str,
        total_stat,
        op.with(op_color)
    );
}
