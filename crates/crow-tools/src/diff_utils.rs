//! Unified diff generation utilities.
//!
//! Wraps the `similar` crate to produce clean unified diffs
//! for file edit/write responses. Showing the agent exactly
//! what changed is crucial for reasoning quality.

use similar::{ChangeTag, TextDiff};

/// Generate a unified diff between old and new content.
///
/// Returns an empty string if the content is identical.
/// Uses `context_lines` lines of context around each change (default: 3).
pub fn generate_diff(old: &str, new: &str, context_lines: usize) -> String {
    if old == new {
        return String::new();
    }

    let diff = TextDiff::from_lines(old, new);
    let unified = diff
        .unified_diff()
        .context_radius(context_lines)
        .header("before", "after")
        .to_string();

    unified
}

/// Generate a compact summary of changes (e.g. "+5 -3 lines").
pub fn diff_summary(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0usize;
    let mut removed = 0usize;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }

    if added == 0 && removed == 0 {
        "no changes".to_string()
    } else {
        format!("+{added} -{removed} lines")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_content() {
        assert_eq!(generate_diff("hello\n", "hello\n", 3), "");
    }

    #[test]
    fn test_simple_change() {
        let diff = generate_diff("hello\nworld\n", "hello\nearth\n", 3);
        assert!(diff.contains("-world"));
        assert!(diff.contains("+earth"));
    }

    #[test]
    fn test_diff_summary() {
        let summary = diff_summary("a\nb\nc\n", "a\nx\ny\nc\n");
        assert!(summary.contains("+2"));
        assert!(summary.contains("-1"));
    }

    #[test]
    fn test_no_changes_summary() {
        assert_eq!(diff_summary("same\n", "same\n"), "no changes");
    }
}
