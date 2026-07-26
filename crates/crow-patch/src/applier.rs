//! Patch applier: converts parsed [`PatchHunk`]s into concrete file changes.
//!
//! The applier never touches the filesystem directly. Instead, it produces a
//! [`PatchAction`] containing a map of [`FileChange`]s that the caller can
//! commit atomically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::parser::{PatchHunk, UpdateFileChunk};
use crate::seek_sequence::{seek_sequence, MatchTier};

// ─── Types ──────────────────────────────────────────────────────────

/// A single computed change to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// Create a new file with this content.
    Add { content: String },
    /// Delete the file.
    Delete,
    /// Rewrite the file with new content.
    /// `exact` is true only when every chunk matched at [`MatchTier::Exact`].
    Update { new_content: String, exact: bool },
}

/// The complete set of file changes produced by applying a patch.
#[derive(Debug, Clone)]
pub struct PatchAction {
    pub changes: HashMap<PathBuf, FileChange>,
}

/// Errors that can occur during patch application.
#[derive(Debug)]
pub enum ApplyError {
    /// A file referenced by an `UpdateFile` or `DeleteFile` hunk was not found.
    FileNotFound(PathBuf),
    /// A chunk's `old_lines` could not be located in the file.
    SequenceNotFound {
        file: PathBuf,
        chunk_index: usize,
        context: Option<String>,
    },
    /// An I/O error occurred while reading a file.
    Io(std::io::Error),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound(path) => write!(f, "file not found: {}", path.display()),
            Self::SequenceNotFound {
                file,
                chunk_index,
                context,
            } => {
                write!(
                    f,
                    "could not find sequence for chunk {chunk_index} in {}",
                    file.display()
                )?;
                if let Some(ctx) = context {
                    write!(f, " (anchor: {ctx})")?;
                }
                Ok(())
            }
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ApplyError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

// ─── Public API ─────────────────────────────────────────────────────

/// Compute the set of file changes described by `hunks` without writing
/// to disk.
///
/// `read_file` is called to retrieve the current content of files that
/// are being updated. All paths are resolved relative to `base_dir`.
///
/// # Errors
///
/// Returns an [`ApplyError`] if a file is missing or a sequence cannot
/// be located.
pub fn compute_patch_changes(
    hunks: &[PatchHunk],
    read_file: impl Fn(&Path) -> std::io::Result<String>,
    base_dir: &Path,
) -> Result<PatchAction, ApplyError> {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

    for hunk in hunks {
        match hunk {
            PatchHunk::AddFile { path, contents } => {
                changes.insert(
                    path.clone(),
                    FileChange::Add {
                        content: contents.clone(),
                    },
                );
            }
            PatchHunk::DeleteFile { path } => {
                changes.insert(path.clone(), FileChange::Delete);
            }
            PatchHunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let abs_path = base_dir.join(path);
                let content = read_file(&abs_path).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        ApplyError::FileNotFound(path.clone())
                    } else {
                        ApplyError::Io(e)
                    }
                })?;

                let (new_content, all_exact) = apply_chunks_to_content(&content, chunks, path)?;

                let target_path = move_path.as_ref().unwrap_or(path);
                changes.insert(
                    target_path.clone(),
                    FileChange::Update {
                        new_content,
                        exact: all_exact,
                    },
                );

                // If the file was moved, the original path should be deleted.
                if move_path.is_some() {
                    changes.insert(path.clone(), FileChange::Delete);
                }
            }
        }
    }

    Ok(PatchAction { changes })
}

// ─── Internal helpers ───────────────────────────────────────────────

/// Apply all `chunks` to `content`, returning the new content and whether
/// every chunk matched exactly.
fn apply_chunks_to_content(
    content: &str,
    chunks: &[UpdateFileChunk],
    file_path: &Path,
) -> Result<(String, bool), ApplyError> {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut all_exact = true;
    let mut cursor: usize = 0;

    for (idx, chunk) in chunks.iter().enumerate() {
        if chunk.old_lines.is_empty() {
            // Pure insertion — insert new lines at the cursor position.
            for (i, new_line) in chunk.new_lines.iter().enumerate() {
                lines.insert(cursor + i, new_line.clone());
            }
            cursor += chunk.new_lines.len();
            continue;
        }

        let seek_result = seek_sequence(&lines, &chunk.old_lines, cursor).ok_or_else(|| {
            ApplyError::SequenceNotFound {
                file: file_path.to_path_buf(),
                chunk_index: idx,
                context: chunk.context_anchor.clone(),
            }
        })?;

        if seek_result.match_tier != MatchTier::Exact {
            all_exact = false;
        }

        let start = seek_result.start_line;
        let end = start + chunk.old_lines.len();

        // Replace old lines with new lines.
        let new_lines = chunk.new_lines.clone();
        lines.splice(start..end, new_lines);

        cursor = start + chunk.new_lines.len();
    }

    // Rejoin with newlines, preserving a trailing newline if the original had one.
    let mut result = lines.join("\n");
    if content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }

    Ok((result, all_exact))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::parser::parse_patch;

    fn mock_fs<'a>(
        files: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&Path) -> std::io::Result<String> + 'a {
        move |path: &Path| {
            for &(name, content) in files {
                if path.ends_with(name) {
                    return Ok(content.to_string());
                }
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("not found: {}", path.display()),
            ))
        }
    }

    #[test]
    fn apply_add_file() {
        let input = "\
*** Begin Patch
*** Add File: src/new.rs
+fn hello() {}
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        let result = compute_patch_changes(&hunks, mock_fs(&[]), Path::new("/project")).unwrap();
        assert!(matches!(
            result.changes.get(Path::new("src/new.rs")),
            Some(FileChange::Add { .. })
        ));
    }

    #[test]
    fn apply_delete_file() {
        let input = "\
*** Begin Patch
*** Delete File: src/old.rs
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        let result = compute_patch_changes(&hunks, mock_fs(&[]), Path::new("/project")).unwrap();
        assert!(matches!(
            result.changes.get(Path::new("src/old.rs")),
            Some(FileChange::Delete)
        ));
    }

    #[test]
    fn apply_update_file() {
        let original = "fn main() {\n    old_call();\n}\n";
        let input = "\
*** Begin Patch
*** Update File: src/main.rs
@@ fn main
 fn main() {
-    old_call();
+    new_call();
 }
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        let result = compute_patch_changes(
            &hunks,
            mock_fs(&[("src/main.rs", original)]),
            Path::new("/project"),
        )
        .unwrap();
        match result.changes.get(Path::new("src/main.rs")) {
            Some(FileChange::Update { new_content, exact }) => {
                assert!(new_content.contains("new_call()"));
                assert!(!new_content.contains("old_call()"));
                assert!(*exact);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn apply_file_not_found() {
        let input = "\
*** Begin Patch
*** Update File: missing.rs
@@ start
 line
-old
+new
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        let result = compute_patch_changes(&hunks, mock_fs(&[]), Path::new("/project"));
        assert!(matches!(result, Err(ApplyError::FileNotFound(_))));
    }
}
