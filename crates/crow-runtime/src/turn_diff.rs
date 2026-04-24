//! Turn-level file change tracker (Codex `TurnDiffTracker` pattern).
//!
//! Snapshots files before tool calls modify them, then computes a
//! unified diff between the baseline and current state at the end of
//! a turn. This provides users with a clear view of exactly what the
//! agent changed.
//!
//! # Design
//!
//! - Baseline snapshots are taken lazily: the first time a file is
//!   touched during a turn, its content is captured.
//! - New files get no baseline (shown as additions from `/dev/null`).
//! - Diffs use the `similar` crate for in-memory unified diff.
//! - The tracker is reset between turns.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Tracks file changes within a single agent turn and produces
/// aggregated unified diffs.
#[derive(Default, Debug)]
pub struct TurnDiffTracker {
    /// Baseline content captured before modification.
    /// Key: canonical file path. Value: original content (None = file didn't exist).
    baselines: HashMap<PathBuf, Option<Vec<u8>>>,
    /// Set of files that were created during this turn.
    created: std::collections::HashSet<PathBuf>,
}

impl TurnDiffTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the tracker for a new turn. Must be called at the start of each turn.
    pub fn reset(&mut self) {
        self.baselines.clear();
        self.created.clear();
    }

    /// Number of tracked files.
    pub fn file_count(&self) -> usize {
        self.baselines.len()
    }

    /// Snapshot a file before it is modified. Only the first call for
    /// each path captures a baseline; subsequent calls are no-ops.
    ///
    /// Call this before applying any patch or writing to the file.
    pub fn snapshot_before_modify(&mut self, path: &Path) {
        let canonical = normalize_path(path);
        if self.baselines.contains_key(&canonical) {
            return; // Already have a baseline
        }
        let content = fs::read(&canonical).ok();
        if content.is_none() {
            self.created.insert(canonical.clone());
        }
        self.baselines.insert(canonical, content);
    }

    /// Snapshot multiple files before modification.
    pub fn snapshot_files(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.snapshot_before_modify(path);
        }
    }

    /// Compute the aggregated unified diff for all tracked files.
    ///
    /// Returns `None` if no files were changed or all changes are identical
    /// to their baselines.
    pub fn unified_diff(&self) -> Option<String> {
        let mut aggregated = String::new();

        // Sort paths for deterministic output
        let mut paths: Vec<&PathBuf> = self.baselines.keys().collect();
        paths.sort();

        for path in paths {
            let baseline = self.baselines.get(path);
            let current = fs::read(path).ok();

            // Compute per-file diff
            if let Some(file_diff) = self.compute_file_diff(path, baseline, current.as_deref()) {
                aggregated.push_str(&file_diff);
                if !aggregated.ends_with('\n') {
                    aggregated.push('\n');
                }
            }
        }

        if aggregated.trim().is_empty() {
            None
        } else {
            Some(aggregated)
        }
    }

    /// Get a summary of changes (file list with change types).
    pub fn change_summary(&self) -> Vec<(PathBuf, ChangeKind)> {
        let mut changes = Vec::new();
        let mut paths: Vec<&PathBuf> = self.baselines.keys().collect();
        paths.sort();

        for path in paths {
            let baseline = self.baselines.get(path);
            let current_exists = path.exists();
            let was_created = self.created.contains(path);

            let kind = if was_created && current_exists {
                ChangeKind::Added
            } else if !current_exists {
                ChangeKind::Deleted
            } else {
                // Check if content actually changed
                let baseline_content = baseline.and_then(|b| b.as_deref());
                let current_content = fs::read(path).ok();
                if baseline_content == current_content.as_deref() {
                    continue; // No actual change
                }
                ChangeKind::Modified
            };

            changes.push((path.clone(), kind));
        }

        changes
    }

    /// Compute a unified diff for a single file.
    fn compute_file_diff(
        &self,
        path: &Path,
        baseline: Option<&Option<Vec<u8>>>,
        current: Option<&[u8]>,
    ) -> Option<String> {
        let left = baseline
            .and_then(|b| b.as_deref())
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("");
        let right = current
            .and_then(|c| std::str::from_utf8(c).ok())
            .unwrap_or("");

        // Fast path: identical content
        if left == right {
            return None;
        }

        let display_path = path.display().to_string();
        let is_new = baseline.is_some_and(Option::is_none);
        let is_deleted = current.is_none();

        let mut diff_text = String::new();
        diff_text.push_str(&format!("diff --git a/{display_path} b/{display_path}\n"));

        if is_new {
            diff_text.push_str("new file mode 100644\n");
        } else if is_deleted {
            diff_text.push_str("deleted file mode 100644\n");
        }

        let old_header = if is_new {
            "/dev/null".to_string()
        } else {
            format!("a/{display_path}")
        };
        let new_header = if is_deleted {
            "/dev/null".to_string()
        } else {
            format!("b/{display_path}")
        };

        let text_diff = similar::TextDiff::from_lines(left, right);
        let unified = text_diff
            .unified_diff()
            .context_radius(3)
            .header(&old_header, &new_header)
            .to_string();

        diff_text.push_str(&unified);
        Some(diff_text)
    }
}

/// Type of change detected for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => write!(f, "added"),
            Self::Modified => write!(f, "modified"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

/// Normalize a path for consistent comparison.
fn normalize_path(path: &Path) -> PathBuf {
    // Attempt to canonicalize; fall back to the original path if it
    // doesn't exist yet (new file case).
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn tracks_new_file() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("new.txt");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_before_modify(&file);

        // Now "create" the file
        fs::write(&file, "hello world\n").expect("write");

        let summary = tracker.change_summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].1, ChangeKind::Added);

        let diff = tracker.unified_diff().expect("should have diff");
        assert!(diff.contains("new file mode"));
        assert!(diff.contains("+hello world"));
    }

    #[test]
    fn tracks_modification() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("existing.txt");
        fs::write(&file, "original content\n").expect("write");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_before_modify(&file);

        // Modify the file
        fs::write(&file, "modified content\n").expect("write");

        let summary = tracker.change_summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].1, ChangeKind::Modified);

        let diff = tracker.unified_diff().expect("should have diff");
        assert!(diff.contains("-original content"));
        assert!(diff.contains("+modified content"));
    }

    #[test]
    fn tracks_deletion() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("doomed.txt");
        fs::write(&file, "goodbye\n").expect("write");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_before_modify(&file);

        // Delete the file
        fs::remove_file(&file).expect("remove");

        let summary = tracker.change_summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].1, ChangeKind::Deleted);

        let diff = tracker.unified_diff().expect("should have diff");
        assert!(diff.contains("deleted file mode"));
        assert!(diff.contains("-goodbye"));
    }

    #[test]
    fn idempotent_snapshot() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("file.txt");
        fs::write(&file, "v1\n").expect("write");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_before_modify(&file);

        // Modify first time
        fs::write(&file, "v2\n").expect("write");

        // Second snapshot should be a no-op (keeps v1 baseline)
        tracker.snapshot_before_modify(&file);

        // Modify again
        fs::write(&file, "v3\n").expect("write");

        let diff = tracker.unified_diff().expect("should have diff");
        // Should show v1 → v3, not v2 → v3
        assert!(diff.contains("-v1"));
        assert!(diff.contains("+v3"));
    }

    #[test]
    fn no_change_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("unchanged.txt");
        fs::write(&file, "same\n").expect("write");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_before_modify(&file);
        // Don't modify the file

        assert!(tracker.unified_diff().is_none());
        assert!(tracker.change_summary().is_empty());
    }

    #[test]
    fn reset_clears_state() {
        let tmp = TempDir::new().expect("tempdir");
        let file = tmp.path().join("f.txt");
        fs::write(&file, "data\n").expect("write");

        let mut tracker = TurnDiffTracker::new();
        tracker.snapshot_before_modify(&file);
        assert_eq!(tracker.file_count(), 1);

        tracker.reset();
        assert_eq!(tracker.file_count(), 0);
    }
}
