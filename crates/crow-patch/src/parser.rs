//! Parser for the Codex `*** Begin Patch` / `*** End Patch` format.
//!
//! Converts a text patch into a sequence of [`PatchHunk`] instructions that
//! the applier can execute against the workspace.

use std::path::PathBuf;

// ─── Types ──────────────────────────────────────────────────────────

/// A single chunk within an `UpdateFile` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateFileChunk {
    /// Optional `@@ <text>` context anchor for fuzzy seeking.
    pub context_anchor: Option<String>,
    /// Lines expected in the original file (context + deletions).
    pub old_lines: Vec<String>,
    /// Lines that replace the old region (context + insertions).
    pub new_lines: Vec<String>,
}

/// A parsed hunk from the patch format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchHunk {
    /// Create a brand-new file with the given contents.
    AddFile { path: PathBuf, contents: String },
    /// Delete an existing file.
    DeleteFile { path: PathBuf },
    /// Apply one or more chunks of changes to an existing file,
    /// optionally moving it to a new path.
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
    },
}

/// Errors that can occur while parsing a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchParseError {
    /// The input does not contain a `*** Begin Patch` line.
    MissingBeginMarker,
    /// The patch body was never closed with `*** End Patch`.
    MissingEndMarker,
    /// A line could not be interpreted in the current parser state.
    UnexpectedLine { line_num: usize, line: String },
    /// The patch contained no hunks at all.
    EmptyPatch,
}

impl std::fmt::Display for PatchParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBeginMarker => write!(f, "missing *** Begin Patch marker"),
            Self::MissingEndMarker => write!(f, "missing *** End Patch marker"),
            Self::UnexpectedLine { line_num, line } => {
                write!(f, "unexpected line {line_num}: {line}")
            }
            Self::EmptyPatch => write!(f, "patch contains no hunks"),
        }
    }
}

impl std::error::Error for PatchParseError {}

// ─── Parser state machine ───────────────────────────────────────────

/// Internal state for the parser.
enum State {
    /// Waiting for a file-level directive.
    AwaitingDirective,
    /// Collecting `+`-prefixed lines for `*** Add File`.
    AddingFile { path: PathBuf, lines: Vec<String> },
    /// Inside an `*** Update File` block, collecting chunks.
    UpdatingFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
        current_chunk: Option<UpdateFileChunk>,
    },
}

/// Parse the Codex `*** Begin Patch` format into a list of [`PatchHunk`]s.
///
/// # Errors
///
/// Returns a [`PatchParseError`] if the input is malformed.
pub fn parse_patch(input: &str) -> Result<Vec<PatchHunk>, PatchParseError> {
    let lines: Vec<&str> = input.lines().collect();

    // Find the begin marker.
    let begin_idx = lines
        .iter()
        .position(|l| l.starts_with("*** Begin Patch"))
        .ok_or(PatchParseError::MissingBeginMarker)?;

    // Find the end marker after begin.
    let end_idx = lines[begin_idx..]
        .iter()
        .position(|l| l.starts_with("*** End Patch"))
        .map(|i| i + begin_idx)
        .ok_or(PatchParseError::MissingEndMarker)?;

    let body = &lines[begin_idx + 1..end_idx];
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut state = State::AwaitingDirective;

    for (rel_idx, &raw_line) in body.iter().enumerate() {
        let line_num = begin_idx + 1 + rel_idx + 1; // 1-based for humans

        match &mut state {
            State::AwaitingDirective => {
                if let Some(path) = raw_line.strip_prefix("*** Add File: ") {
                    state = State::AddingFile {
                        path: PathBuf::from(path.trim()),
                        lines: Vec::new(),
                    };
                } else if let Some(path) = raw_line.strip_prefix("*** Delete File: ") {
                    hunks.push(PatchHunk::DeleteFile {
                        path: PathBuf::from(path.trim()),
                    });
                } else if let Some(path) = raw_line.strip_prefix("*** Update File: ") {
                    state = State::UpdatingFile {
                        path: PathBuf::from(path.trim()),
                        move_path: None,
                        chunks: Vec::new(),
                        current_chunk: None,
                    };
                } else if raw_line.trim().is_empty() {
                    // Skip blank lines between directives.
                } else {
                    return Err(PatchParseError::UnexpectedLine {
                        line_num,
                        line: raw_line.to_string(),
                    });
                }
            }
            State::AddingFile { path, lines } => {
                if raw_line.starts_with("*** ") {
                    // End of add block — flush and re-process this line.
                    let content = lines.join("\n");
                    let content = if content.is_empty() {
                        content
                    } else {
                        format!("{content}\n")
                    };
                    hunks.push(PatchHunk::AddFile {
                        path: path.clone(),
                        contents: content,
                    });
                    // Re-dispatch this line as a directive.
                    return reparse_remaining(
                        &lines_to_string_with_prefix(raw_line, &body[rel_idx + 1..]),
                        hunks,
                        line_num,
                    );
                } else if let Some(stripped) = raw_line.strip_prefix('+') {
                    lines.push(stripped.to_string());
                } else {
                    return Err(PatchParseError::UnexpectedLine {
                        line_num,
                        line: raw_line.to_string(),
                    });
                }
            }
            State::UpdatingFile {
                path,
                move_path,
                chunks,
                current_chunk,
            } => {
                if let Some(mp) = raw_line.strip_prefix("*** Move to: ") {
                    *move_path = Some(PathBuf::from(mp.trim()));
                } else if let Some(anchor_text) = raw_line.strip_prefix("@@ ") {
                    // Flush the previous chunk if any.
                    if let Some(chunk) = current_chunk.take() {
                        chunks.push(chunk);
                    }
                    *current_chunk = Some(UpdateFileChunk {
                        context_anchor: Some(anchor_text.trim().to_string()),
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                    });
                } else if raw_line.starts_with("*** End of File")
                    || raw_line.starts_with("*** Add File: ")
                    || raw_line.starts_with("*** Delete File: ")
                    || raw_line.starts_with("*** Update File: ")
                    || raw_line.starts_with("*** End Patch")
                {
                    // Flush current chunk.
                    if let Some(chunk) = current_chunk.take() {
                        chunks.push(chunk);
                    }
                    hunks.push(PatchHunk::UpdateFile {
                        path: path.clone(),
                        move_path: move_path.clone(),
                        chunks: std::mem::take(chunks),
                    });
                    if raw_line.starts_with("*** End of File")
                        || raw_line.starts_with("*** End Patch")
                    {
                        state = State::AwaitingDirective;
                    } else {
                        // Another file directive — re-dispatch.
                        return reparse_remaining(
                            &lines_to_string_with_prefix(raw_line, &body[rel_idx + 1..]),
                            hunks,
                            line_num,
                        );
                    }
                } else if let Some(content) = raw_line.strip_prefix(' ') {
                    // Context line: belongs to both old and new.
                    let chunk = current_chunk.get_or_insert_with(|| UpdateFileChunk {
                        context_anchor: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                    });
                    chunk.old_lines.push(content.to_string());
                    chunk.new_lines.push(content.to_string());
                } else if let Some(removed) = raw_line.strip_prefix('-') {
                    let chunk = current_chunk.get_or_insert_with(|| UpdateFileChunk {
                        context_anchor: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                    });
                    chunk.old_lines.push(removed.to_string());
                } else if let Some(added) = raw_line.strip_prefix('+') {
                    let chunk = current_chunk.get_or_insert_with(|| UpdateFileChunk {
                        context_anchor: None,
                        old_lines: Vec::new(),
                        new_lines: Vec::new(),
                    });
                    chunk.new_lines.push(added.to_string());
                } else {
                    return Err(PatchParseError::UnexpectedLine {
                        line_num,
                        line: raw_line.to_string(),
                    });
                }
            }
        }
    }

    // Flush trailing state.
    match state {
        State::AwaitingDirective => {}
        State::AddingFile { path, lines } => {
            let content = lines.join("\n");
            let content = if content.is_empty() {
                content
            } else {
                format!("{content}\n")
            };
            hunks.push(PatchHunk::AddFile {
                path,
                contents: content,
            });
        }
        State::UpdatingFile {
            path,
            move_path,
            mut chunks,
            current_chunk,
        } => {
            if let Some(chunk) = current_chunk {
                chunks.push(chunk);
            }
            hunks.push(PatchHunk::UpdateFile {
                path,
                move_path,
                chunks,
            });
        }
    }

    if hunks.is_empty() {
        return Err(PatchParseError::EmptyPatch);
    }

    Ok(hunks)
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Re-enter the parser for the remaining lines when a directive boundary
/// is hit mid-stream. This avoids duplicating the state-machine logic.
fn reparse_remaining(
    remaining: &str,
    mut accumulated: Vec<PatchHunk>,
    _line_offset: usize,
) -> Result<Vec<PatchHunk>, PatchParseError> {
    // Wrap the remaining lines in begin/end markers so `parse_patch` can
    // process them.
    let wrapped = format!("*** Begin Patch\n{remaining}\n*** End Patch");
    let tail = parse_patch(&wrapped)?;
    accumulated.extend(tail);
    Ok(accumulated)
}

/// Join a current line with remaining body lines into a single string.
fn lines_to_string_with_prefix(current: &str, rest: &[&str]) -> String {
    let mut out = String::from(current);
    for line in rest {
        out.push('\n');
        out.push_str(line);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_add_file() {
        let input = "\
*** Begin Patch
*** Add File: src/new.rs
+fn hello() {
+    println!(\"hi\");
+}
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        assert_eq!(hunks.len(), 1);
        match &hunks[0] {
            PatchHunk::AddFile { path, contents } => {
                assert_eq!(path, &PathBuf::from("src/new.rs"));
                assert!(contents.contains("fn hello()"));
            }
            other => panic!("expected AddFile, got {other:?}"),
        }
    }

    #[test]
    fn parse_delete_file() {
        let input = "\
*** Begin Patch
*** Delete File: src/old.rs
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        assert_eq!(hunks.len(), 1);
        assert!(
            matches!(&hunks[0], PatchHunk::DeleteFile { path } if path == &PathBuf::from("src/old.rs"))
        );
    }

    #[test]
    fn parse_update_file_with_context() {
        let input = "\
*** Begin Patch
*** Update File: src/lib.rs
@@ fn main
 fn main() {
-    old_call();
+    new_call();
 }
*** End Patch";
        let hunks = parse_patch(input).unwrap();
        assert_eq!(hunks.len(), 1);
        match &hunks[0] {
            PatchHunk::UpdateFile { path, chunks, .. } => {
                assert_eq!(path, &PathBuf::from("src/lib.rs"));
                assert_eq!(chunks.len(), 1);
                assert_eq!(chunks[0].context_anchor.as_deref(), Some("fn main"));
                assert_eq!(chunks[0].old_lines.len(), 3);
                assert_eq!(chunks[0].new_lines.len(), 3);
            }
            other => panic!("expected UpdateFile, got {other:?}"),
        }
    }

    #[test]
    fn missing_begin_marker() {
        assert_eq!(
            parse_patch("no markers here").unwrap_err(),
            PatchParseError::MissingBeginMarker
        );
    }

    #[test]
    fn missing_end_marker() {
        assert_eq!(
            parse_patch("*** Begin Patch\n*** Add File: x\n+y").unwrap_err(),
            PatchParseError::MissingEndMarker
        );
    }

    #[test]
    fn empty_patch() {
        assert_eq!(
            parse_patch("*** Begin Patch\n*** End Patch").unwrap_err(),
            PatchParseError::EmptyPatch
        );
    }
}
